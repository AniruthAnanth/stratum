//! **ADR-011's fence** — a shipped binary must carry no test-only IPC command.
//!
//! > "A test-only IPC command that reaches production is a remote-control
//! > backdoor."
//!
//! The `e2e` cargo feature on `stratum-desktop` gates
//! `e2e_cmds::tauri_surface`, which is the only place [`E2E_DISPATCH`] and
//! [`E2E_SNAPSHOT`] are *referenced*; with the feature off nothing mentions
//! them, so they never reach `.rodata`. This module is the assertion that the
//! gate held.
//!
//! # Why the check lives here and not in `xtask`
//!
//! It lived in `xtask/src/e2e.rs` through repair round 1 and was **compiled by
//! nothing**: `xtask/src/main.rs` is W00's file and has no `mod e2e;` (R0 — W25
//! may not add it). A security-shaped assertion whose implementation no compiler
//! has ever seen is not an assertion. The fence's *subject* was in the same
//! state until repair round 3, when [`crate::host`] pulled `e2e_cmds.rs` into
//! this crate with a `#[path]`, so the two command-name constants are now
//! compared by the compiler and not only by a source-text scrape. `crates/stratum-e2e` is W25's own crate
//! and is built and tested by `cargo build --workspace` / `cargo test
//! --workspace` today, so the check is compiled, clippy-covered and unit-tested
//! from this commit forward, whatever happens to the xtask registration.
//!
//! Three callers, one implementation:
//!
//! * `cargo run -p stratum-e2e --bin stratum-e2e-gate -- fence <binary>` —
//!   what `.github/workflows/e2e.yml`'s `fence` job runs, and what W22's
//!   packaging smoke job can run over a *packaged* artifact without depending on
//!   `xtask` at all (the acceptance bullet names `smoke.yml`, which W25 does not
//!   own);
//! * `cargo xtask e2e --check-fence <binary>`, which shells out to that binary
//!   rather than keeping a second copy of `FENCED_COMMANDS` to drift against;
//! * [`crate::fence`] directly, from the drift test in `tests/e2e/harness.rs`.
//!
//! # A byte scan, not an introspection call
//!
//! The claim is about *what is in the artifact*, because the artifact is what
//! ships. Asking a running app for its command table proves the command is not
//! registered on this run, which is strictly weaker.
//!
//! # Counters, not stopwatches (ADR-017)
//!
//! [`scan_for`] reads the artifact **once**, whatever the length of
//! `FENCED_COMMANDS`: one pass, matched against every needle simultaneously
//! through a first-byte bitmap. That is the asserted property
//! ([`FenceScan::passes`] is 1; `bytes_scanned` for a clean binary is the file
//! length and does not grow with the needle count), rather than a duration —
//! the round-1 implementation ran `haystack.windows(n).any(..)` once *per name*,
//! which is a second full pass over a release binary for every command the fence
//! learns about.

use std::path::{Path, PathBuf};

/// The Tauri command that routes a harness action into the app's own command
/// registry. Must equal `e2e_cmds::E2E_DISPATCH`; asserted by
/// `the_fence_and_the_host_agree_on_the_command_names` in `tests/e2e/harness.rs`.
pub const E2E_DISPATCH: &str = "e2e_dispatch";

/// The Tauri command that reads gutter glyphs, card headers, panes and the
/// document back out. Must equal `e2e_cmds::E2E_SNAPSHOT`.
pub const E2E_SNAPSHOT: &str = "e2e_snapshot";

/// Every name that must be absent from a shipped binary.
pub const FENCED_COMMANDS: &[&str] = &[E2E_DISPATCH, E2E_SNAPSHOT];

/// What one pass over an artifact found, and what it cost.
///
/// `'n` is the lifetime of the needle list, not of the artifact. It is a
/// parameter rather than `'static` because the scanner has no reason to require
/// its subject be known at compile time, and requiring it broke the one test
/// that matters most: the negative control in
/// [`the_scanner_finds_a_fenced_name_in_a_real_linked_binary`](self) has to ask
/// about a name that is *not* in the artifact, and any name written as a literal
/// is put in that artifact's `.rodata` by rustc, so it must be built at run
/// time. [`scan`] and both `check_*` functions return `FenceScan<'static>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FenceScan<'n> {
    /// The fenced names present in the artifact, in `FENCED_COMMANDS` order.
    pub found: Vec<&'n str>,
    /// Bytes actually examined. Equal to the artifact length when nothing is
    /// found, which is the shipping case and the one the gate asserts.
    pub bytes_scanned: u64,
    /// Passes made over the artifact. Always 1 — see the module header.
    pub passes: u32,
}

/// Anything that stopped the fence being asserted, or the assertion failing.
#[derive(Debug, thiserror::Error)]
pub enum FenceError {
    /// The artifact would not read.
    #[error("reading {path} to check the e2e fence: {source}")]
    Io {
        /// The artifact.
        path: PathBuf,
        /// Why not.
        source: std::io::Error,
    },
    /// The fence did not hold: a shipped artifact carries test-only commands.
    #[error(
        "{path} contains the test-only IPC command(s) {found:?}. ADR-011: a test-only IPC \
         command that reaches production is a remote-control backdoor. The `e2e` cargo \
         feature must be off in every shipped build."
    )]
    Breached {
        /// The artifact.
        path: PathBuf,
        /// What was found in it.
        found: Vec<&'static str>,
    },
    /// The *positive control* failed: a build made with `--features e2e` does
    /// not contain the names, so the negative assertion would pass vacuously.
    #[error(
        "{path} was built with --features e2e but does not contain {missing:?}, so the \
         fence has no subject and passes for the wrong reason. The command names in \
         apps/desktop/src-tauri/src/e2e_cmds.rs and stratum_e2e::fence::FENCED_COMMANDS \
         have drifted apart."
    )]
    NoSubject {
        /// The artifact.
        path: PathBuf,
        /// What should have been in it and was not.
        missing: Vec<&'static str>,
    },
}

/// Scan `haystack` for every name in [`FENCED_COMMANDS`], in one pass.
#[must_use]
pub fn scan(haystack: &[u8]) -> FenceScan<'static> {
    scan_for(haystack, FENCED_COMMANDS)
}

/// Scan `haystack` for `needles`, in one pass whatever `needles.len()` is.
///
/// A 256-entry first-byte bitmap turns the common case — a byte that starts no
/// needle — into one array lookup, so the cost is the artifact length plus the
/// comparisons at the few positions that could start a match. `FENCED_COMMANDS`
/// share the `e2e_` prefix, so on a real binary the candidate positions are the
/// handful of places that string appears at all.
#[must_use]
pub fn scan_for<'n>(haystack: &[u8], needles: &[&'n str]) -> FenceScan<'n> {
    let mut starts = [false; 256];
    for n in needles {
        if let Some(&b) = n.as_bytes().first() {
            starts[b as usize] = true;
        }
    }

    let mut found: Vec<&'n str> = Vec::new();
    let mut scanned = 0u64;
    for (i, &byte) in haystack.iter().enumerate() {
        scanned += 1;
        if !starts[byte as usize] {
            continue;
        }
        let tail = &haystack[i..];
        for n in needles {
            if found.contains(n) {
                continue;
            }
            let bytes = n.as_bytes();
            if !bytes.is_empty() && tail.len() >= bytes.len() && &tail[..bytes.len()] == bytes {
                found.push(*n);
            }
        }
        if found.len() == needles.len() {
            // Every needle is accounted for; reading the rest cannot change the
            // answer. The shipping case finds nothing and therefore always
            // reads the whole artifact.
            break;
        }
    }

    // Keep the report in FENCED_COMMANDS order rather than discovery order, so
    // a CI log line is stable across builds.
    found.sort_unstable_by_key(|n| needles.iter().position(|c| c == n).unwrap_or(usize::MAX));

    FenceScan {
        found,
        bytes_scanned: scanned,
        passes: 1,
    }
}

/// Assert that a **shipped** artifact carries none of the fenced commands.
///
/// # Errors
/// [`FenceError::Io`] if the artifact will not read, [`FenceError::Breached`] if
/// the fence did not hold.
pub fn check_absent(path: &Path) -> Result<FenceScan<'static>, FenceError> {
    let bytes = read(path)?;
    let scan = scan(&bytes);
    if scan.found.is_empty() {
        Ok(scan)
    } else {
        Err(FenceError::Breached {
            path: path.to_path_buf(),
            found: scan.found,
        })
    }
}

/// The **positive control**: assert that an `--features e2e` artifact carries
/// all of them.
///
/// Without this, "absent from the shipped binary" is satisfied by a fence that
/// greps for a string no build ever emits.
///
/// # Errors
/// [`FenceError::Io`] if the artifact will not read, [`FenceError::NoSubject`]
/// if the names have drifted apart from the host's.
pub fn check_present(path: &Path) -> Result<FenceScan<'static>, FenceError> {
    let bytes = read(path)?;
    let scan = scan(&bytes);
    let missing: Vec<&'static str> = FENCED_COMMANDS
        .iter()
        .filter(|n| !scan.found.contains(n))
        .copied()
        .collect();
    if missing.is_empty() {
        Ok(scan)
    } else {
        Err(FenceError::NoSubject {
            path: path.to_path_buf(),
            missing,
        })
    }
}

fn read(path: &Path) -> Result<Vec<u8>, FenceError> {
    std::fs::read(path).map_err(|source| FenceError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A "binary" as a release build with the feature accidentally left on
    /// would be.
    const BREACHED: &[u8] = b"\x7fELF...doc_save...e2e_dispatch...e2e_snapshot...";
    /// The same build with the feature off: the surface that references the
    /// names is gone, so the names are gone.
    const CLEAN: &[u8] = b"\x7fELF...doc_save...menu_accelerator...run_block...";

    #[test]
    fn the_fence_catches_a_binary_that_kept_its_test_only_commands() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stratum-desktop");

        std::fs::write(&path, BREACHED).unwrap();
        let err = check_absent(&path).expect_err("the fence must catch this");
        let msg = err.to_string();
        assert!(msg.contains("e2e_dispatch"), "{msg}");
        assert!(msg.contains("backdoor"), "{msg}");

        std::fs::write(&path, CLEAN).unwrap();
        check_absent(&path).expect("a fenced build passes");
    }

    #[test]
    fn a_vacuous_positive_control_is_an_error_not_a_pass() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stratum-desktop");

        std::fs::write(&path, CLEAN).unwrap();
        let err = check_present(&path).expect_err("an e2e build without the names is a drift");
        assert!(err.to_string().contains("no subject"), "{err}");

        std::fs::write(&path, BREACHED).unwrap();
        check_present(&path).expect("an e2e build carries both names");
    }

    /// ADR-017. The property the plan states as speed, asserted as the counter
    /// that causes it: the artifact is read **once**, not once per fenced name.
    /// A release `stratum-desktop` is tens of megabytes once Tauri lands and
    /// this runs on three OSes on every push.
    #[test]
    fn a_clean_artifact_is_read_once_however_many_names_are_fenced() {
        let mut haystack = Vec::with_capacity(1 << 16);
        while haystack.len() < (1 << 16) {
            haystack.extend_from_slice(CLEAN);
        }

        let one = scan_for(&haystack, &[E2E_DISPATCH]);
        let both = scan(&haystack);
        let many = scan_for(
            &haystack,
            &[E2E_DISPATCH, E2E_SNAPSHOT, "e2e_reply", "e2e_ready"],
        );

        for s in [&one, &both, &many] {
            assert_eq!(s.passes, 1, "the fence makes exactly one pass");
            assert_eq!(
                s.bytes_scanned,
                haystack.len() as u64,
                "a clean artifact is scanned exactly once end to end"
            );
            assert!(s.found.is_empty());
        }
    }

    #[test]
    fn a_breach_is_reported_in_a_stable_order_and_stops_early() {
        let mut haystack = BREACHED.to_vec();
        haystack.extend(std::iter::repeat_n(b'\0', 1 << 16));
        let s = scan(&haystack);
        assert_eq!(s.found, vec![E2E_DISPATCH, E2E_SNAPSHOT]);
        assert_eq!(s.passes, 1);
        assert!(
            s.bytes_scanned < haystack.len() as u64,
            "once every name is accounted for the rest cannot change the answer"
        );
    }

    #[test]
    fn a_name_split_across_the_end_of_the_artifact_is_not_a_match() {
        // The truncation case: the last bytes look like the start of a needle.
        let s = scan(b"....e2e_dis");
        assert!(s.found.is_empty());
    }

    #[test]
    fn a_missing_artifact_is_an_error_rather_than_a_pass() {
        let dir = tempfile::tempdir().unwrap();
        let err = check_absent(&dir.path().join("never-built")).expect_err("must not pass");
        assert!(matches!(err, FenceError::Io { .. }), "{err}");
    }

    /// **The positive control over a REAL linked artifact.**
    ///
    /// Every other case above builds its haystack out of a byte-string literal,
    /// which proves the matcher and nothing about the thing the matcher is
    /// pointed at. The one artifact this crate can be sure exists and is sure to
    /// contain both names is *this test binary*: `E2E_DISPATCH` and
    /// `E2E_SNAPSHOT` are `pub const &str`, they are referenced from the cases
    /// above, and rustc puts them in its read-only data exactly the way it will
    /// put `e2e_cmds::tauri_surface`'s copies in `stratum-desktop`.
    ///
    /// **What this does and does not establish.** It establishes that the scan
    /// finds a fenced name inside a real, linked, platform-native executable —
    /// Mach-O here, ELF and PE on the other two CI runners — rather than only
    /// inside a 48-byte fake. Repair round 3 added the artifact that makes the
    /// same point about the *host's* constants rather than this module's:
    /// `stratum-e2e-host-probe`, built from `e2e_cmds.rs` through
    /// [`crate::host`], which `e2e.yml` runs `fence --require-present` against
    /// on every push.
    ///
    /// Neither establishes that `stratum-desktop --release` is clean *because
    /// the feature gate held*. That is a differential over two builds of the
    /// same crate and it needs `[features] e2e` in
    /// `apps/desktop/src-tauri/Cargo.toml` and `mod e2e_cmds;` in its `main.rs`,
    /// both of which are W17's files (R0). Until they land, the `fence` job says
    /// out loud which of the two claims it made.
    #[test]
    fn the_scanner_finds_a_fenced_name_in_a_real_linked_binary() {
        let me = std::env::current_exe().expect("this test binary's own path");
        let scan = scan(&std::fs::read(&me).expect("reading this test binary"));
        assert_eq!(
            scan.found,
            vec![E2E_DISPATCH, E2E_SNAPSHOT],
            "the fence did not find its own constants in {}. Every negative result \
             this module reports is only as good as this positive one: a scanner that \
             cannot see a name in a real executable reports every shipped binary clean.",
            me.display()
        );
        assert_eq!(scan.passes, 1);

        // And the negative direction over the same real artifact: a name that is
        // NOT in the binary must not be reported, or "found" means "scanned".
        //
        // The needle is BUILT at run time. The first draft of this test used a
        // string literal, which rustc dutifully put in this binary's `.rodata`,
        // so the scan found it and the test failed — correctly. A needle that
        // cannot be a literal is the only kind this particular haystack can be
        // asked about.
        let absent = format!("e2e_absent_{}_{}", std::process::id(), scan.bytes_scanned);
        let needles: &[&str] = &[absent.as_str()];
        let miss = scan_for(
            &std::fs::read(&me).expect("reading this test binary"),
            needles,
        );
        assert!(miss.found.is_empty(), "{:?}", miss.found);
        assert_eq!(
            miss.bytes_scanned,
            std::fs::metadata(&me).expect("size").len(),
            "a clean scan reads the whole artifact exactly once"
        );
    }
}
