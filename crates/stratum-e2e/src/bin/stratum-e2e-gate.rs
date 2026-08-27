//! `stratum-e2e-gate` — the two CI gates of plan W25, as a program.
//!
//! ```text
//! stratum-e2e-gate fence   <binary>   # ADR-011: a shipped build has no e2e command
//! stratum-e2e-gate fence   --require-present <binary>   # the positive control
//! stratum-e2e-gate compare <dir>      # spec §38-E: the platforms did the same thing
//! ```
//!
//! # Why a binary here rather than only a subcommand of `xtask`
//!
//! Three callers need these two gates and only one of them can reach `xtask`:
//!
//! 1. `.github/workflows/e2e.yml`'s `fence` and `cross-platform` jobs — W25's
//!    file, but `cargo xtask e2e` does not exist yet: `xtask/src/main.rs` is
//!    W00's and carries no `mod e2e;` (R0 — see the header of `xtask/src/e2e.rs`
//!    for the exact three lines that are owed). Round 1 therefore fell back to
//!    an inline `grep` loop and a `diff -r`, which is a second implementation of
//!    a security gate written in shell.
//! 2. **W22's packaging smoke job.** W25's acceptance bullet 3 names `smoke.yml`
//!    as the fence's home, and W25 owns neither `smoke.yml` nor
//!    `xtask/src/smoke.rs`. One `cargo run -p stratum-e2e --bin
//!    stratum-e2e-gate -- fence <artifact>` is a line W22 can add to the
//!    packaged-artifact smoke job with no dependency on this unit at all.
//! 3. `cargo xtask e2e --check-fence` / `--compare`, which shell out to this
//!    binary for exactly the reason `--tier 1` shells out to cargo: one
//!    execution, not two code paths that agree by inspection. It also means
//!    `FENCED_COMMANDS` exists once in the repository instead of once here and
//!    once in a file no compiler has seen.
//!
//! Argument parsing is hand-rolled because `clap` is not a dependency of
//! `stratum-e2e` and adding one for ~30 lines of dispatch would put a proc-macro
//! crate on the path of a gate whose whole value is that it is simple enough to
//! read. The crate is excluded from `default-members` (spec §32), so this binary
//! costs a bare `cargo build` nothing and is built by `--workspace`, which is
//! what CI and this workflow run.

use std::path::Path;
use std::process::ExitCode;

use stratum_e2e::{compare, fence};

const USAGE: &str = "\
usage:
  stratum-e2e-gate fence [--require-present] <binary>
        ADR-011 — a shipped binary must carry no test-only IPC command.
        --require-present inverts it: the positive control, without which the
        negative assertion can pass because the names were never emitted by
        any build. Point it at target/debug/stratum-e2e-host-probe, which is
        built from e2e_cmds.rs and always exists, or — once W17 lands the
        feature — at stratum-desktop built with `--features e2e`, which is the
        stronger claim because it is a differential over the gate itself.

  stratum-e2e-gate compare <dir>
        Spec §38-E — DIR/<platform>/scenario_<ID>.transcript is identical on
        every platform that reported, and at least two reported.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("stratum-e2e-gate: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<String, String> {
    let (cmd, rest) = args.split_first().ok_or_else(|| USAGE.to_owned())?;
    match cmd.as_str() {
        "fence" => {
            let (require_present, path) = match rest {
                [flag, path] if flag == "--require-present" => (true, path),
                [path] if !path.starts_with("--") => (false, path),
                _ => return Err(USAGE.to_owned()),
            };
            let path = Path::new(path);
            if require_present {
                let scan = fence::check_present(path).map_err(|e| e.to_string())?;
                // Names the artifact rather than asserting how it was built.
                // Two different callers pass two different things —
                // `stratum-e2e-host-probe`, which always exists, and
                // `stratum-desktop --features e2e`, which needs W17 — and only
                // the second is evidence about the feature GATE. A verdict line
                // that said "the --features e2e build" for both would let a CI
                // reader take the weaker claim for the stronger one.
                Ok(format!(
                    "ok    positive control: {} contains {:?}, so the fence has a subject \
                     ({} bytes scanned in {} pass)",
                    path.display(),
                    scan.found,
                    scan.bytes_scanned,
                    scan.passes
                ))
            } else {
                let scan = fence::check_absent(path).map_err(|e| e.to_string())?;
                Ok(format!(
                    "ok    e2e fence: {} contains none of {:?} ({} bytes scanned in {} pass)",
                    path.display(),
                    fence::FENCED_COMMANDS,
                    scan.bytes_scanned,
                    scan.passes
                ))
            }
        }
        "compare" => {
            let [dir] = rest else {
                return Err(USAGE.to_owned());
            };
            compare::compare_dir(Path::new(dir))
                .map(|r| r.to_string())
                .map_err(|e| e.to_string())
        }
        _ => Err(USAGE.to_owned()),
    }
}
