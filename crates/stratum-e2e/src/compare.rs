//! **Spec §38-E** — the same analysis file produced equivalent runtime results
//! on every platform.
//!
//! Each tier-1 job writes `DIR/<platform>/scenario_<ID>.transcript`
//! (`tests/e2e/harness.rs`, gated on `STRATUM_E2E_TRANSCRIPT_DIR`); this module
//! is the comparison the `cross-platform` job in `.github/workflows/e2e.yml`
//! then runs over the downloaded artifacts.
//!
//! [`crate::ScenarioReport::transcript`] is host-free and duration-free by
//! construction, so a byte difference here is a difference in **what the
//! application did** on one platform and not on another — which is exactly the
//! claim §38-E makes, and the reason this is not `diff -r` in a shell step: a
//! shell diff cannot tell "macOS produced no transcript at all" from "the two
//! agreed", and the first of those is the failure §38-E is about.
//!
//! Same reason as [`crate::fence`] for living in this crate rather than in
//! `xtask/src/e2e.rs`: that file is compiled by nothing until W00 registers it,
//! so logic kept there is logic no compiler has checked.

use std::path::{Path, PathBuf};

use crate::ScenarioId;

/// The five scenarios, in the order a report lists them.
const ALL: [ScenarioId; 5] = [
    ScenarioId::A,
    ScenarioId::B,
    ScenarioId::C,
    ScenarioId::D,
    ScenarioId::E,
];

/// What the comparison proved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompareReport {
    /// The platform directories found, sorted. These are whatever the CI matrix
    /// named them (`macos-15`, `windows-2022`, `ubuntu-22.04`).
    pub platforms: Vec<String>,
    /// Scenarios that had a transcript on every platform and matched.
    pub scenarios: Vec<ScenarioId>,
    /// Transcript files actually read. `platforms.len() * scenarios.len()` when
    /// every platform reported every scenario — the counter that distinguishes
    /// "three platforms agreed" from "one platform reported and nothing
    /// disagreed with it".
    pub files_read: u32,
}

impl std::fmt::Display for CompareReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ok    §38-E: {} scenario transcript(s) identical across {} platforms ({}), \
             {} files read",
            self.scenarios.len(),
            self.platforms.len(),
            self.platforms.join(", "),
            self.files_read
        )
    }
}

/// Anything that stopped §38-E being asserted, or the assertion failing.
#[derive(Debug, thiserror::Error)]
pub enum CompareError {
    /// The transcript directory would not read.
    #[error("reading the transcript directory {path}: {source}")]
    Io {
        /// The directory or file.
        path: PathBuf,
        /// Why not.
        source: std::io::Error,
    },
    /// Fewer than two platforms reported, so there is nothing to compare and a
    /// pass would mean nothing.
    #[error(
        "spec §38-E compares platforms: found {found} transcript director(y/ies) under \
         {path}, need at least two"
    )]
    TooFewPlatforms {
        /// The directory.
        path: PathBuf,
        /// How many platform directories were in it.
        found: usize,
    },
    /// One platform produced a transcript another did not.
    #[error("{present} produced {file} and {absent} did not: the run was not equivalent")]
    Asymmetric {
        /// The platform that has it.
        present: String,
        /// The platform that does not.
        absent: String,
        /// The transcript file name.
        file: String,
    },
    /// The platforms disagree about what the application did.
    #[error(
        "spec §38-E: {base} and {other} disagree about scenario {scenario}:\n\
         --- {base}\n{base_text}\n--- {other}\n{other_text}"
    )]
    Diverged {
        /// The reference platform (first, sorted).
        base: String,
        /// The platform that differs.
        other: String,
        /// Which scenario.
        scenario: ScenarioId,
        /// The reference transcript.
        base_text: String,
        /// What the other platform produced.
        other_text: String,
    },
    /// The directories exist but contain no transcript at all.
    #[error("no transcript to compare under {path}: every platform directory is empty")]
    Empty {
        /// The directory.
        path: PathBuf,
    },
}

/// The transcript file name a scenario writes.
#[must_use]
pub fn transcript_name(id: ScenarioId) -> String {
    format!("scenario_{id}.transcript")
}

/// Compare `dir/<platform>/scenario_*.transcript` across every platform present.
///
/// # Errors
/// I/O, fewer than two platforms, a scenario present on one platform and absent
/// on another, or a byte difference between two platforms' transcripts.
pub fn compare_dir(dir: &Path) -> Result<CompareReport, CompareError> {
    let mut platforms: Vec<(String, PathBuf)> = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(|source| CompareError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| CompareError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let is_dir = entry
            .file_type()
            .map_err(|source| CompareError::Io {
                path: entry.path(),
                source,
            })?
            .is_dir();
        if is_dir {
            platforms.push((
                entry.file_name().to_string_lossy().into_owned(),
                entry.path(),
            ));
        }
    }
    platforms.sort();

    if platforms.len() < 2 {
        return Err(CompareError::TooFewPlatforms {
            path: dir.to_path_buf(),
            found: platforms.len(),
        });
    }

    let (base_name, base_dir) = platforms[0].clone();
    let mut scenarios = Vec::new();
    let mut files_read = 0u32;

    for id in ALL {
        let file = transcript_name(id);
        // Absent on the reference platform is not a failure on its own: a
        // scenario nobody ran anywhere has nothing to say about §38-E. Absent on
        // only *some* platforms is `Asymmetric`, below.
        let Ok(base_text) = std::fs::read_to_string(base_dir.join(&file)) else {
            for (name, other_dir) in platforms.iter().skip(1) {
                if other_dir.join(&file).is_file() {
                    return Err(CompareError::Asymmetric {
                        present: name.clone(),
                        absent: base_name.clone(),
                        file,
                    });
                }
            }
            continue;
        };
        files_read += 1;

        for (name, other_dir) in platforms.iter().skip(1) {
            let path = other_dir.join(&file);
            let other_text =
                std::fs::read_to_string(&path).map_err(|_| CompareError::Asymmetric {
                    present: base_name.clone(),
                    absent: name.clone(),
                    file: file.clone(),
                })?;
            files_read += 1;
            if base_text != other_text {
                return Err(CompareError::Diverged {
                    base: base_name.clone(),
                    other: name.clone(),
                    scenario: id,
                    base_text,
                    other_text,
                });
            }
        }
        scenarios.push(id);
    }

    if scenarios.is_empty() {
        return Err(CompareError::Empty {
            path: dir.to_path_buf(),
        });
    }

    Ok(CompareReport {
        platforms: platforms.into_iter().map(|(n, _)| n).collect(),
        scenarios,
        files_read,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, platform: &str, id: ScenarioId, text: &str) {
        let dir = root.join(platform);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(transcript_name(id)), text).unwrap();
    }

    const A: &str = "scenario A\n0 open via injection passed\n1 run block passed\n";

    #[test]
    fn comparing_fewer_than_two_platforms_is_an_error_not_a_pass() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "ubuntu-22.04", ScenarioId::A, A);
        let err = compare_dir(dir.path()).expect_err("one platform proves nothing");
        assert!(err.to_string().contains("need at least two"), "{err}");
    }

    #[test]
    fn two_platforms_that_did_the_same_thing_compare_equal() {
        let dir = tempfile::tempdir().unwrap();
        for os in ["macos-15", "ubuntu-22.04"] {
            write(dir.path(), os, ScenarioId::A, A);
        }
        let report = compare_dir(dir.path()).expect("identical transcripts");
        assert_eq!(report.scenarios, vec![ScenarioId::A]);
        assert_eq!(report.platforms, vec!["macos-15", "ubuntu-22.04"]);
        // The counter that says both sides were actually read.
        assert_eq!(report.files_read, 2);
    }

    #[test]
    fn a_one_line_divergence_fails_with_the_scenario_named() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "macos-15", ScenarioId::A, A);
        write(
            dir.path(),
            "ubuntu-22.04",
            ScenarioId::A,
            "scenario A\n0 open via injection blocked W13\n1 run block passed\n",
        );
        let err = compare_dir(dir.path()).expect_err("a divergence must fail");
        let msg = err.to_string();
        assert!(msg.contains("disagree about scenario A"), "{msg}");
        assert!(msg.contains("blocked W13"), "{msg}");
    }

    /// The failure mode a shell `diff -r` reports as success: one platform ran
    /// the scenario and another silently did not.
    #[test]
    fn a_scenario_missing_on_one_platform_is_a_failure_not_a_match() {
        let dir = tempfile::tempdir().unwrap();
        for os in ["macos-15", "ubuntu-22.04"] {
            write(dir.path(), os, ScenarioId::A, A);
        }
        write(dir.path(), "macos-15", ScenarioId::C, "scenario C\n");
        let err = compare_dir(dir.path()).expect_err("asymmetry is not agreement");
        assert!(err.to_string().contains("did not"), "{err}");

        // And the mirror image: present on the reference platform, absent on
        // the other. `platforms` is sorted, so `macos-15` is the reference.
        let dir = tempfile::tempdir().unwrap();
        for os in ["macos-15", "ubuntu-22.04"] {
            write(dir.path(), os, ScenarioId::A, A);
        }
        write(dir.path(), "ubuntu-22.04", ScenarioId::C, "scenario C\n");
        let err = compare_dir(dir.path()).expect_err("asymmetry is not agreement");
        assert!(err.to_string().contains("did not"), "{err}");
    }

    #[test]
    fn platform_directories_with_no_transcripts_are_not_a_pass() {
        let dir = tempfile::tempdir().unwrap();
        for os in ["macos-15", "ubuntu-22.04"] {
            std::fs::create_dir_all(dir.path().join(os)).unwrap();
        }
        let err = compare_dir(dir.path()).expect_err("nothing compared is not a pass");
        assert!(err.to_string().contains("no transcript"), "{err}");
    }

    #[test]
    fn every_scenario_present_everywhere_is_counted() {
        let dir = tempfile::tempdir().unwrap();
        for os in ["macos-15", "ubuntu-22.04", "windows-2022"] {
            for id in ALL {
                write(dir.path(), os, id, &format!("scenario {id}\nsteps ok\n"));
            }
        }
        let report = compare_dir(dir.path()).expect("all five, all three");
        assert_eq!(report.scenarios.len(), 5);
        assert_eq!(report.platforms.len(), 3);
        assert_eq!(report.files_read, 15, "3 platforms x 5 scenarios");
    }
}
