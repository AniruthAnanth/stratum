//! `--deterministic` — CONTRACTS.md §7.2's substitution table, and nothing else.
//!
//! §7.2 is what ARCHITECTURE §8.9 compares across macOS, Windows and Linux, what
//! W08's "two consecutive clean runs are byte-identical" asserts, and what
//! `cargo xtask conformance` drives. It also declares
//!
//! > `stratum run --json | xtask normalize-ndjson` is equivalent to
//! > `--deterministic`
//!
//! so there are two implementations of one table. They drift apart silently
//! unless something ties them together, and the tie is structural rather than a
//! promise: this module normalises a `serde_json::Value` and re-emits it with
//! `serde_json::to_string`, which is byte-for-byte what `xtask
//! normalize-ndjson` does to a line it has parsed. `xtask conformance` then
//! requires every captured stream to be a **fixed point** of the xtask
//! normalizer — whatever we emitted, running the other implementation over it
//! must change nothing — so a divergence in either direction is a red check
//! rather than a silence.
//!
//! # What is deliberately NOT normalised
//!
//! `seq`, `ExecutionId`, `ResultId`, `BlockId`, `DatasetStateId`, `StateId` and
//! `SessionId`. They are deterministic already, and normalising them would hide
//! **id-allocation drift** — the exact class of bug this comparison exists to
//! catch. `tests::the_ids_section_7_2_names_are_left_alone` is that rule as an
//! assertion, over a stream that carries all six.
//!
//! `SessionId` deserves its own sentence because it appears in neither §7.2
//! list: only the session *segment inside an asset path* is rewritten, so a
//! `RunStarted.session` is emitted verbatim. That is only consistent if `run`
//! allocates a deterministic session id — which [`crate::cmd::run`] does, from a
//! constant.

use serde_json::{Map, Value};

/// Everything one stream needs to normalise itself.
///
/// `base` is fixed by the first `RunStarted.source` in the stream, exactly as
/// §7.2 says ("relative to the entry file's parent"), or supplied up front by a
/// caller that knows the entry file and may emit diagnostics before the run
/// starts.
#[derive(Clone, Debug, Default)]
pub struct Normalizer {
    base: Option<String>,
}

impl Normalizer {
    /// A normalizer that learns its base directory from the stream.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A normalizer whose base directory is already known — the parent of the
    /// entry `.do`.
    #[must_use]
    pub fn with_base(base: impl Into<String>) -> Self {
        Self {
            base: Some(base.into()),
        }
    }

    /// Normalise one already-serialised envelope in place.
    ///
    /// Takes a `Value` rather than a typed envelope on purpose: §7.2 is stated
    /// over field *names* across every payload in the protocol, including ones
    /// this crate never names (`DepKey::File`, `Finding`, `SiteRef`). A typed
    /// walk would have to be extended every time proto grows a path-valued
    /// field, and the version that forgot would be silently wrong.
    pub fn normalize(&mut self, value: &mut Value) {
        if self.base.is_none() {
            if let Some(src) = run_started_source(value) {
                self.base = parent_of(&src);
            }
        }
        normalize_value(value, self.base.as_deref());
    }

    /// The base directory in force, once it is known.
    ///
    /// `#[cfg(test)]`: §7.2's base is an input to normalisation and never an
    /// output, so the shipped path has no reason to read it back. The tests do,
    /// because "which directory did the stream anchor itself to" is exactly what
    /// distinguishes a correct relativisation from a lucky one.
    #[cfg(test)]
    #[must_use]
    pub fn base(&self) -> Option<&str> {
        self.base.as_deref()
    }
}

/// `RunStarted.source`, if this envelope is one.
fn run_started_source(value: &Value) -> Option<String> {
    let body = value.get("body").unwrap_or(value);
    if body.get("event")? != "run_started" {
        return None;
    }
    body.get("source")?.as_str().map(str::to_owned)
}

fn normalize_value(value: &mut Value, base: Option<&str>) {
    match value {
        Value::Array(items) => {
            for item in items {
                normalize_value(item, base);
            }
        }
        Value::Object(map) => normalize_object(map, base),
        _ => {}
    }
}

fn normalize_object(map: &mut Map<String, Value>, base: Option<&str>) {
    // `AssetRef { path, mime, bytes }` is the one object whose `path` is NOT a
    // filesystem path (CONTRACTS §9: "Never a filesystem path"), so it is
    // recognised structurally and handled before the by-name rules below.
    if is_asset_ref(map) {
        if let Some(Value::String(p)) = map.get_mut("path") {
            *p = asset_session_to_s0(p);
        }
        return;
    }

    for (key, val) in map.iter_mut() {
        match (key.as_str(), &mut *val) {
            // "every `*_at_ms`". The `is_f64` guard keeps the rule off a field
            // that happens to end in those bytes but carries a float: a UnixMs
            // is a u64 on the wire (A2) and nothing else should match.
            (k, Value::Number(n)) if k.ends_with("_at_ms") && !n.is_f64() => {
                *val = Value::from(0_u64);
            }
            // "every `duration_us`" — including `verified_duration_us`, which is
            // why this is a suffix test and not equality.
            (k, Value::Number(_)) if k.ends_with("duration_us") => {
                *val = Value::from(0_u64);
            }
            ("stratum_version", Value::String(_)) => *val = Value::from("<version>"),
            ("cwd", Value::String(_)) => *val = Value::from("<cwd>"),
            // Every path-valued field §7.2 names uses one of these four names in
            // CONTRACTS: `source` (RunStarted), `file` (Diagnostic, Related),
            // `path` (DepKey::File, StaleReason::FileChanged,
            // BrokenReason::MissingFile, Provenance) and `files` (DefUseIndex,
            // which `Finding`/`SiteRef` index into — `SiteRef::file` is a u32
            // index, and the `Value::String` guard is what keeps this rule off
            // it).
            ("source" | "file" | "path", Value::String(s)) => {
                *val = Value::from(relativize(s, base));
            }
            ("files", Value::Array(items)) => {
                for item in items.iter_mut() {
                    if let Value::String(s) = item {
                        *item = Value::from(relativize(s, base));
                    }
                }
            }
            _ => normalize_value(val, base),
        }
    }
}

/// Recognised by shape rather than by a field count, so adding a field to
/// `AssetRef` in proto does not silently turn its `path` into a filesystem path.
fn is_asset_ref(map: &Map<String, Value>) -> bool {
    map.get("path").is_some_and(Value::is_string)
        && map.get("mime").is_some_and(Value::is_string)
        && map.get("bytes").is_some_and(Value::is_number)
}

/// `{kind}/{session}/…` — CONTRACTS §10.1 pins the shape, so replacing segment 1
/// is exact rather than a guess. A path without that shape is left alone:
/// inventing a substitution would corrupt an asset URL we do not own.
fn asset_session_to_s0(path: &str) -> String {
    let mut parts: Vec<&str> = path.split('/').collect();
    let kind_at = usize::from(parts.first() == Some(&""));
    if parts.len() > kind_at + 1 && matches!(parts[kind_at], "result" | "graph" | "frame") {
        parts[kind_at + 1] = "S0";
    }
    parts.join("/")
}

/// Make `p` relative to `base`, `/`-separated, or `"<abs>"` if it escapes.
///
/// Purely lexical: it never touches the filesystem. That is the point — macOS,
/// Windows and Linux must produce the same answer for a path that may not exist
/// on the machine doing the comparison.
#[must_use]
pub fn relativize(p: &str, base: Option<&str>) -> String {
    let path = to_slashes(p);
    if !is_absolute(&path) {
        return path;
    }
    let Some(base) = base else {
        return "<abs>".to_owned();
    };
    let base = trim_trailing_slash(&to_slashes(base));

    let (hay, needle) = if cfg!(windows) {
        (path.to_lowercase(), base.to_lowercase())
    } else {
        (path.clone(), base.clone())
    };
    if hay == needle {
        return ".".to_owned();
    }
    match hay.strip_prefix(&needle) {
        Some(rest) if rest.starts_with('/') => path[base.len() + 1..].to_owned(),
        _ => "<abs>".to_owned(),
    }
}

fn parent_of(p: &str) -> Option<String> {
    let p = to_slashes(p);
    let idx = p.rfind('/')?;
    Some(if idx == 0 {
        "/".to_owned()
    } else {
        p[..idx].to_owned()
    })
}

fn to_slashes(p: &str) -> String {
    p.replace('\\', "/")
}

fn trim_trailing_slash(p: &str) -> String {
    if p.len() > 1 && p.ends_with('/') {
        p[..p.len() - 1].to_owned()
    } else {
        p.to_owned()
    }
}

/// True for `/x` and for a Windows path such as `C:/x`. Recognising Windows
/// shapes on every host is deliberate: CI normalises the Windows runner's output
/// on Linux before diffing it.
fn is_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    if b.first() == Some(&b'/') {
        return true;
    }
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'/'
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `RunStarted` with every §7.2-relevant field populated.
    const RUN_STARTED: &str = r#"{"v":1,"t":"event","body":{"event":"run_started","seq":0,"schema":1,"run":7,"session":3,"stratum_version":"0.1.0+ci","source":"/home/u/proj/analysis.do","clean_state":true,"cwd":"/home/u/proj","started_at_ms":1755820000123,"seed":null,"plan_len":4}}"#;

    fn norm(lines: &[&str]) -> Vec<Value> {
        let mut n = Normalizer::new();
        lines
            .iter()
            .map(|l| {
                let mut v: Value = serde_json::from_str(l).expect("test input is JSON");
                n.normalize(&mut v);
                v
            })
            .collect()
    }

    #[test]
    fn run_started_is_normalized_and_fixes_the_base_directory() {
        let v = &norm(&[RUN_STARTED])[0];
        let b = &v["body"];
        assert_eq!(b["started_at_ms"], 0);
        assert_eq!(b["stratum_version"], "<version>");
        assert_eq!(b["cwd"], "<cwd>");
        assert_eq!(b["source"], "analysis.do");
    }

    /// **The rule §7.2 states twice, because it is the point of the exercise.**
    /// `seq` and the five id families are deterministic already; normalising
    /// them would hide id-allocation drift.
    #[test]
    fn the_ids_section_7_2_names_are_left_alone() {
        let block_finished = r#"{"v":1,"t":"event","body":{"event":"block_finished","seq":9,"run":7,"exec":3,"block":2,"result":11,"status":{"status":"succeeded"},"rc":0,"duration_us":41233,"dataset_state_out":4}}"#;
        let state_changed = r#"{"v":1,"t":"event","body":{"event":"state_changed","seq":10,"exec":3,"dataset_state":4,"state":19,"frame":"default","n_obs":74,"n_vars":12,"events":[]}}"#;
        let out = norm(&[RUN_STARTED, block_finished, state_changed]);

        assert_eq!(out[1]["body"]["duration_us"], 0, "durations DO go to zero");
        for (line, field, want) in [
            (0, "seq", 0),
            (0, "run", 7),
            (0, "session", 3),
            (0, "plan_len", 4),
            (1, "seq", 9),
            (1, "exec", 3),
            (1, "block", 2),
            (1, "result", 11),
            (1, "dataset_state_out", 4),
            (1, "rc", 0),
            (2, "dataset_state", 4),
            (2, "state", 19),
        ] {
            assert_eq!(
                out[line]["body"][field], want,
                "{field} must survive verbatim — normalising it hides id drift"
            );
        }
    }

    #[test]
    fn timestamps_and_durations_go_to_zero_at_every_depth() {
        let nested = r#"{"v":1,"t":"resp","corr":7,"body":{"generated_at_ms":1755820001000,"verified_duration_us":12,"nested":[{"finished_at_ms":9}]}}"#;
        let out = norm(&[RUN_STARTED, nested]);
        assert_eq!(out[1]["body"]["generated_at_ms"], 0);
        assert_eq!(out[1]["body"]["verified_duration_us"], 0);
        assert_eq!(out[1]["body"]["nested"][0]["finished_at_ms"], 0);
        assert_eq!(out[1]["corr"], 7, "the envelope is not a payload");
    }

    #[test]
    fn diagnostic_and_dep_paths_are_relativized_and_escapes_become_abs() {
        let diag = r#"{"v":1,"t":"event","body":{"event":"diagnostic","seq":3,"exec":1,"diagnostic":{"severity":"error","code":"STATA0111","message":"variable not found","file":"/home/u/proj/sub/clean.do","related":[{"span":{"start":0,"end":4},"file":"/elsewhere/other.do","message":"first used here"}],"notes":[]}}}"#;
        let d = &norm(&[RUN_STARTED, diag])[1]["body"]["diagnostic"];
        assert_eq!(d["file"], "sub/clean.do");
        assert_eq!(d["related"][0]["file"], "<abs>");
        assert_eq!(d["code"], "STATA0111", "the code is not a path");
    }

    #[test]
    fn asset_refs_lose_the_session_and_keep_everything_else() {
        let result = r#"{"v":1,"t":"event","body":{"event":"result","seq":6,"exec":1,"envelope":{"raw":{"bytes":9001,"lines":42,"head":"x","truncated":true,"asset":{"path":"result/7f3a9c/12/raw","mime":"text/plain","bytes":9001}},"graph":{"path":"graph/7f3a9c/13.svg","mime":"image/svg+xml","bytes":1024}}}}"#;
        let e = &norm(&[RUN_STARTED, result])[1]["body"]["envelope"];
        assert_eq!(e["raw"]["asset"]["path"], "result/S0/12/raw");
        assert_eq!(e["graph"]["path"], "graph/S0/13.svg");
        assert_eq!(
            e["raw"]["bytes"], 9001,
            "RawRef.bytes is not an AssetRef field"
        );
    }

    #[test]
    fn a_defuse_file_table_is_relativized_but_its_indices_are_not() {
        let defuse = r#"{"v":1,"t":"resp","corr":2,"body":{"generation":3,"files":["/home/u/proj/analysis.do","/home/u/proj/lib/util.do","/opt/ado/x.ado"],"defs":[["income",[{"file":0,"line":12,"col":1,"span":{"start":100,"end":120},"block":1,"statement":"gen income = .","kind":"def","confidence":"exact"}]]],"uses":[],"unresolved":[]}}"#;
        let b = &norm(&[RUN_STARTED, defuse])[1]["body"];
        assert_eq!(b["files"][0], "analysis.do");
        assert_eq!(b["files"][1], "lib/util.do");
        assert_eq!(b["files"][2], "<abs>");
        assert_eq!(b["defs"][0][1][0]["file"], 0, "SiteRef::file is an index");
    }

    /// The invariant the whole flag exists for.
    #[test]
    fn two_platforms_agree_after_normalization() {
        let unix = norm(&[
            RUN_STARTED,
            r#"{"v":1,"t":"event","body":{"event":"block_finished","seq":9,"run":7,"exec":3,"block":2,"result":null,"status":{"status":"succeeded"},"rc":0,"duration_us":41233,"dataset_state_out":4}}"#,
        ]);
        let windows = norm(&[
            r#"{"v":1,"t":"event","body":{"event":"run_started","seq":0,"schema":1,"run":7,"session":3,"stratum_version":"0.1.0+win","source":"C:\\Users\\u\\proj\\analysis.do","clean_state":true,"cwd":"C:\\Users\\u\\proj","started_at_ms":1755999999999,"seed":null,"plan_len":4}}"#,
            r#"{"v":1,"t":"event","body":{"event":"block_finished","seq":9,"run":7,"exec":3,"block":2,"result":null,"status":{"status":"succeeded"},"rc":0,"duration_us":98,"dataset_state_out":4}}"#,
        ]);
        assert_eq!(unix, windows);
    }

    /// A normalizer that is not idempotent cannot be a fixed point of `xtask
    /// normalize-ndjson`, which is how the two implementations of §7.2 are tied
    /// together.
    #[test]
    fn normalization_is_idempotent() {
        let once = norm(&[RUN_STARTED]);
        let mut twice = once.clone();
        let mut n = Normalizer::new();
        for v in &mut twice {
            n.normalize(v);
        }
        assert_eq!(once, twice);
    }

    #[test]
    fn relativize_edge_cases() {
        assert_eq!(relativize("/a/b/c.do", Some("/a/b")), "c.do");
        assert_eq!(relativize("/a/b/d/c.do", Some("/a/b")), "d/c.do");
        assert_eq!(
            relativize("/a/bb/c.do", Some("/a/b")),
            "<abs>",
            "a prefix must end at a separator"
        );
        assert_eq!(relativize("/a/b", Some("/a/b")), ".");
        assert_eq!(relativize("/a/b/c.do", None), "<abs>");
        assert_eq!(relativize("rel/c.do", Some("/a/b")), "rel/c.do");
        assert_eq!(relativize("rel\\c.do", None), "rel/c.do");
        assert_eq!(relativize("C:\\p\\c.do", Some("C:\\p")), "c.do");
    }

    #[test]
    fn an_asset_path_that_is_not_ours_is_left_alone() {
        assert_eq!(asset_session_to_s0("app/index.html"), "app/index.html");
        assert_eq!(asset_session_to_s0("result"), "result");
        assert_eq!(
            asset_session_to_s0("/frame/abc/f1/page"),
            "/frame/S0/f1/page"
        );
    }

    #[test]
    fn an_explicit_base_beats_the_stream() {
        let mut n = Normalizer::with_base("/tmp/elsewhere");
        let mut v: Value = serde_json::from_str(RUN_STARTED).unwrap();
        n.normalize(&mut v);
        assert_eq!(v["body"]["source"], "<abs>");
        assert_eq!(n.base(), Some("/tmp/elsewhere"));
    }
}
