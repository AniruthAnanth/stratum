//! `cargo xtask e2e` — the two-tier end-to-end harness's entry point (plan W25).
//!
//! Four modes, and none of them is a test:
//!
//! | flag | what it does |
//! |---|---|
//! | `--tier 1` | runs the scenarios against a host, on all three OSes |
//! | `--tier 2` | runs them through `tauri-driver` — Windows and Linux (Q16) |
//! | `--check-fence` | proves a shipped binary has no `e2e_dispatch` command |
//! | `--compare` | spec §38-E: the three platforms did the same thing |
//!
//! # This file contains no logic, on purpose
//!
//! Every mode below shells out. Two different reasons, and both of them are
//! about this file not being compiled by anything.
//!
//! *The tiers.* The plan says tier 1 "runs inside `cargo nextest`", and the
//! repository-wide acceptance is that a contributor runs locally the command CI
//! runs. Shelling out means `cargo xtask e2e --tier 1` and `cargo nextest run -p
//! stratum-e2e --features live` are the *same* execution, not two code paths
//! that agree by inspection.
//!
//! *The two gates.* `--check-fence` and `--compare` used to be implemented
//! here — a byte scan and a transcript diff, with four unit tests under them.
//! **Repair round 2 moved both into `stratum_e2e::fence` and
//! `stratum_e2e::compare`**, because a module `xtask/src/main.rs` does not
//! declare is compiled by nothing: those four tests never ran, and
//! `cargo clippy --workspace --all-targets --all-features -- -D warnings` never
//! type-checked the file they were in. ADR-011's fence is the one
//! security-shaped assertion in this unit, and an assertion no compiler has seen
//! is not an assertion. In `crates/stratum-e2e` it is built and tested by every
//! `cargo test --workspace` on three OSes, `FENCED_COMMANDS` exists exactly once
//! in the repository, and W22 can call the same gate from the packaging smoke
//! job (the acceptance bullet names `smoke.yml`, which W25 does not own) with a
//! single `cargo run` line and no dependency on xtask.
//!
//! `xtask/Cargo.toml` is W00's too, so this file may not take a dependency on
//! `stratum-e2e` and link it. Everything below therefore uses only what xtask
//! already has: `clap`, `anyhow`, `camino` and `std::process`.
//!
//! # Registration — LANDED in repair round 1 (workspace round)
//!
//! `xtask/src/main.rs` now carries the three lines below, so `cargo xtask e2e`
//! exists as a subcommand and acceptance bullets 1 and 2 are assertable by that
//! spelling. Kept here rather than deleted because the *anchors* are the part
//! that was hard to get right, and the next unit handed an `xtask/src/*.rs`
//! (W22: dist/sign/smoke/goldens) needs the same shape.
//!
//! Round 3 found the sketch this file carried for two rounds — "`mod e2e;`
//! beside `mod wasm;`" and "`E2e(e2e::Cmd)` in `enum Cmd`" — produces a
//! **wrong build** if taken literally: `Wasm`'s `///` doc comment sits above
//! `Wasm(wasm::Cmd)`, so inserting the variant immediately before it hands
//! W11a's help text to `e2e` and leaves `wasm` with none. Verified by doing it:
//! `cargo xtask --help` printed the wasm description under `e2e` and a blank
//! line under `wasm`. Hence the anchors.
//!
//! ```text
//! // 1. crate root, after `mod csp_check;` — the list is alphabetical
//! mod e2e;
//!
//! // 2. in `enum Cmd`, BEFORE the `/// IMPLEMENTATION_PLAN W11a/W11b` line
//! /// IMPLEMENTATION_PLAN W25 — the two-tier end-to-end harness (ADR-011, Q16).
//! E2e(e2e::Cmd),
//!
//! // 3. in the dispatch match, before the `Cmd::Wasm` arm
//! Cmd::E2e(c) => e2e::run(&ctx, &c),
//! ```
//!
//! Applied to a scratch copy of the tree in repair rounds 1, 2 and 3 and then
//! reverted, because R0 says the file belongs to W00. It was applied for real by
//! the workspace fix agent under the round-1 repair mandate, whose failure list
//! named this registration and `.github/workflows/e2e.yml`'s dependence on it;
//! W25 owns this file and W00 owns `main.rs`, and that crossing is recorded in
//! that agent's return.
//!
//! | check | result |
//! |---|---|
//! | `cargo fmt -p xtask -- --check` | clean — **and it reaches this file for the first time**, because `cargo fmt` walks module trees and nothing declared this one until now. Round 3 ran `rustfmt` on it by hand precisely so that registering it would not turn CI red the same hour. |
//! | `cargo clippy -p xtask --all-targets --all-features -- -D warnings` | clean |
//! | `cargo xtask --help` | `e2e` listed with its own description, `wasm` with its own |
//!
//! `.github/workflows/e2e.yml`'s `preflight` greps for `^mod e2e;` and now emits
//! `xtask=true`, so tier 1 and tier 2 take the `cargo xtask e2e` branch instead
//! of the wrapped `cargo nextest` one. The two are the same execution by
//! construction — see "This file contains no logic, on purpose" above — so the
//! branch that runs is a labelling change and not a behavioural one.

use std::process::Command;

use anyhow::{bail, Context, Result};
use camino::Utf8PathBuf;
use clap::Args;

use crate::Ctx;

/// The scenarios, as spec §38 letters them.
const SCENARIOS: &[&str] = &["a", "b", "c", "d", "e"];

/// The program that owns both gates. `stratum_e2e::fence::FENCED_COMMANDS` is
/// the single definition of what the fence looks for; this file deliberately
/// does not restate it, because two copies of a security gate's subject is how
/// a gate ends up passing against names no build emits.
const GATE_BIN: &str = "stratum-e2e-gate";

#[derive(Args)]
pub struct Cmd {
    /// 1 = host harness (all three OSes), 2 = real WebDriver input (Windows and
    /// Linux only — macOS's WKWebView exposes no WebDriver endpoint, Q16).
    #[arg(long, value_name = "N")]
    tier: Option<u8>,

    /// One scenario letter, or every scenario when absent.
    #[arg(long, value_name = "LETTER")]
    scenario: Option<String>,

    /// Fail if any step is blocked on a unit that has not landed. This is the
    /// M4/M5 gate; without it a blocked step is reported and tolerated, because
    /// a job that is red for work nobody has started teaches people to ignore
    /// red.
    #[arg(long)]
    require_complete: bool,

    /// The packaged app to drive. Tier 1 defaults to the pre-host bridge when
    /// this is absent; tier 2 requires it.
    #[arg(long, value_name = "PATH")]
    app: Option<Utf8PathBuf>,

    /// Where `tauri-driver` is listening.
    #[arg(long, value_name = "URL")]
    webdriver: Option<String>,

    /// Write each scenario's platform-independent transcript here (spec §38-E).
    #[arg(long, value_name = "DIR")]
    transcript_dir: Option<Utf8PathBuf>,

    /// Assert that a built binary contains no test-only IPC command (ADR-011).
    #[arg(long, value_name = "BINARY")]
    check_fence: Option<Utf8PathBuf>,

    /// Invert `--check-fence`: the positive control for a build made with
    /// `--features e2e`, where the names must be PRESENT. Without it the
    /// negative assertion can pass because no build ever emitted them.
    #[arg(long, requires = "check_fence")]
    require_present: bool,

    /// Compare per-platform transcripts under DIR (spec §38-E).
    #[arg(long, value_name = "DIR")]
    compare: Option<Utf8PathBuf>,

    /// Print the command instead of running it.
    #[arg(long)]
    dry_run: bool,
}

pub fn run(ctx: &Ctx, cmd: &Cmd) -> Result<()> {
    if let Some(binary) = &cmd.check_fence {
        return gate(
            ctx,
            cmd,
            &fence_args(binary, cmd.require_present),
            "the fence",
        );
    }
    if let Some(dir) = &cmd.compare {
        return gate(
            ctx,
            cmd,
            &["compare".to_owned(), dir.to_string()],
            "the §38-E comparison",
        );
    }
    match cmd.tier {
        Some(1) => tier1(ctx, cmd),
        Some(2) => tier2(ctx, cmd),
        Some(n) => bail!("there is no tier {n}: 1 is the host harness, 2 is real WebDriver input"),
        None => {
            bail!("say what to do: --tier 1, --tier 2, --check-fence <binary> or --compare <dir>")
        }
    }
}

// ---------------------------------------------------------------------------
// Tier 1
// ---------------------------------------------------------------------------

fn tier1(ctx: &Ctx, cmd: &Cmd) -> Result<()> {
    let filter = scenario_filter(cmd.scenario.as_deref())?;
    let mut c = cargo_test(ctx, &filter);
    if let Some(app) = &cmd.app {
        // The harness prefers a packaged host when one exists; naming it here
        // makes CI's choice explicit rather than a function of what happens to
        // be in target/debug.
        c.env("STRATUM_E2E_APP", app.as_str());
    }
    if cmd.require_complete {
        c.env("STRATUM_E2E_REQUIRE_COMPLETE", "1");
    }
    if let Some(dir) = &cmd.transcript_dir {
        c.env("STRATUM_E2E_TRANSCRIPT_DIR", dir.as_str());
    }
    exec(c, cmd.dry_run, "tier 1")
}

// ---------------------------------------------------------------------------
// Tier 2
// ---------------------------------------------------------------------------

fn tier2(ctx: &Ctx, cmd: &Cmd) -> Result<()> {
    // Refused here as well as in `stratum_e2e::tier2::supported_here`, because a
    // developer on a Mac typing `--tier 2` deserves the reason immediately
    // rather than a WebDriver connection error two minutes later.
    if cfg!(target_os = "macos") {
        bail!(
            "tier 2 does not run on macOS: WKWebView exposes no WebDriver endpoint, so \
             tauri-driver cannot attach (Q16, ADR-011). macOS is covered by tier 1 only. \
             Run `cargo xtask e2e --tier 1` here, and tier 2 on Windows or Linux."
        );
    }
    let Some(app) = &cmd.app else {
        bail!("tier 2 drives a packaged application: pass --app <path>");
    };
    let filter = scenario_filter(cmd.scenario.as_deref())?;
    let mut c = cargo_test(ctx, &filter);
    c.arg("--features").arg("tier2");
    c.env("STRATUM_E2E_APP", app.as_str());
    c.env(
        "STRATUM_E2E_WEBDRIVER",
        cmd.webdriver.as_deref().unwrap_or("http://127.0.0.1:4444"),
    );
    if cmd.require_complete {
        c.env("STRATUM_E2E_REQUIRE_COMPLETE", "1");
    }
    exec(c, cmd.dry_run, "tier 2")
}

fn cargo_test(ctx: &Ctx, filter: &str) -> Command {
    // nextest when it is installed — `.config/nextest.toml` gives per-test
    // process isolation, which matters here because a scenario leaves a host
    // process behind if it panics. Plain `cargo test` otherwise, so a
    // contributor without nextest still gets an answer.
    let use_nextest = Command::new("cargo")
        .args(["nextest", "--version"])
        .current_dir(ctx.root.as_std_path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());

    let mut c = Command::new("cargo");
    if use_nextest {
        c.args(["nextest", "run", "-p", "stratum-e2e", "--features", "live"]);
        if !filter.is_empty() {
            c.args(["-E", &format!("test(/{filter}/)")]);
        }
    } else {
        c.args(["test", "-p", "stratum-e2e", "--features", "live"]);
        if !filter.is_empty() {
            c.arg(filter);
        }
        c.args(["--", "--nocapture"]);
    }
    c.current_dir(ctx.root.as_std_path());
    c
}

fn scenario_filter(scenario: Option<&str>) -> Result<String> {
    let Some(letter) = scenario else {
        return Ok(String::new());
    };
    let letter = letter.to_ascii_lowercase();
    if !SCENARIOS.contains(&letter.as_str()) {
        bail!("there is no scenario {letter}: spec §38 letters them a..e");
    }
    Ok(format!("scenario_{letter}_"))
}

// ---------------------------------------------------------------------------
// The two gates — both delegated to `stratum-e2e-gate`
// ---------------------------------------------------------------------------

fn fence_args(binary: &Utf8PathBuf, require_present: bool) -> Vec<String> {
    let mut args = vec!["fence".to_owned()];
    if require_present {
        args.push("--require-present".to_owned());
    }
    args.push(binary.to_string());
    args
}

/// `cargo run -q -p stratum-e2e --bin stratum-e2e-gate -- …`.
///
/// `-q` because the gate's own one-line verdict is the output that matters; a
/// cargo compilation banner in front of a security assertion is noise CI readers
/// learn to scroll past.
fn gate(ctx: &Ctx, cmd: &Cmd, args: &[String], what: &str) -> Result<()> {
    let mut c = Command::new("cargo");
    c.args(["run", "-q", "-p", "stratum-e2e", "--bin", GATE_BIN, "--"]);
    c.args(args);
    c.current_dir(ctx.root.as_std_path());
    exec(c, cmd.dry_run, what)
}

fn exec(mut c: Command, dry_run: bool, what: &str) -> Result<()> {
    if dry_run {
        println!("{c:?}");
        return Ok(());
    }
    let status = c.status().with_context(|| format!("running {what}"))?;
    if !status.success() {
        bail!("{what}: failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scenario_letter_outside_the_specification_is_refused() {
        assert!(scenario_filter(Some("f")).is_err());
        assert_eq!(scenario_filter(Some("A")).unwrap(), "scenario_a_");
        assert_eq!(scenario_filter(None).unwrap(), "");
    }

    /// The delegation, not the gate: `stratum_e2e::fence` owns and tests what
    /// the fence *means*, and this file must only be able to ask for it in both
    /// directions without inventing a third spelling.
    #[test]
    fn the_fence_is_asked_for_by_name_in_both_directions() {
        let bin = Utf8PathBuf::from("target/release/stratum-desktop");
        assert_eq!(
            fence_args(&bin, false),
            vec!["fence", "target/release/stratum-desktop"]
        );
        assert_eq!(
            fence_args(&bin, true),
            vec![
                "fence",
                "--require-present",
                "target/release/stratum-desktop"
            ]
        );
    }
}
