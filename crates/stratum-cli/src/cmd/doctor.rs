//! `stratum doctor` — environment diagnostics; **exit 0 iff healthy**.
//!
//! Design 08 §4.2: "`doctor` prints the resolved value and its source for every
//! setting." The precedence it reports against is CLI flag > `STRATUM_*` env var
//! > project `stratum.toml` (nearest ancestor) > user config > built-in default.
//!
//! Every path here comes from `stratum_platform::Paths`, which is a struct
//! rather than a trait precisely because path resolution is "pure computation
//! over environment variables" (design 08 §5.2) — so `doctor` is testable and
//! `PlatformError::Unsupported` never reaches the hot path.

use std::io::Write;

use camino::Utf8PathBuf;
use serde::Serialize;

use crate::cli::{DoctorArgs, ExitCode, Format};
use crate::cmd::CmdError;

/// One resolved setting and where it came from.
#[derive(Clone, Debug, Serialize)]
pub struct Setting {
    /// Dotted name, as `stratum.toml` spells it.
    pub key: &'static str,
    /// The resolved value, rendered.
    pub value: String,
    /// Which layer of design 08 §4.2's precedence won.
    pub source: &'static str,
}

/// What `doctor` found.
#[derive(Clone, Debug, Serialize)]
pub struct Report {
    /// Resolved settings, in a stable order.
    pub settings: Vec<Setting>,
    /// Anything that would stop the product working.
    pub problems: Vec<String>,
}

impl Report {
    /// Healthy means: no problems. The exit code is exactly this predicate.
    #[must_use]
    pub fn healthy(&self) -> bool {
        self.problems.is_empty()
    }
}

/// Gather the report.
#[must_use]
pub fn gather() -> Report {
    let mut settings = Vec::new();
    let mut problems = Vec::new();

    match stratum_platform::Paths::discover() {
        Ok(p) => {
            for (key, value) in [
                ("paths.config", p.config_dir().to_string()),
                ("paths.data", p.data_dir().to_string()),
                ("paths.cache", p.cache_dir().to_string()),
                ("paths.state", p.state_dir().to_string()),
                ("paths.log", p.log_dir().to_string()),
            ] {
                settings.push(Setting {
                    key,
                    value,
                    source: "platform",
                });
            }
        }
        Err(e) => problems.push(format!("cannot resolve the platform directories: {e}")),
    }

    settings.push(Setting {
        key: "engine.linked",
        // The blocker, reported rather than hidden. `stratum run` exits 10
        // while this says `false`.
        value: crate::cmd::ENGINE_LINKED.to_string(),
        source: "build",
    });
    if !crate::cmd::ENGINE_LINKED {
        problems.push(crate::cmd::ENGINE_ABSENT.to_owned());
    }

    settings.push(Setting {
        key: "run.linesize",
        // ADR-016: 80 in v1, and anything else fails with rc 10 rather than
        // being silently ignored.
        value: "80".to_owned(),
        source: "built-in default",
    });
    settings.push(Setting {
        key: "engine.stream_schema",
        value: stratum_proto::engine::STREAM_SCHEMA.to_string(),
        source: "built-in default",
    });

    // The nearest ancestor `stratum.toml`, per design 08 §4.2.
    match nearest_project_config() {
        Some(path) => settings.push(Setting {
            key: "project.config",
            value: path.to_string(),
            source: "project",
        }),
        None => settings.push(Setting {
            key: "project.config",
            value: "<none>".to_owned(),
            source: "built-in default",
        }),
    }

    settings.sort_by_key(|s| s.key);
    Report { settings, problems }
}

/// Walk up from the working directory looking for `stratum.toml`.
fn nearest_project_config() -> Option<Utf8PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let mut dir = Utf8PathBuf::from_path_buf(cwd).ok()?;
    loop {
        let candidate = dir.join("stratum.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// `stratum doctor`.
///
/// # Errors
/// [`CmdError::Io`] on a write failure.
pub fn doctor(
    args: &DoctorArgs,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<ExitCode, CmdError> {
    let report = gather();
    let format = if args.json { Format::Json } else { args.format };
    match format {
        Format::Quiet => {}
        Format::Json => {
            let line =
                serde_json::to_string(&report).map_err(|e| CmdError::Internal(e.to_string()))?;
            writeln!(out, "{line}").ok();
        }
        Format::Text => {
            for s in &report.settings {
                writeln!(out, "  {:<24} {:<40} [{}]", s.key, s.value, s.source).ok();
            }
            for p in &report.problems {
                writeln!(err, "problem: {p}").ok();
            }
        }
    }
    Ok(if report.healthy() {
        ExitCode::Success
    } else {
        // Not exit 9: nothing is broken in the build, something is missing from
        // the environment. §4.4 has no dedicated code, and "unsupported feature"
        // is what an absent engine is.
        ExitCode::Unsupported
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exit_code_is_exactly_the_healthy_predicate() {
        let report = gather();
        let args = DoctorArgs {
            format: Format::Quiet,
            json: false,
        };
        let code = doctor(&args, &mut Vec::new(), &mut Vec::new()).unwrap();
        assert_eq!(
            code == ExitCode::Success,
            report.healthy(),
            "doctor must exit 0 iff healthy"
        );
    }

    #[test]
    fn every_setting_reports_where_it_came_from() {
        let report = gather();
        assert!(!report.settings.is_empty());
        for s in &report.settings {
            assert!(!s.source.is_empty(), "{} has no source", s.key);
        }
        assert!(report.settings.iter().any(|s| s.key == "run.linesize"));
        assert!(report.settings.iter().any(|s| s.key == "engine.linked"));
    }

    /// The blocker is reported, not hidden: `doctor` is where a user finds out
    /// why `stratum run` exits 10.
    #[test]
    fn the_absent_engine_is_a_reported_problem() {
        let report = gather();
        // The problem is only expected while nothing is linked; when the const
        // flips this test's premise is gone and it must be revisited rather than
        // left asserting a stale gap.
        if crate::cmd::ENGINE_LINKED {
            assert!(report.problems.is_empty(), "{:?}", report.problems);
            return;
        }
        assert!(
            report.problems.iter().any(|p| p.contains("stratum-exec")),
            "{:?}",
            report.problems
        );
    }

    #[test]
    fn settings_come_back_in_a_stable_order() {
        let a: Vec<_> = gather().settings.iter().map(|s| s.key).collect();
        let b: Vec<_> = gather().settings.iter().map(|s| s.key).collect();
        assert_eq!(a, b);
        assert!(a.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn json_mode_is_one_document_on_stdout() {
        let args = DoctorArgs {
            format: Format::Json,
            json: true,
        };
        let mut out = Vec::new();
        doctor(&args, &mut out, &mut Vec::new()).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.lines().count(), 1);
        let v: serde_json::Value = serde_json::from_str(text.trim_end()).unwrap();
        assert!(v["settings"].is_array());
    }
}
