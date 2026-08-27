//! Turning the scenario scripts into `cargo nextest` cases.
//!
//! Two kinds of test live here, and the difference matters.
//!
//! * **Always on.** Properties of the scenario *scripts* themselves: that they
//!   assert something, that the units they can be blocked on are real units,
//!   that the corpus they read is the committed one. These need no host and run
//!   in the ordinary `cargo nextest run --workspace` on three OSes.
//! * **`--features live`.** The scenarios actually driven against a host. Off by
//!   default because ci.yml's `test` job has no `pnpm install` in it and W17's
//!   packaged app does not exist yet; `cargo xtask e2e --tier 1` and
//!   `.github/workflows/e2e.yml` turn it on, which is where the acceptance
//!   bullets point.
//!
//! Some of the always-on tests are **tripwires**: they pass today *because* a
//! dependency has not landed, and they fail the day it does. That is deliberate,
//! and it is W26's idiom (`d4_is_still_actually_blocked_on_w09`). A harness
//! whose blocked list is prose rots; a harness whose blocked list is a failing
//! test the day the blocker clears does not.
//!
//! Three have now fired, and none was deleted when it did — a tripwire's
//! message is the specification of the work it was waiting for, so the way to
//! retire one is to **invert** it into an assertion that the work is still done:
//!
//! * `w26s_scenario_d_still_cannot_be_compiled_in` fired in repair round 1. W26
//!   re-keyed D.4's predicate, `mod scenario_d;` went into `mod.rs`, and the
//!   guard that replaced it reads the directory
//!   (`every_scenario_file_in_this_directory_is_declared`, in `mod.rs`).
//! * `tier1_still_has_no_packaged_host_to_drive` fired in repair round 3, when
//!   W17's `tauri.conf.json` landed. It is now
//!   `w17s_packaged_host_is_the_one_tier1_drives`, which asserts the three
//!   things its message said were owed.
//! * `tier2_still_has_no_packaged_application_to_launch` fired in the same round
//!   on W22's bundle configs — and its instruction turned out to be **wrong**,
//!   not merely due. `tier2_drives_an_e2e_build_and_not_w22s_bundle` records why
//!   in full: an artifact ADR-011 forbids from carrying the e2e surface is an
//!   artifact tier 2 cannot drive.
//!
//! `the_model_strip_is_still_owed_a_display_string` (a frozen `stratum-proto`)
//! is the one still armed.
//!
//! The two **half-registration guards** further down are not tripwires and must
//! not be written as ones — see the comment above them.

use stratum_e2e::fixtures;
use stratum_e2e::{fence, Capability, ScenarioId};

/// The body of every `key = "..."` entry in a TOML file, whatever column the
/// formatter chose to put the `=` in.
///
/// A scan rather than a `toml` dependency, which is this file's idiom for
/// reading another unit's file (see the two half-registration guards below),
/// but **whitespace-tolerant on purpose**. `taplo fmt --check` is a required PR
/// check and `taplo.toml` is authoritative over every `.toml` in the tree; with
/// `align_entries = false` the padding inside `id = "W12"` is the formatter's to
/// choose. Repair round 2 asserted `contains("id    = \"W12\"")` — the
/// hand-aligned spelling — which set the two gates against each other: with the
/// aligned form on disk `taplo fmt --check` was red, and with taplo's form on
/// disk this test was red. No spelling of the manifest could satisfy both, which
/// is the signature of a test that has pinned formatting instead of content.
///
/// `strip_prefix(key)` before the `=` is what keeps `id` from also matching an
/// `identifier`: the remainder has to start with `=` once trimmed.
fn string_entries<'a>(toml: &'a str, key: &str) -> Vec<&'a str> {
    toml.lines()
        .map(str::trim)
        .filter_map(|line| {
            let rest = line.strip_prefix(key)?.trim_start().strip_prefix('=')?;
            rest.trim_start().strip_prefix('"')?.split('"').next()
        })
        .collect()
}

/// Every capability a step can be blocked on must name a unit that exists in
/// `docs/ownership.toml`. A ledger that says "blocked on W99" tells nobody
/// anything.
#[test]
fn every_blocking_unit_is_a_real_work_unit() {
    let manifest = String::from_utf8(
        fixtures::read_repo_file("docs/ownership.toml").expect("the ownership manifest"),
    )
    .expect("utf-8");
    let units = string_entries(&manifest, "id");

    // A scan that quietly matched nothing would pass this test for every unit
    // forever, which is the failure mode the round-2 spelling had. The manifest
    // states its own owner count in `[meta.counts]`, so the scan is checked
    // against the file's own arithmetic rather than against a number kept here.
    let declared = manifest
        .lines()
        .map(str::trim)
        .find_map(|l| {
            l.strip_prefix("owners")?
                .trim_start()
                .strip_prefix('=')?
                .trim()
                .parse::<usize>()
                .ok()
        })
        .expect("docs/ownership.toml states `owners` in [meta.counts]");
    assert_eq!(
        units.len(),
        declared,
        "the scan found {} `id` entries but the manifest's own [meta.counts] says {declared} \
         owners — one of the two is wrong, and a scan that undercounts makes every \
         assertion below vacuous",
        units.len()
    );

    for cap in [
        Capability::Commands,
        Capability::Keymap,
        Capability::Layout,
        Capability::Settings,
        Capability::Results,
        Capability::History,
        Capability::EventInjection,
        Capability::Editor,
        Capability::Gutter,
        Capability::Cards,
        Capability::Panes,
        Capability::DataEditor,
        Capability::Engine,
    ] {
        let unit = cap.owner();
        assert!(
            units.contains(&unit),
            "{cap:?} is attributed to {unit}, which is not a unit in docs/ownership.toml. \
             It declares: {units:?}"
        );
    }
}

#[test]
fn all_five_scenarios_name_their_specification_section() {
    for s in fixtures::all().expect("the scenarios") {
        assert!(
            s.title.contains("§38-"),
            "scenario {} does not say which acceptance criterion it is: {}",
            s.id,
            s.title
        );
    }
}

/// Scenario E is Scenario A's script run on three platforms and compared. If
/// they ever stop being the same script, the comparison stops meaning
/// "equivalent runtime results" and starts meaning "two different tests agreed".
#[test]
fn scenario_e_is_scenario_a_run_elsewhere() {
    let a = fixtures::scenario_a().expect("A");
    let e = fixtures::scenario_e().expect("E");
    assert_eq!(a.steps, e.steps);
    assert_eq!(e.id, ScenarioId::E);
}

/// Does this TOML declare `key` at the top level of some table? Whitespace- and
/// alignment-tolerant, and it will not mistake `tauri-build` for `tauri`.
fn declares_key(toml: &str, key: &str) -> bool {
    toml.lines().map(str::trim).any(|l| {
        l.strip_prefix(key)
            .is_some_and(|r| r.trim_start().starts_with('='))
    })
}

/// One job's block out of a workflow file, so an assertion about tier 1 cannot
/// be satisfied by a line in tier 2.
///
/// Jobs are the only thing in this file indented by exactly two spaces and
/// starting with a letter: job keys are four, steps are six, and the banner
/// comments start with `#`.
fn workflow_job<'a>(yaml: &'a str, job: &str) -> &'a str {
    let head = format!("\n  {job}:\n");
    let start = yaml
        .find(&head)
        .unwrap_or_else(|| panic!("e2e.yml has no `{job}` job"))
        + head.len();
    let rest = &yaml[start..];
    let end = rest
        .match_indices('\n')
        .find(|(i, _)| {
            let next = &rest[i + 1..];
            next.starts_with("  ") && next.as_bytes().get(2).is_some_and(u8::is_ascii_alphabetic)
        })
        .map_or(rest.len(), |(i, _)| i);
    &rest[..end]
}

/// **W17 has landed, and this is the work its arrival made owed.**
///
/// Through wave 1 this was a tripwire the other way up —
/// `assert!(!tauri.conf.json.is_file())` — whose message listed three things
/// that became owed the day `stratum-desktop` stopped being a placeholder that
/// exits 64. In repair round 3 it fired. Deleting it would throw the list away;
/// inverting it keeps each item asserted, so the wiring cannot come quietly
/// undone:
///
/// 1. `apps/desktop/src-tauri` **compiles** `e2e_cmds.rs` — a plain
///    `mod e2e_cmds;`, NOT `#[cfg(feature = "e2e")] mod e2e_cmds;`, which would
///    leave the ~450 non-Tauri lines and their four tests compiled by nothing.
///    The gate belongs on the inner `tauri_surface` module, where it is. The
///    crate also has to name `tauri` itself, or `--features e2e` fails to
///    resolve the surface it gates.
/// 2. `.github/workflows/e2e.yml`'s **tier-1 job** builds that host with
///    `--features e2e` and hands the binary to the harness, rather than driving
///    the node bridge on all three OSes.
/// 3. The bridge is no longer the default host. `tier1::default_host` prefers an
///    e2e-capable binary and `tier1::tests::a_binary_is_only_a_host_if_it_was_
///    built_with_the_e2e_feature` pins what "capable" means; the bridge stays as
///    the browser-tab development path, which is why e2e.yml still installs it.
///    Asserted there rather than restated here.
///
/// The `e2e` feature's own existence is asserted beside `mod e2e_cmds;` by
/// `the_host_bridge_feature_and_its_module_arrive_together`, and is not repeated.
#[test]
fn w17s_packaged_host_is_the_one_tier1_drives() {
    let read = |p: &str| {
        String::from_utf8(fixtures::read_repo_file(p).unwrap_or_else(|e| panic!("{p}: {e}")))
            .expect("utf-8")
    };
    let main = read("apps/desktop/src-tauri/src/main.rs");
    let manifest = read("apps/desktop/src-tauri/Cargo.toml");
    let workflow = read(".github/workflows/e2e.yml");

    // 1. Declared, and declared unconditionally. The nearest preceding line that
    // is neither blank nor a comment is the one that would carry a `#[cfg]`.
    let lines: Vec<&str> = main.lines().map(str::trim).collect();
    let at = lines
        .iter()
        .position(|l| *l == "mod e2e_cmds;")
        .expect("apps/desktop/src-tauri/src/main.rs no longer declares `mod e2e_cmds;`");
    let guard = lines[..at]
        .iter()
        .copied()
        .rev()
        .find(|l| !l.is_empty() && !l.starts_with("//"));
    assert!(
        !guard.is_some_and(|l| l.starts_with("#[cfg")),
        "`mod e2e_cmds;` is gated by `{}`. Measured in repair round 1: with the module \
         declared unconditionally and the feature off, neither fenced name reaches the \
         debug or the release binary — so the cfg buys nothing and costs ~450 lines and \
         four tests that then compile in no configuration anybody runs. The gate belongs \
         on `e2e_cmds::tauri_surface`.",
        guard.unwrap_or_default()
    );
    assert!(
        declares_key(&manifest, "tauri"),
        "apps/desktop/src-tauri/Cargo.toml declares the `e2e` feature but not `tauri`, \
         so `cargo build -p stratum-desktop --features e2e` cannot resolve \
         `e2e_cmds::tauri_surface` and every job in e2e.yml that builds it is red."
    );

    // 2. Tier 1 drives that binary. Scoped to the job, because tier2 and fence
    // build with the same feature and would satisfy a whole-file `contains`
    // while tier 1 quietly went on running the node bridge.
    let tier1 = workflow_job(&workflow, "tier1");
    // The slicer's own self-check, as a bound on the slice rather than on its
    // prose: a `workflow_job` that silently ran to the end of the file would
    // make both assertions below true for the wrong reason, since tier 2 and the
    // fence build the host with the same feature.
    let tier1_at = workflow.find("\n  tier1:\n").expect("the tier-1 job");
    let tier2_at = workflow.find("\n  tier2:\n").expect("the tier-2 job");
    assert!(
        !tier1.is_empty() && tier1_at < tier2_at && tier1.len() < tier2_at - tier1_at,
        "the tier-1 slice is {} bytes and reaches past the tier-2 job {} bytes away — \
         the job boundaries in e2e.yml are no longer what `workflow_job` assumes",
        tier1.len(),
        tier2_at.saturating_sub(tier1_at)
    );
    assert!(
        tier1.contains("cargo build -p stratum-desktop --features e2e"),
        "e2e.yml's tier-1 job no longer builds the packaged host with `--features e2e`, \
         so it is driving the pre-host bridge on all three OSes. The bridge advertises no \
         editor, no cards and no panes; a green tier 1 against it is not the acceptance \
         bullet it is reported as."
    );
    assert!(
        tier1.contains("STRATUM_E2E_APP"),
        "e2e.yml's tier-1 job builds the packaged host and never hands it to the harness. \
         `tier1::default_host` reads STRATUM_E2E_APP first and `xtask e2e --tier 1 --app` \
         sets it; without either, the run falls back to whatever is in target/debug."
    );
}

/// **W22 has landed, and the answer is not the one this test used to prescribe.**
///
/// Until repair round 3 this was the tier-2 twin of the tripwire above: it
/// asserted `tauri.macos.conf.json` did not exist and told whoever tripped it to
/// "point `xtask e2e --tier 2 --app <path>` at the packaged artifact". W22's
/// per-OS bundle configs landed and it fired. **Following it literally would
/// break ADR-011**, so it is recorded here rather than obeyed.
///
/// Tier 2 reaches the app through `__STRATUM_E2E__`, and
/// `apps/desktop/src/e2e/webview.ts` installs that global only when the host
/// emits an e2e request — which only a build with `--features e2e` ever does.
/// The artifact W22 packages is, by the fence's whole premise, the build that
/// does *not* carry it: the `fence` job below asserts `cargo build -p
/// stratum-desktop --release` is clean, and W25's acceptance bullet asks
/// `smoke.yml` to assert the same of the shipped binary. A tier 2 aimed at a
/// bundle would be a tier 2 with no bridge to talk to — or, worse, an argument
/// for bundling the surface.
///
/// So tier 2 drives what W25's §Design in `IMPLEMENTATION_PLAN` says it drives:
/// real `tauri-driver` input into an `--features e2e` build. What W22's arrival
/// actually owes tier 2 is nothing; what it owes ADR-011 is the `smoke.yml` half
/// of the fence, which is W22's file and not W25's.
///
/// Asserted rather than left as prose so that "upgrade tier 2 to the bundle",
/// read off the old instruction, is a failing test rather than a plausible idea.
#[test]
fn tier2_drives_an_e2e_build_and_not_w22s_bundle() {
    let workflow = String::from_utf8(
        fixtures::read_repo_file(".github/workflows/e2e.yml").expect("this workflow"),
    )
    .expect("utf-8");

    let tier2 = workflow_job(&workflow, "tier2");
    assert!(
        tier2.contains("cargo build -p stratum-desktop --features e2e"),
        "e2e.yml's tier-2 job no longer builds the host with `--features e2e`. Without \
         that feature the host never emits an e2e request, `webview.ts` never installs \
         `__STRATUM_E2E__`, and every `executeScript` in `stratum_e2e::tier2` returns \
         undefined against an app that looks perfectly healthy."
    );
    assert!(
        !tier2.contains("bundle/") && !tier2.contains("--release"),
        "e2e.yml's tier-2 job is being pointed at a packaged or release artifact. That \
         artifact must have no e2e commands — the `fence` job in this same file asserts \
         it — so it has no bridge for tier 2 to reach. See this test's doc comment."
    );

    // The claim above ("the packaged build does not carry the surface") is only
    // worth anything while something still checks it.
    let fence = workflow_job(&workflow, "fence");
    assert!(
        fence.contains("cargo build -p stratum-desktop --release"),
        "the fence job no longer builds a release binary, so nothing in this workflow \
         asserts that the shape W22 packages is free of the e2e commands — which is the \
         premise the test above rests on."
    );
}

/// ADR-011's fence looks for two byte strings, and they are worth exactly as
/// much as their agreement with the names the host actually registers.
///
/// Repair round 3 gave that agreement a compiler:
/// `stratum_e2e::host::tests::the_fence_greps_for_exactly_what_the_host_registers`
/// compares the two constants as values, because `src/host.rs` now compiles
/// `e2e_cmds.rs` into this crate. This test is not redundant beside it. It reads
/// the source *text*, so it is the half that catches a **third** `pub const
/// E2E_*` being added to the host and not to `FENCED_COMMANDS` — a new fenced
/// command that no equality assertion can see, and the exact shape of a
/// test-only IPC command shipping unnoticed.
#[test]
fn the_fence_and_the_host_agree_on_the_command_names() {
    let src = String::from_utf8(
        fixtures::read_repo_file("apps/desktop/src-tauri/src/e2e_cmds.rs").expect("the host half"),
    )
    .expect("utf-8");

    // `pub const E2E_DISPATCH: &str = "e2e_dispatch";` — the two command-name
    // constants, and deliberately not `PORT_ENV`/`HOST_ENV`/`REQUEST_EVENT`,
    // which are not Tauri commands and are not fenced.
    let mut declared: Vec<String> = src
        .lines()
        .filter(|l| l.starts_with("pub const E2E_") && l.contains(": &str = \""))
        .filter_map(|l| l.split('"').nth(1).map(str::to_owned))
        .collect();
    declared.sort();

    let mut fenced: Vec<String> = fence::FENCED_COMMANDS
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    fenced.sort();

    assert_eq!(
        declared, fenced,
        "the host's command names and stratum_e2e::fence::FENCED_COMMANDS have drifted. \
         A fence that greps for a name no build emits passes on every binary forever, \
         which is ADR-011's failure mode dressed as a green tick."
    );
}

// ---------------------------------------------------------------------------
// Half-registration guards
//
// W25 owns two source files that live inside other units' module trees:
// `xtask/src/e2e.rs` (W00's crate root) and
// `apps/desktop/src-tauri/src/e2e_cmds.rs` (W17's). Both are compiled by
// nothing at HEAD, which is the defect repair round 1 reports and round 2
// contains rather than cures: the fence and the §38-E comparison moved into
// `stratum_e2e::{fence,compare}`, so what is left uncompiled in
// `xtask/src/e2e.rs` is clap plumbing that shells out, and the ~450 non-Tauri
// lines of `e2e_cmds.rs` are still owed a `mod` line by W17.
//
// These two tests are NOT tripwires — they do not fail when the registration
// lands, which would punish whoever fixes it. They fail only on a *partial*
// registration, which is a silent failure mode: a `mod` line without its
// dispatch arm compiles the module and still leaves the subcommand missing, and
// an `e2e` feature without the module gates nothing while making
// `--features e2e` in e2e.yml look meaningful. Both pass today (neither half is
// present) and both pass after a correct fix.
// ---------------------------------------------------------------------------

/// `mod e2e;` alone is not the registration. The subcommand exists only when the
/// `Cmd` variant and the dispatch arm land with it.
#[test]
fn the_xtask_subcommand_is_registered_all_three_lines_or_none() {
    let main = String::from_utf8(
        fixtures::read_repo_file("xtask/src/main.rs").expect("xtask's crate root"),
    )
    .expect("utf-8");

    // Deliberately loose on the last two. A guard that insists on one exact
    // spelling of a match arm reports a false half-registration the first time
    // rustfmt or the author breaks the line differently, which is a guard that
    // gets deleted rather than obeyed. What is actually being asserted is
    // "`e2e::run` is reachable from the dispatch", and `e2e::run(` says that.
    let declared = main.lines().any(|l| l.trim() == "mod e2e;");
    let variant = main.contains("E2e(e2e::Cmd)");
    let dispatched = main.contains("e2e::run(");

    assert!(
        (declared && variant && dispatched) || !(declared || variant || dispatched),
        "xtask/src/e2e.rs is half-registered in xtask/src/main.rs — \
         mod={declared}, Cmd variant={variant}, dispatch arm={dispatched}. \
         All three lines are listed in the header of xtask/src/e2e.rs; any \
         proper subset either fails to compile or leaves `cargo xtask e2e` \
         missing while looking wired up."
    );
}

/// The `e2e` cargo feature and the module it gates have to arrive together.
#[test]
fn the_host_bridge_feature_and_its_module_arrive_together() {
    let manifest = String::from_utf8(
        fixtures::read_repo_file("apps/desktop/src-tauri/Cargo.toml").expect("W17's manifest"),
    )
    .expect("utf-8");
    let main = String::from_utf8(
        fixtures::read_repo_file("apps/desktop/src-tauri/src/main.rs").expect("W17's crate root"),
    )
    .expect("utf-8");

    let feature = manifest
        .lines()
        .any(|l| l.trim_start().starts_with("e2e") && l.contains('='));
    let declared = main.lines().any(|l| l.trim().ends_with("mod e2e_cmds;"));

    assert_eq!(
        feature, declared,
        "apps/desktop/src-tauri is half-wired for the e2e bridge — \
         `e2e` feature in Cargo.toml={feature}, `mod e2e_cmds;` in main.rs={declared}. \
         A feature with no module behind it makes `cargo build --features e2e` \
         (which e2e.yml's tier2 and fence jobs run) prove nothing; a module with no \
         feature makes the Tauri surface unreachable. See the header of e2e_cmds.rs."
    );
}

/// Scenario A's estimation card cannot show 06 §6.4's model strip, and this is
/// the tripwire that hands the assertion back when it can.
///
/// 06 §6.4 draws the strip as `N 74 · F(3,70) · Prob>F · R² · Adj R² · Root MSE`.
/// Today W14's card draws `N` and the two degrees of freedom and stops, because
/// those are the only ones that arrive as *display strings*:
/// `EstimationPayload.scalars` is `Vec<(String, f64)>` and A6 gave display
/// siblings to `Term.display_num`, `SummarizeDetail.display_*` and
/// `AnovaTable.display` without reaching it. Formatting an `f64` in TypeScript
/// is the one thing A6 exists to forbid, so the renderer prints nothing and
/// escalates — see the header of `apps/desktop/src/renderers/estimation/
/// index.tsx`. `stratum-proto` is FROZEN (R1); this is reported, not patched.
///
/// So `fixtures::scenario_a()` asserts the golden `foreign` coefficient in the
/// rendered card and pins `R-squared` on the raw classic text, where it is
/// genuinely written. The moment the contract grows a display sibling for
/// `scalars`, this test goes red and names the step to restore. That is the
/// difference between a known gap and a forgotten one.
#[test]
fn the_model_strip_is_still_owed_a_display_string() {
    let result = String::from_utf8(
        fixtures::read_repo_file("crates/stratum-proto/src/result.rs").expect("the contract"),
    )
    .expect("utf-8");

    // Anything that pairs a display string with `scalars` closes this. Matched
    // loosely on purpose: the field's final name is the contract owner's to
    // choose, and a tripwire keyed to one guessed spelling never fires.
    let landed = result.contains("display_scalars")
        || result.contains("ScalarDisplay")
        || (result.contains("scalars") && result.contains("scalars_display"));

    assert!(
        !landed,
        "stratum-proto now carries display strings for e()' scalars, so W14's card \
         can draw 06 §6.4's model strip. Restore the model-strip assertion on \
         Scenario A step 5 in crates/stratum-e2e/src/fixtures.rs — \
         `Expect::CardBodyContains(2, \"R²\")` — and delete this test. It was \
         removed only because the number could not be rendered without \
         reimplementing fmt_g in TypeScript (A6)."
    );
}

// ---------------------------------------------------------------------------
// The live tier-1 runs
// ---------------------------------------------------------------------------

#[cfg(feature = "live")]
mod live {
    use stratum_e2e::snapshot::{Field, Snapshot, What};
    use stratum_e2e::tier1::{default_host, Tier1Driver};
    use stratum_e2e::{fixtures, run_scenario, Counters, Driver, RunOptions, Scenario};

    /// **The expiry on the blocked ledger.**
    ///
    /// Every `Unavailable` field names a `witness`: the repo-relative path whose
    /// absence is the claim. If that file exists, the claim has expired and the
    /// host is under-reporting the tree.
    ///
    /// This is the check that was missing through wave 1. The bridge said W13
    /// owed `doc` and `gutter` — "no editor is mounted" — and W14 owed `cards` —
    /// "none is written" — for a whole wave *after both units shipped*, and
    /// nothing anywhere went red, because prose cannot expire. Every scenario
    /// below runs it, so a stale claim fails the same run that prints the
    /// ledger rather than surviving to the next review.
    fn assert_no_expired_claims(snap: &Snapshot) {
        let root = fixtures::repo_root().expect("repo root");
        let mut expired: Vec<String> = Vec::new();
        let mut checked = 0u32;

        // One closure per field because each `Field<T>` is a different type.
        macro_rules! check {
            ($($field:ident),+ $(,)?) => {$(
                if let Field::Unavailable { unit, why, witness } = &snap.$field {
                    checked += 1;
                    if root.join(witness).exists() {
                        expired.push(format!(
                            "  `{}` is reported owed by {unit} ({why}), but {witness} exists",
                            stringify!($field)
                        ));
                    }
                }
            )+};
        }
        check!(doc, gutter, results, cards, panes, focus, layout, history, blocks);

        // `panes[].content` is a second layer of the same claim.
        if let Field::Present(panes) = &snap.panes {
            for pane in panes {
                if let Field::Unavailable { unit, why, witness } = &pane.content {
                    checked += 1;
                    if root.join(witness).exists() {
                        expired.push(format!(
                            "  pane `{}` is reported owed by {unit} ({why}), but {witness} exists",
                            pane.id
                        ));
                    }
                }
            }
        }

        assert!(
            expired.is_empty(),
            "the host's blocked ledger has expired claims — the unit landed and the \
             bridge is still reporting it as owing the field:\n{}\n\
             Wire the field to that unit's real module (apps/desktop/src/e2e/bridge.ts; \
             it has a DOM, see dom.ts) or change the witness to name what is actually \
             still missing. {checked} claim(s) were checked.",
            expired.join("\n")
        );
    }

    fn drive(scenario: &Scenario) -> (stratum_e2e::ScenarioReport, Counters) {
        let host = default_host().expect("a host to drive");
        let mut driver = Tier1Driver::launch(host).expect("the host started and shook hands");
        let report = run_scenario(&mut driver, scenario, RunOptions::default());
        let counters = driver.counters();
        println!("{}", report.render());

        // Every field, so a claim is checked whether or not this scenario reads
        // it. One extra round trip, taken after the scenario is scored so it
        // cannot affect the counters `assert_counters` asserts on.
        let ledger = driver
            .snapshot(&What::all())
            .expect("a final snapshot for the expiry check");
        assert_no_expired_claims(&ledger);

        // Spec §38-E: `xtask e2e --compare` diffs these across the three OSes.
        if let Some(dir) = std::env::var_os("STRATUM_E2E_TRANSCRIPT_DIR") {
            let dir = std::path::PathBuf::from(dir);
            std::fs::create_dir_all(&dir).expect("the transcript directory");
            std::fs::write(
                dir.join(format!("scenario_{}.transcript", report.id)),
                report.transcript(),
            )
            .expect("writing the transcript");
        }

        // `xtask e2e --tier 1 --require-complete` is the M4/M5 gate: it is not
        // enough that nothing failed, nothing may be blocked either.
        if std::env::var_os("STRATUM_E2E_REQUIRE_COMPLETE").is_some() {
            assert!(
                report.is_complete(),
                "--require-complete: scenario {} still has blocked steps\n{}",
                report.id,
                report.render()
            );
        }
        (report, counters)
    }

    /// ADR-017. The plan's "< 3 s per scenario" restated as the counters that
    /// cause it: one round trip per dispatch, one per snapshot, one handshake,
    /// and nothing else. Duration is printed by `render()`, never asserted.
    fn assert_counters(c: Counters, steps_dispatched: u32) {
        assert_eq!(
            c.sleeps, 0,
            "a harness that sleeps has a wall-clock dependency baked into its result"
        );
        assert_eq!(
            c.polls, 0,
            "re-asking until the answer changes is the same bug in a loop"
        );
        assert_eq!(
            c.round_trips,
            c.dispatches + c.snapshots + 1,
            "one round trip per dispatch, one per snapshot, one handshake"
        );
        assert!(
            c.dispatches <= steps_dispatched,
            "no step may be dispatched twice"
        );
    }

    #[test]
    fn scenario_a_notebook_like_analysis() {
        let s = fixtures::scenario_a().expect("A");
        let steps = u32::try_from(s.steps.len()).unwrap();
        let (report, counters) = drive(&s);
        assert!(report.is_green(), "{}", report.render());
        assert_counters(counters, steps);
        assert!(
            report.passed() > 0,
            "a scenario in which nothing ran proves nothing:\n{}",
            report.render()
        );
    }

    #[test]
    fn scenario_b_stale_state() {
        let s = fixtures::scenario_b().expect("B");
        let steps = u32::try_from(s.steps.len()).unwrap();
        let (report, counters) = drive(&s);
        assert!(report.is_green(), "{}", report.render());
        assert_counters(counters, steps);
    }

    #[test]
    fn scenario_c_classic_workflow() {
        let s = fixtures::scenario_c().expect("C");
        let steps = u32::try_from(s.steps.len()).unwrap();
        let (report, counters) = drive(&s);
        assert!(report.is_green(), "{}", report.render());
        assert_counters(counters, steps);
    }

    #[test]
    fn scenario_d_interoperability_at_the_app_level() {
        let s = fixtures::scenario_d().expect("D");
        let (report, _) = drive(&s);
        assert!(report.is_green(), "{}", report.render());
    }

    #[test]
    fn scenario_e_cross_platform() {
        let s = fixtures::scenario_e().expect("E");
        let (report, _) = drive(&s);
        assert!(report.is_green(), "{}", report.render());
    }

    /// **M4, and it is now a true sentence.**
    ///
    /// Through wave 1 this was a tripwire the other way up — `assert!(!report
    /// .is_complete())`, because the pre-host bridge advertised no editor, no
    /// cards and no panes and Scenario A was *green* without being *complete*.
    /// In repair round 3 it fired: W13's editor and W14's cards are mounted by
    /// the bridge, and A runs 8 of 8 with nothing blocked and nothing partial.
    ///
    /// The tripwire's own instruction was "say so, delete this test, and turn on
    /// `--require-complete`". Deleting it outright would throw away the
    /// statement; inverting it keeps M4 asserted, so a later change that makes
    /// any step of Scenario A blocked again is a failing test rather than a
    /// quietly shorter report. `.github/workflows/e2e.yml` passes
    /// `--require-complete` for A, D and E for the same reason.
    ///
    /// B and C are deliberately NOT here: their remaining steps are blocked on
    /// W09's engine, W16's command bar and W18's Data Editor, and a red build
    /// for work nobody has started is how people learn to ignore red.
    #[test]
    fn scenario_a_is_complete_not_merely_green() {
        for (id, s) in [
            ("A", fixtures::scenario_a().expect("A")),
            ("D", fixtures::scenario_d().expect("D")),
            ("E", fixtures::scenario_e().expect("E")),
        ] {
            let (report, _) = drive(&s);
            assert!(report.is_green(), "{}", report.render());
            assert!(
                report.is_complete(),
                "scenario {id} has gone back to having blocked steps. It ran end to end \
                 in repair round 3, so this is a regression in the host or in a unit the \
                 host reads, not work that has not started.\n{}",
                report.render()
            );
        }
    }
}
