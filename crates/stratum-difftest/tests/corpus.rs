//! The corpus phase against the real committed goldens — the acceptance run.
//!
//! These tests need NO Stata and run in ordinary `cargo test --workspace` CI
//! on all three OSes. Every assertion is a counter or an exact value
//! (ADR-017): nothing here measures time.

use pretty_assertions::assert_eq;
use stratum_difftest::corpus::{self, Perturb};
use stratum_difftest::{log, repo_root};

/// The whole corpus compares clean: 26 cases byte-exact against the logs, 3
/// listings under the §17.3 tolerances — with the counters pinned so a case
/// that silently drops out turns this red.
#[test]
fn corpus_is_green_against_the_committed_goldens() {
    let report = corpus::run(&repo_root(), Perturb::None).expect("corpus runs");
    for m in &report.mismatches {
        eprintln!("MISMATCH {m}");
    }
    assert!(report.ok(), "{} mismatches", report.mismatches.len());

    let c = report.counters;
    assert_eq!(c.cases, 26, "the corpus matrix");
    assert_eq!(c.text_blocks, 26, "every case byte-compared");
    assert_eq!(c.listings, 3, "return list + 2 ereturn lists");
    // The three listings, from the committed logs: summarize posts 8 r()
    // scalars; each regress ereturn posts 12 e() scalars.
    assert_eq!(c.scalars, 8 + 12 + 12);
    // OLS posts 10 macros; the robust fit adds e(vcetype).
    assert_eq!(c.macros, 10 + 11);
    // OLS: b, V, beta. Robust: b, V, beta, V_modelbased.
    assert_eq!(c.matrices, 3 + 4);
    // e(sample), once per ereturn listing.
    assert_eq!(c.functions, 2);
    assert!(c.text_bytes > 10_000, "the blocks are real tables");
}

/// The negative test: a deliberately perturbed value is caught. One flipped
/// byte of classic text and one scalar nudged past its tolerance produce
/// exactly two mismatches — no more (the sabotage is surgical), no fewer
/// (the harness can fail).
#[test]
fn a_deliberately_perturbed_value_is_caught() {
    let report = corpus::run(&repo_root(), Perturb::Deliberate).expect("corpus runs");
    assert_eq!(
        report.mismatches.len(),
        2,
        "both perturbations caught, nothing else: {:?}",
        report.mismatches
    );
    let channels: Vec<&str> = report.mismatches.iter().map(|m| m.channel).collect();
    assert!(channels.contains(&"text"), "the flipped byte: {channels:?}");
    assert!(
        channels.contains(&"scalar"),
        "the nudged scalar: {channels:?}"
    );
    for m in &report.mismatches {
        assert_eq!(m.case, "summarize_mpg", "sabotage hits one known case");
    }
}

/// Cross-check the extractor against W05's committed cuts: the block this
/// harness slices out of the log is byte-identical to the `.txt` golden the
/// stats crate compares against. Two independent readings of the corpus that
/// agree to the byte — if either extraction drifts, this is the tripwire.
#[test]
fn extraction_agrees_with_the_stats_crate_goldens() {
    let root = repo_root();
    let mut checked = 0usize;
    for c in corpus::manifest() {
        let log_text = std::fs::read_to_string(root.join(c.log.rel())).expect("read committed log");
        let block = log::command_output(log::body(&log_text), c.echo, c.occurrence)
            .unwrap_or_else(|| panic!("{}: `. {}` not found", c.name, c.echo));
        let txt = root.join(format!("crates/stratum-stats/tests/golden/{}.txt", c.name));
        let golden = std::fs::read_to_string(&txt).unwrap_or_else(|e| panic!("read {txt}: {e}"));
        assert_eq!(block, golden, "{}: extraction differs from {txt}", c.name);
        checked += 1;
    }
    assert_eq!(checked, 26);
}

/// The committed corpus's redacted banner is tolerated: the body starts
/// after `. do`, licence lines and all.
#[test]
fn the_redacted_banner_never_reaches_the_comparison() {
    let root = repo_root();
    for rel in [
        "tests/golden/stata18/core_surface.log",
        "tests/golden/stata18/extended_surface.log",
    ] {
        let text = std::fs::read_to_string(root.join(rel)).expect("read log");
        assert!(
            text.contains("[redacted]"),
            "{rel}: the corpus banner is redacted — that is the shape under test"
        );
        let body = log::body(&text);
        assert!(
            !body.contains("[redacted]"),
            "{rel}: banner leaked into the body"
        );
    }
}

/// The committed goldens ARE the redaction convention: running the live-log
/// redactor over them changes nothing. Pins two things at once — the goldens
/// carry no licence value (they are public), and the placeholder the harness
/// writes into SKIP reasons and mismatch details is the one the goldens
/// already use, so a redacted live excerpt and a golden banner read alike.
#[test]
fn the_goldens_are_a_fixed_point_of_licence_redaction() {
    let root = repo_root();
    let dir = root.join("tests/golden/stata18");
    let mut logs = 0usize;
    for entry in std::fs::read_dir(&dir).expect("read goldens dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_some_and(|e| e == "log") {
            let text = std::fs::read_to_string(&path).expect("read log");
            assert_eq!(
                log::redact_licence(&text),
                text,
                "{}: redaction changed a committed golden",
                path.display()
            );
            logs += 1;
        }
    }
    assert_eq!(logs, 5, "every committed stata18 log was checked");
}

/// `errors.log` ends in `r(111);` and the rc reader believes the log — this
/// is the committed evidence for "never trust the exit code".
#[test]
fn the_final_rc_comes_from_the_log() {
    let root = repo_root();
    let text =
        std::fs::read_to_string(root.join("tests/golden/stata18/errors.log")).expect("read log");
    assert_eq!(log::final_rc(&text), 111);
    let clean = std::fs::read_to_string(root.join("tests/golden/stata18/core_surface.log"))
        .expect("read log");
    assert_eq!(log::final_rc(&clean), 0, "a clean run has no r();");
}
