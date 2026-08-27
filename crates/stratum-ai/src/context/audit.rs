//! `07` §4.5 — the local audit log: what was sent, where it went, and when.
//!
//! # Why this exists at all
//!
//! "Preview what will be sent" answers the question before the fact. This
//! answers it after, and it is the one the user actually asks — a month later,
//! to an IRB, about a dataset they no longer have open. The post-hoc "what was
//! sent" viewer is served **from this file**, not reconstructed from the current
//! session, so it cannot drift from what was sent.
//!
//! # Local, and only local
//!
//! Never uploaded. Never attached to a crash report. Created 0600. There is no
//! telemetry of any kind on AI content (D-AI-12) — in this product category the
//! toggle existing at all is a procurement blocker, so the toggle does not
//! exist.
//!
//! # No `time` dependency
//!
//! A2 makes every timestamp a `u64` of Unix milliseconds, and `deny.toml` lists
//! this crate nowhere in `time`'s `wrappers`. The day a record files under is
//! computed here with Howard Hinnant's `civil_from_days`, which is thirty lines
//! of integer arithmetic, is exact for every date this product will see, and
//! introduces no locale or timezone surface. Rendering a timestamp for a human
//! is the desktop's job; filing one under a UTC day is ours.

use std::io::{BufRead as _, Write as _};

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

use super::policy::{TierBound, TierInputs};
use super::tiers::PrivacyTier;
use crate::provider::redact::scrub;
use crate::provider::types::{ModelId, ProviderId, TokenUsage};
use crate::service::surface::Surface;

/// The directory, relative to the platform data dir, holding the log.
pub const AUDIT_DIR: &str = "ai-audit";

/// Default retention. `0` means "keep forever", which is a legitimate choice for
/// a lab that must be able to answer a question about a run from last year.
pub const DEFAULT_RETENTION_DAYS: u32 = 30;

/// How a request ended.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// The provider answered.
    Ok,
    /// The user, a supersession, or an invalidated precondition stopped it.
    Cancelled,
    /// It failed. The string has already been through
    /// [`crate::provider::redact::scrub`].
    Error {
        /// What went wrong, in the user's words.
        detail: String,
    },
}

/// One request, verbatim.
///
/// `prompt_bytes` is the exact transcript. That is the point: an audit record
/// that summarised would be a second rendering, and a second rendering can be
/// wrong. It is written through [`scrub`] anyway, because a prompt that somehow
/// contained key material must not become a permanent copy of it.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct SentRecord {
    /// Stable within a machine, derived rather than random: blake3 over
    /// `(surface, at_unix_ms, seq)`. `uuid` is not in the workspace dependency
    /// table and a 128-bit random id buys nothing here — nothing correlates
    /// these across machines, by design.
    pub id: String,
    /// Unix milliseconds, UTC (A2).
    pub at_unix_ms: u64,
    /// Which surface asked.
    pub surface: Surface,
    /// Which backend.
    pub provider: ProviderId,
    /// The host the bytes actually went to. This is the field that proves it.
    pub endpoint_host: String,
    /// Which model.
    pub model: ModelId,
    /// The tier the gate ran at.
    pub effective_tier: PrivacyTier,
    /// All four tier inputs.
    pub tier_inputs: TierInputs,
    /// Which of the four bound the result.
    pub bound_by: TierBound,
    /// The exact prompt.
    pub prompt_bytes: String,
    /// The exact response, when there was one.
    pub response_bytes: Option<String>,
    /// Final accounting.
    pub usage: TokenUsage,
    /// Estimated, from the shipped price table. Never fetched (`07` §11.1): a
    /// live pricing endpoint would be a network call made before the user has
    /// consented to any network activity.
    pub est_cost_usd: f64,
    /// Wall clock, recorded not asserted (ADR-017).
    pub latency_ms: u64,
    /// How it ended.
    pub outcome: Outcome,
    /// How many variables were pseudonymised.
    pub pseudonym_map_size: usize,
}

impl SentRecord {
    /// Derive the stable record id.
    #[must_use]
    pub fn derive_id(surface: Surface, at_unix_ms: u64, seq: u64) -> String {
        let mut h = blake3::Hasher::new();
        h.update(surface.as_str().as_bytes());
        h.update(&at_unix_ms.to_le_bytes());
        h.update(&seq.to_le_bytes());
        h.finalize().to_hex()[..32].to_owned()
    }
}

/// An inclusive-start, exclusive-end window in Unix milliseconds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TimeRange {
    /// Inclusive.
    pub from_unix_ms: u64,
    /// Exclusive.
    pub to_unix_ms: u64,
}

impl TimeRange {
    /// Everything.
    #[must_use]
    pub const fn all() -> Self {
        Self {
            from_unix_ms: 0,
            to_unix_ms: u64::MAX,
        }
    }

    /// Whether a record falls inside.
    #[must_use]
    pub const fn contains(self, at_unix_ms: u64) -> bool {
        at_unix_ms >= self.from_unix_ms && at_unix_ms < self.to_unix_ms
    }
}

/// The append-only JSONL log under `<data_dir>/ai-audit/`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AuditLog {
    dir: Utf8PathBuf,
    retention_days: u32,
}

/// The log could not be written. Never fatal to a request: an AI answer the user
/// can read but that we failed to record is better than no answer, and the
/// failure is surfaced rather than swallowed.
#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    /// The filesystem said no.
    #[error("ai audit log: {0}")]
    Io(String),
}

impl AuditLog {
    /// Open (creating) the log directory under a platform data dir.
    ///
    /// # Errors
    /// [`AuditError::Io`] when the directory cannot be created.
    pub fn open(data_dir: &Utf8Path) -> Result<Self, AuditError> {
        let dir = data_dir.join(AUDIT_DIR);
        std::fs::create_dir_all(&dir).map_err(|e| AuditError::Io(format!("{dir}: {e}")))?;
        restrict(dir.as_std_path())?;
        Ok(Self {
            dir,
            retention_days: DEFAULT_RETENTION_DAYS,
        })
    }

    /// Override the retention window. `0` keeps everything forever.
    #[must_use]
    pub const fn with_retention_days(mut self, days: u32) -> Self {
        self.retention_days = days;
        self
    }

    /// The directory the log lives in.
    #[must_use]
    pub fn dir(&self) -> &Utf8Path {
        &self.dir
    }

    /// The file a timestamp files under: `YYYY-MM-DD.jsonl`, UTC.
    #[must_use]
    pub fn file_for(&self, at_unix_ms: u64) -> Utf8PathBuf {
        self.dir.join(format!("{}.jsonl", utc_day(at_unix_ms)))
    }

    /// Append one record.
    ///
    /// # Errors
    /// [`AuditError::Io`].
    pub fn append(&self, record: &SentRecord) -> Result<(), AuditError> {
        let path = self.file_for(record.at_unix_ms);
        // Defence in depth. The prompt is gated bytes and should never contain
        // key material; if a provider echoed one back into an error detail, this
        // is the last place it could become a permanent copy.
        let mut safe = record.clone();
        safe.prompt_bytes = scrub(&safe.prompt_bytes);
        safe.response_bytes = safe.response_bytes.map(|s| scrub(&s));
        if let Outcome::Error { detail } = &safe.outcome {
            safe.outcome = Outcome::Error {
                detail: scrub(detail),
            };
        }
        let line = serde_json::to_string(&safe)
            .map_err(|e| AuditError::Io(format!("serialising a record: {e}")))?;

        let mut file = open_append(&path)?;
        file.write_all(line.as_bytes())
            .map_err(|e| AuditError::Io(format!("{path}: {e}")))?;
        file.write_all(b"\n")
            .map_err(|e| AuditError::Io(format!("{path}: {e}")))?;
        Ok(())
    }

    /// Every record in a window, oldest first.
    ///
    /// A record that will not parse is skipped rather than failing the read: a
    /// truncated last line from a power loss must not make a year of history
    /// unreadable.
    #[must_use]
    pub fn range(&self, range: TimeRange) -> Vec<SentRecord> {
        let mut files: Vec<Utf8PathBuf> = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd
                .filter_map(Result::ok)
                .filter_map(|e| Utf8PathBuf::from_path_buf(e.path()).ok())
                .filter(|p| p.extension() == Some("jsonl"))
                .collect(),
            Err(_) => return Vec::new(),
        };
        files.sort();

        let mut out = Vec::new();
        for path in files {
            let Ok(file) = std::fs::File::open(&path) else {
                continue;
            };
            for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
                if let Ok(r) = serde_json::from_str::<SentRecord>(&line) {
                    if range.contains(r.at_unix_ms) {
                        out.push(r);
                    }
                }
            }
        }
        out.sort_by_key(|r| r.at_unix_ms);
        out
    }

    /// Delete day files older than the retention window.
    ///
    /// `now_unix_ms` is passed rather than read from the clock so this is
    /// testable without waiting thirty days.
    ///
    /// # Errors
    /// [`AuditError::Io`] when a file exists and cannot be removed.
    pub fn purge_expired(&self, now_unix_ms: u64) -> Result<usize, AuditError> {
        if self.retention_days == 0 {
            return Ok(0);
        }
        let cutoff_day = day_number(now_unix_ms).saturating_sub(i64::from(self.retention_days));
        let mut removed = 0;
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return Ok(0);
        };
        for entry in rd.filter_map(Result::ok) {
            let Ok(path) = Utf8PathBuf::from_path_buf(entry.path()) else {
                continue;
            };
            if path.extension() != Some("jsonl") {
                continue;
            }
            let Some(stem) = path.file_stem() else {
                continue;
            };
            let Some(day) = day_number_of_iso(stem) else {
                continue;
            };
            if day < cutoff_day {
                std::fs::remove_file(&path).map_err(|e| AuditError::Io(format!("{path}: {e}")))?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// "Delete all AI history" — every day file, unconditionally.
    ///
    /// # Errors
    /// [`AuditError::Io`].
    pub fn delete_all(&self) -> Result<usize, AuditError> {
        let mut removed = 0;
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return Ok(0);
        };
        for entry in rd.filter_map(Result::ok) {
            let Ok(path) = Utf8PathBuf::from_path_buf(entry.path()) else {
                continue;
            };
            if path.extension() == Some("jsonl") {
                std::fs::remove_file(&path).map_err(|e| AuditError::Io(format!("{path}: {e}")))?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn open_append(path: &Utf8Path) -> Result<std::fs::File, AuditError> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        // 0600 at creation, not chmod-after: a log of everything a researcher
        // has asked about restricted data must never be world-readable, not even
        // for the microseconds between `create` and `set_permissions`.
        .mode(0o600)
        .open(path)
        .map_err(|e| AuditError::Io(format!("{path}: {e}")))
}

#[cfg(not(unix))]
fn open_append(path: &Utf8Path) -> Result<std::fs::File, AuditError> {
    // Windows inherits the parent directory's ACL, and the platform data dir is
    // already per-user (`%LOCALAPPDATA%`). There is no mode to set.
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| AuditError::Io(format!("{path}: {e}")))
}

#[cfg(unix)]
fn restrict(dir: &std::path::Path) -> Result<(), AuditError> {
    use std::os::unix::fs::PermissionsExt as _;
    let perms = std::fs::Permissions::from_mode(0o700);
    std::fs::set_permissions(dir, perms)
        .map_err(|e| AuditError::Io(format!("{}: {e}", dir.display())))
}

#[cfg(not(unix))]
fn restrict(_dir: &std::path::Path) -> Result<(), AuditError> {
    Ok(())
}

// ---------------------------------------------------------------------------
// Dates, without a date library
// ---------------------------------------------------------------------------

/// Days since the Unix epoch, UTC.
#[must_use]
pub fn day_number(at_unix_ms: u64) -> i64 {
    (at_unix_ms / 86_400_000) as i64
}

/// `YYYY-MM-DD`, UTC.
///
/// Howard Hinnant's `civil_from_days`, shifted to an era beginning 0000-03-01 so
/// that the leap day is the last day of the era and the month arithmetic has no
/// special cases.
#[must_use]
pub fn utc_day(at_unix_ms: u64) -> String {
    let (y, m, d) = civil_from_days(day_number(at_unix_ms));
    format!("{y:04}-{m:02}-{d:02}")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + u64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

fn day_number_of_iso(stem: &str) -> Option<i64> {
    let mut parts = stem.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400_000;

    fn record(at: u64) -> SentRecord {
        SentRecord {
            id: SentRecord::derive_id(Surface::Chat, at, 0),
            at_unix_ms: at,
            surface: Surface::Chat,
            provider: ProviderId::Anthropic,
            endpoint_host: "api.anthropic.com".to_owned(),
            model: ModelId::from("claude-opus-5"),
            effective_tier: PrivacyTier::SchemaOnly,
            tier_inputs: TierInputs::default(),
            bound_by: TierBound::Global,
            prompt_bytes: "## VARIABLES\nprice int \"Price\"\n".to_owned(),
            response_bytes: Some("It is a price.".to_owned()),
            usage: TokenUsage::default(),
            est_cost_usd: 0.0,
            latency_ms: 12,
            outcome: Outcome::Ok,
            pseudonym_map_size: 0,
        }
    }

    #[test]
    fn dates_round_trip_across_leap_years_and_century_boundaries() {
        // The whole point of transcribing the algorithm rather than reaching for
        // `time`: it has to be right, and being right is checkable.
        for (ms, expect) in [
            (0u64, "1970-01-01"),
            (DAY * 59, "1970-03-01"),
            (1_582_934_400_000, "2020-02-29"), // a leap day
            (951_782_400_000, "2000-02-29"),   // a century that IS a leap year
            (1_756_857_600_000, "2025-09-03"),
        ] {
            assert_eq!(utc_day(ms), expect, "{ms}");
        }
    }

    #[test]
    fn every_iso_stem_round_trips_through_the_day_number() {
        for day in [0i64, 1, 59, 18_321, 20_000, 25_000] {
            let (y, m, d) = civil_from_days(day);
            let stem = format!("{y:04}-{m:02}-{d:02}");
            assert_eq!(day_number_of_iso(&stem), Some(day), "{stem}");
        }
    }

    #[test]
    fn a_record_round_trips_and_files_under_its_utc_day() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let log = AuditLog::open(dir).unwrap();
        let r = record(1_756_857_600_000);
        log.append(&r).unwrap();
        assert!(log
            .file_for(r.at_unix_ms)
            .as_str()
            .ends_with("2025-09-03.jsonl"));
        let got = log.range(TimeRange::all());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].prompt_bytes, r.prompt_bytes);
        assert_eq!(got[0].id, r.id);
    }

    #[test]
    fn a_registered_secret_never_reaches_the_file() {
        // The last line of defence. A provider that echoed a key into an error
        // body must not turn the audit log into a permanent copy of it.
        crate::provider::redact::forget_all();
        let key = secrecy::SecretString::from("sk-ant-ZZTESTKEY0123456789".to_owned());
        crate::provider::redact::register(&key);

        let tmp = tempfile::tempdir().unwrap();
        let dir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let log = AuditLog::open(dir).unwrap();
        let mut r = record(0);
        r.response_bytes = Some("your key sk-ant-ZZTESTKEY0123456789 was rejected".to_owned());
        r.outcome = Outcome::Error {
            detail: "sk-ant-ZZTESTKEY0123456789".to_owned(),
        };
        log.append(&r).unwrap();

        let bytes = std::fs::read_to_string(log.file_for(0).as_std_path()).unwrap();
        assert!(!bytes.contains("ZZTESTKEY"), "{bytes}");
        crate::provider::redact::forget_all();
    }

    #[test]
    fn retention_removes_old_days_and_keeps_the_window() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let log = AuditLog::open(dir).unwrap().with_retention_days(30);
        let now = DAY * 20_000;
        for back in [0u64, 10, 29, 31, 400] {
            log.append(&record(now - DAY * back)).unwrap();
        }
        let removed = log.purge_expired(now).unwrap();
        assert_eq!(
            removed, 2,
            "31 and 400 days back are outside a 30-day window"
        );
        assert_eq!(log.range(TimeRange::all()).len(), 3);
    }

    #[test]
    fn zero_retention_keeps_everything_forever() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let log = AuditLog::open(dir).unwrap().with_retention_days(0);
        log.append(&record(0)).unwrap();
        assert_eq!(log.purge_expired(DAY * 40_000).unwrap(), 0);
        assert_eq!(log.range(TimeRange::all()).len(), 1);
    }

    #[test]
    fn delete_all_purges_every_day_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let log = AuditLog::open(dir).unwrap();
        log.append(&record(0)).unwrap();
        log.append(&record(DAY * 5)).unwrap();
        assert_eq!(log.delete_all().unwrap(), 2);
        assert!(log.range(TimeRange::all()).is_empty());
    }

    #[test]
    fn a_truncated_last_line_does_not_make_the_history_unreadable() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let log = AuditLog::open(dir).unwrap();
        log.append(&record(0)).unwrap();
        let path = log.file_for(0);
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(path.as_std_path())
            .unwrap();
        f.write_all(b"{\"id\":\"trunc").unwrap();
        assert_eq!(log.range(TimeRange::all()).len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn the_log_file_is_created_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let dir = camino::Utf8Path::from_path(tmp.path()).unwrap();
        let log = AuditLog::open(dir).unwrap();
        log.append(&record(0)).unwrap();
        let meta = std::fs::metadata(log.file_for(0).as_std_path()).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }
}
