//! **Scenario D — interoperability.** Spec §38-D, plan §2 row D, plan W26.
//!
//! > save `.do` → inspect in a plain text editor → verify no embedded
//! > proprietary notebook data → run through the runtime → where applicable test
//! > in local licensed Stata.
//!
//! This is the acceptance scenario for the product's central promise: a
//! researcher's analysis never becomes trapped in our format. It was owned by
//! **nobody** before the audit (A32), which is why the mechanism enforcing
//! ADR-010 was unimplemented and untested. It is W26's now.
//!
//! # Scope, and what is deferred to whom
//!
//! Plan §2 row D lists five obligations across four units. The three that are
//! properties of the *written bytes* are here and run today:
//!
//! * the saved bytes contain no JSON, no base64, and no sidecar marker, and
//!   round-trip through a plain text editor;
//! * **a CRLF file with a BOM saves back byte-identical** (A24);
//! * `section_rename` and `section_move` produce byte diffs that pass their
//!   gates.
//!
//! The remaining two need binaries that do not exist yet and are marked
//! `#[ignore]` with the reason on the test:
//!
//! * `stratum run analysis.do --json` produces a well-framed stream — **W09**,
//!   and specifically W09's *engine edge*, not its `run` subcommand;
//! * `xtask difftest` agrees with StataMP on the golden set — **W23**, and
//!   self-hosted only (spec §32: no CI job may reach Stata).
//!
//! **So "Scenario D passes" is true of D.1–D.3 and not yet of D.4.** Saying it
//! plainly here because an ignored test reads like a passing one in a summary.
//! `d4_is_still_actually_blocked_on_w09` is a live test that fails the moment
//! there is a `stratum run` that can actually run something, so the gap closes
//! itself out loud instead of rotting. See `d4_unblocked_because` — the
//! predicate has now been wrong twice, and both times in the same direction.
//!
//! # Where this file is compiled
//!
//! Twice, on purpose. `crates/stratum-workspace/tests/scenario_d.rs` includes it
//! with `#[path]`, and so does W25's `tests/e2e/mod.rs` — the latter only since
//! repair round 1, because W25 held the `mod scenario_d;` line back for as long
//! as the tripwire below was mis-keyed and red. This file therefore depends on
//! `stratum_workspace`, `stratum_proto`, `camino` and `std` only — nothing from
//! the e2e harness — which is what let that registration be one line rather than
//! a rewrite, and what keeps both copies the same bytes instead of two drifting
//! forks of an acceptance test.
//!
//! # Who edits this file
//!
//! W26 — `docs/ownership.toml` names it sole owner, and every change since
//! d8f1779 was made by W26 in its own repair rounds. Written down because
//! `cargo xtask ownership` cannot establish it: the check reads the manifest and
//! `git ls-files`, so it proves a file has exactly *one* owner and never that
//! the owner is who touched it. That gap turned a legitimate edit into an
//! escalation once already. W25's `tests/e2e/mod.rs` records the same change
//! from its side, and the pair is the whole story: W26 re-keyed the tripwire
//! here, W25 then added its own one-line `mod scenario_d;` there. Neither unit
//! wrote a byte in the other's file.

use camino::{Utf8Path, Utf8PathBuf};
use stratum_proto::{Edit, SectionId, Span};
use stratum_workspace::bytes::{DocBytes, Eol};
use stratum_workspace::keymap::KeymapStore;
use stratum_workspace::layout::LayoutStore;
use stratum_workspace::project::Project;
use stratum_workspace::write::{EditGate, StandaloneGate};
use stratum_workspace::Workspace;

/// The analysis a researcher actually writes, in the encoding Stata for Windows
/// actually produces: UTF-8 BOM, CRLF, `// %%` section markers, a `///`
/// continuation, a `#delimit ;` stretch and a `/* … */` block.
const ANALYSIS: &str = "\
// %% Load
sysuse auto, clear
label var price \"Price in dollars\"

// %% Clean
drop if price > 15000
gen lprice = log(price) ///
    if price < .

/* A block comment, because real do-files have them
   and a naive writer eats them. */
#delimit ;
summarize
  price mpg weight;
#delimit cr

// %% Model
regress lprice mpg weight
";

fn windows_bytes(text: &str) -> Vec<u8> {
    let mut v = b"\xef\xbb\xbf".to_vec();
    v.extend_from_slice(text.replace('\n', "\r\n").as_bytes());
    v
}

struct Fixture {
    _tmp: tempfile::TempDir,
    root: Utf8PathBuf,
    path: Utf8PathBuf,
    ws: Workspace,
}

fn fixture(raw: &[u8]) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let path = root.join("analysis.do");
    std::fs::write(&path, raw).unwrap();
    let ws = Workspace::new(
        Project::load(&root).unwrap(),
        LayoutStore::new(root.join("resources/layouts"), root.join("config/layouts")),
        KeymapStore::new(root.join("resources/keymaps"), root.join("config/keymaps")),
    );
    Fixture {
        _tmp: tmp,
        root,
        path,
        ws,
    }
}

// ---------------------------------------------------------------------------
// D.1 — no embedded proprietary notebook data
// ---------------------------------------------------------------------------

/// Every marker a notebook format leaves behind, and why each one is checked.
///
/// The list is deliberately over-broad: it is cheaper to explain a false
/// positive than to discover in year two that the file has grown a metadata
/// block nobody noticed.
const FORBIDDEN_MARKERS: &[(&str, &str)] = &[
    ("\"cells\"", "the .ipynb cell array"),
    ("\"nbformat\"", "the .ipynb version stamp"),
    ("\"outputs\"", "output embedded beside code — spec §6"),
    ("\"execution_count\"", "an execution id in the source"),
    ("\"metadata\"", "a notebook metadata block"),
    ("base64", "an encoded blob of anything"),
    ("data:image/", "an inline image"),
    ("<smcl>", "SMCL log output"),
    ("STRATUM-BEGIN", "a fenced region only we can read"),
    ("stratum:", "a private directive"),
    ("%%stratum", "a magic-comment escape hatch"),
    ("\\u0000", "an escaped NUL, i.e. binary in disguise"),
];

#[test]
fn d1_the_saved_do_file_contains_no_embedded_notebook_data() {
    let mut f = fixture(ANALYSIS.as_bytes());
    let opened = f.ws.doc_open(&f.path).unwrap();

    // Exercise everything that writes to the sidecar, so the test is not
    // vacuously true on an empty workspace.
    f.ws.sidecar_patch(
        opened.doc,
        stratum_workspace::sidecar_durable::DurableSidecarPatch {
            collapsed: Some(vec![stratum_proto::CodeHash([7; 16])]),
            doc_view: Some(Some(true)),
            inline_results: Some(Some(stratum_proto::InlineResultsMode::Compact)),
            ..Default::default()
        },
    )
    .unwrap();
    f.ws.cache_update(opened.doc, |c| c.scroll_line = 40)
        .unwrap();
    f.ws.section_rename(opened.doc, SectionId(1), "Clean and transform")
        .unwrap();
    f.ws.doc_save(opened.doc).unwrap();

    let raw = std::fs::read(&f.path).unwrap();
    let text = String::from_utf8(raw.clone()).expect("the .do file is still plain UTF-8 text");

    for (marker, why) in FORBIDDEN_MARKERS {
        assert!(
            !text.contains(marker),
            "the .do file contains {marker:?} ({why}) — ADR-010 / spec §6"
        );
    }
    // Nothing binary: every byte is printable, tab, or a line terminator.
    assert!(
        raw.iter()
            .all(|&c| c >= 0x20 || c == b'\t' || c == b'\n' || c == b'\r' || c >= 0x80),
        "the .do file contains control bytes"
    );
    // The file is still the researcher's program, statement for statement.
    for stmt in [
        "sysuse auto, clear",
        "drop if price > 15000",
        "regress lprice mpg weight",
        "#delimit ;",
    ] {
        assert!(text.contains(stmt), "{stmt} did not survive a save");
    }
    // And the code the runtime sees is provably identical to what we opened.
    StandaloneGate
        .assert_comment_only(ANALYSIS, &text)
        .expect("a save plus a section rename is a comment-only change");
}

#[test]
fn d1b_everything_stratum_knows_that_is_not_code_lives_outside_the_do_file() {
    let mut f = fixture(ANALYSIS.as_bytes());
    let opened = f.ws.doc_open(&f.path).unwrap();
    f.ws.sidecar_patch(
        opened.doc,
        stratum_workspace::sidecar_durable::DurableSidecarPatch {
            collapsed: Some(vec![stratum_proto::CodeHash([1; 16])]),
            ..Default::default()
        },
    )
    .unwrap();
    f.ws.cache_update(opened.doc, |c| c.scroll_line = 7)
        .unwrap();
    f.ws.doc_save(opened.doc).unwrap();

    // Exactly three artifacts, and only one of them is the source (C19).
    // The walk yields host separators; the assertions below spell paths with
    // `/`. No file this fixture creates has a `\` in its NAME, so folding
    // separators is lossless on every host (Windows is the one that has them).
    let mut found: Vec<String> = walk(&f.root)
        .into_iter()
        .map(|p| p.strip_prefix(&f.root).unwrap().as_str().replace('\\', "/"))
        .collect();
    found.sort();
    assert!(found.contains(&"analysis.do".to_owned()));
    assert!(found.contains(&".analysis.do.workspace".to_owned()));
    // The volatile tree ignores itself, so a clone never carries it.
    assert!(found.iter().any(|p| p == ".stratum/.gitignore"));
    assert_eq!(
        std::fs::read_to_string(f.root.join(".stratum/.gitignore")).unwrap(),
        "*\n"
    );
    // Nothing else at all — no lock file, no index, no project database.
    let unexpected: Vec<&String> = found
        .iter()
        .filter(|p| {
            *p != "analysis.do" && *p != ".analysis.do.workspace" && !p.starts_with(".stratum/")
        })
        .collect();
    assert!(unexpected.is_empty(), "{unexpected:?}");
}

// ---------------------------------------------------------------------------
// D.2 — a CRLF file with a BOM saves back byte-identical (A24)
// ---------------------------------------------------------------------------

#[test]
fn d2_a_crlf_file_with_a_bom_saves_back_byte_identical() {
    let raw = windows_bytes(ANALYSIS);
    let mut f = fixture(&raw);
    let opened = f.ws.doc_open(&f.path).unwrap();
    assert_eq!(
        opened.bytes,
        DocBytes {
            eol: Eol::Crlf,
            bom: true
        }
    );
    assert!(
        opened.diagnostics.is_empty(),
        "a uniformly-CRLF file must not raise L013: {:?}",
        opened.diagnostics
    );

    f.ws.doc_save(opened.doc).unwrap();
    assert_eq!(
        std::fs::read(&f.path).unwrap(),
        raw,
        "opening and saving a Windows do-file must not touch a single byte"
    );
}

#[test]
fn d2b_editing_one_line_of_a_crlf_file_diffs_one_line() {
    let raw = windows_bytes(ANALYSIS);
    let mut f = fixture(&raw);
    let opened = f.ws.doc_open(&f.path).unwrap();

    let old = "regress lprice mpg weight";
    let at = opened.text.find(old).unwrap() as u32;
    f.ws.doc_change(
        opened.doc,
        opened.version,
        &[Edit {
            span: Span {
                start: at,
                end: at + old.len() as u32,
            },
            text: "regress lprice mpg weight foreign".to_owned(),
        }],
    )
    .unwrap();
    f.ws.doc_save(opened.doc).unwrap();

    let after = std::fs::read(&f.path).unwrap();
    assert_eq!(
        differing_lines(&raw, &after),
        1,
        "a one-line edit to a CRLF file must not rewrite the file"
    );
    assert!(after.starts_with(b"\xef\xbb\xbf"));
    assert_eq!(
        after.iter().filter(|&&c| c == b'\r').count(),
        raw.iter().filter(|&&c| c == b'\r').count()
    );
}

#[test]
fn d2c_a_latin1_do_file_is_refused_and_left_alone() {
    // The other half of A24: we never lossily transcode a researcher's source.
    let mut raw = b"sysuse auto\nlabel var price \"Prix (".to_vec();
    raw.push(0xa3); // `£` in latin-1
    raw.extend_from_slice(b")\"\n");

    let mut f = fixture(&raw);
    let failure = f.ws.doc_open(&f.path).unwrap_err();
    assert_eq!(failure.diagnostic().unwrap().code, "STRATUM0601");
    assert_eq!(std::fs::read(&f.path).unwrap(), raw);

    let ro = f.ws.doc_open_read_only(&f.path).unwrap();
    assert!(ro.read_only);
    assert!(f.ws.doc_save(ro.doc).is_err());
    assert_eq!(std::fs::read(&f.path).unwrap(), raw);
}

// ---------------------------------------------------------------------------
// D.3 — section_rename and section_move produce gated byte diffs
// ---------------------------------------------------------------------------

#[test]
fn d3_section_rename_is_a_one_line_diff_that_passes_its_gate() {
    let raw = windows_bytes(ANALYSIS);
    let mut f = fixture(&raw);
    let opened = f.ws.doc_open(&f.path).unwrap();
    let before_text = opened.text.clone();

    f.ws.section_rename(opened.doc, SectionId(2), "Wage model")
        .unwrap();

    let after = std::fs::read(&f.path).unwrap();
    assert_eq!(differing_lines(&raw, &after), 1);
    let after_text = f.ws.document(opened.doc).unwrap().text.clone();
    assert!(after_text.contains("// %% Wage model\n"));
    StandaloneGate
        .assert_comment_only(&before_text, &after_text)
        .expect("a rename must be provably comment-only");
    // Byte fidelity survives a rename too.
    assert!(after.starts_with(b"\xef\xbb\xbf"));
    assert_eq!(
        after.iter().filter(|&&c| c == b'\r').count(),
        raw.iter().filter(|&&c| c == b'\r').count()
    );
}

#[test]
fn d3b_section_move_reorders_statements_and_passes_its_gate() {
    let raw = windows_bytes(ANALYSIS);
    let mut f = fixture(&raw);
    let opened = f.ws.doc_open(&f.path).unwrap();
    let before_text = opened.text.clone();

    // Move the Model section above Clean.
    f.ws.section_move(opened.doc, SectionId(2), Some(SectionId(1)), None)
        .unwrap();

    let after_text = f.ws.document(opened.doc).unwrap().text.clone();
    assert!(after_text.find("// %% Model").unwrap() < after_text.find("// %% Clean").unwrap());
    StandaloneGate
        .assert_statement_partition_preserved(&before_text, &after_text)
        .expect("a move must be provably a reordering");

    // The file is still valid, still Windows-encoded, and still every statement.
    let after = std::fs::read(&f.path).unwrap();
    assert!(after.starts_with(b"\xef\xbb\xbf"));
    let text = String::from_utf8(after).unwrap();
    for stmt in [
        "sysuse auto, clear",
        "drop if price > 15000",
        "regress lprice mpg weight",
    ] {
        assert_eq!(text.matches(stmt).count(), 1, "{stmt} did not survive");
    }
    // The `///` chain and the `#delimit ;` stretch moved intact.
    assert!(text.contains("gen lprice = log(price) ///"));
    assert!(text.contains("#delimit ;"));
}

// ---------------------------------------------------------------------------
// D.4 / D.5 — deferred to W09 and W23
// ---------------------------------------------------------------------------

#[test]
#[ignore = "W09: `stratum run --json` exists but has no engine behind it — \
            `cmd::ENGINE_LINKED` is false and every run answers STRATUM0010 with \
            rc = 10. This asserts the well-framed NDJSON stream over the file \
            written above, and a zero exit status, which rc = 10 is not"]
fn d4_the_saved_file_runs_through_the_headless_runtime() {
    // The binary is located by env var rather than `CARGO_BIN_EXE_stratum`,
    // which only exists for a package that declares that binary — this file is
    // compiled by two crates, neither of which is `stratum-cli`.
    let bin = std::env::var("STRATUM_BIN").expect("W09 sets STRATUM_BIN");
    let raw = windows_bytes(ANALYSIS);
    let f = fixture(&raw);
    let out = std::process::Command::new(bin)
        .args(["run", f.path.as_str(), "--json", "--deterministic"])
        .output()
        .expect("stratum binary");
    assert!(out.status.success());
    for line in String::from_utf8(out.stdout).unwrap().lines() {
        serde_json::from_str::<serde_json::Value>(line).expect("one JSON object per line");
    }
}

#[test]
#[ignore = "W23 owns `xtask difftest`, and spec §32 forbids any CI job from \
            reaching Stata — this half of row D is self-hosted, permanently"]
fn d5_the_runtime_agrees_with_statamp_on_the_golden_set() {
    // Unlike D.4, this one never becomes a CI test: §32 is a rule about the
    // build, not a temporary gap. What it *can* assert on a self-hosted run,
    // before W23's harness exists, is that the oracle the runtime will be
    // compared against is the captured StataMP output and is present — a
    // difftest against a missing or fabricated golden set proves nothing.
    let goldens = workspace_root().join("tests/golden/stata18");
    for log in ["core_surface.log", "semantics.log", "errors.log"] {
        let p = goldens.join(log);
        assert!(p.is_file(), "the StataMP 18.5 oracle {p} is missing");
    }
}

/// The tripwire that stops D.4 from being deferred forever.
///
/// `#[ignore]` is static: nothing re-reads it when the blocking unit lands, so a
/// permanently ignored acceptance test is indistinguishable from a missing one —
/// and D.4 is half of the product's central promise, not a nice-to-have. This
/// test is **not** ignored. It fails the day a `stratum run` exists to point at,
/// and the failure says exactly what to delete.
///
/// D.5 has no tripwire on purpose: it stays ignored under spec §32 whatever
/// W23 lands.
#[test]
fn d4_is_still_actually_blocked_on_w09() {
    assert!(
        d4_unblocked_because().is_none(),
        "D.4 is no longer blocked ({}): delete the `#[ignore]` on \
         `d4_the_saved_file_runs_through_the_headless_runtime` in tests/e2e/scenario_d.rs \
         (W26's file). CI's half is already done — `.github/workflows/e2e.yml`'s tier-1 \
         job builds `stratum` into STRATUM_BIN. Scenario D is not passing until D.4 runs.",
        d4_unblocked_because().unwrap_or("")
    );
}

/// What lifted D.4's blocker, or `None` while it is still down.
///
/// **This predicate has been wrong twice, both times the same way: it asserted a
/// PROXY for the capability instead of the capability.** Written down in full
/// because the shape is the interesting part, not the two instances.
///
/// *First wrong reading — the crate exists.* It tested the `stratum-cli` crate
/// manifest for existence, assuming the manifest and W09 arrive together. They
/// did not: the architect created it — and `main.rs`, whose `main` printed "the
/// command surface lands with W09" and exited EX_USAGE — so that **W07**'s
/// finished `serve/**`, which owns no manifest anywhere in the tree, would be
/// compiled by something. The tripwire fired at a placeholder, stayed red at
/// HEAD, and (because `cargo test` is fail-fast by default) took every doctest
/// in the workspace down with it.
///
/// *Second wrong reading — `run.rs` exists, and `STRATUM_BIN` is set.* Both were
/// true as of the workspace repair round, and both are still not the capability.
/// `crates/stratum-cli/src/cmd/run.rs` is finished, and running the very file
/// D.4 writes through the binary CI builds produces:
///
/// ```text
/// {"event":"diagnostic",…,"code":"STRATUM0010","stata_rc":10,
///  "message":"the execution engine (crates/stratum-exec, work unit W08) is not
///             linked into this build; …"}
/// {"event":"run_finished",…,"rc":10,"blocks_run":0}
/// ```
///
/// — a well-framed NDJSON stream, three events, **exit 10**. D.4 asserts
/// `out.status.success()`, so un-ignoring it on that signal trades a red
/// tripwire for a red acceptance test and learns nothing. `cmd/mod.rs`'s own
/// header calls this out: `Engine::Absent` reports "we are incomplete" (exit 10)
/// and never "we are wrong" (exit 1), by design.
///
/// *The capability.* D.4 needs an executable that can `run <file> --json` **and
/// execute the file**. `stratum-cli` publishes exactly one flag for that,
/// `cmd::ENGINE_LINKED`, whose own doc comment says it "flips to `true` in the
/// *same commit* that adds the `stratum-exec` edge and the `Engine::Linked`
/// variant, and there is no other way to flip it" — deliberately a `const` and
/// not a Cargo feature, precisely so that it cannot be switched on ahead of the
/// thing it claims. That is the signal, and it is read from the source tree so
/// the tripwire still fires on a plain `cargo test` with nothing built, which is
/// how it will actually be noticed.
///
/// `STRATUM_BIN` is no longer consulted here. It is where D.4 *finds* the
/// binary, not evidence about what that binary can do — e2e.yml sets it whenever
/// `cmd/run.rs` exists, so treating it as an unblocker is the second wrong
/// reading wearing a different hat. Existence of the crate, the manifest, the
/// binary target or the subcommand proves nothing and none of them is consulted.
fn d4_unblocked_because() -> Option<&'static str> {
    engine_linked().then_some("crates/stratum-cli declares `ENGINE_LINKED = true`")
}

/// `stratum-cli`'s single declaration of whether an engine sits behind `run`.
///
/// Read as text rather than linked, because this file is compiled by
/// `stratum-workspace` and `stratum-e2e` and neither may take a dependency on
/// `stratum-cli` to ask (ARCHITECTURE §5 — the CLI is above both). A textual
/// read of one `const` is the cheapest thing that is still keyed on the
/// capability; the panic below is what stops it degrading into a green-forever
/// scan if W09 renames or moves the constant.
fn engine_linked() -> bool {
    let path = cli_src_dir().join("cmd/mod.rs");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("D.4's tripwire cannot read {path}: {e}"));
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("pub const ENGINE_LINKED: bool = ") else {
            continue;
        };
        return match rest.trim_end_matches(';').trim() {
            "true" => true,
            "false" => false,
            other => panic!("`ENGINE_LINKED` is `{other}`, which this tripwire cannot read"),
        };
    }
    panic!(
        "`pub const ENGINE_LINKED: bool` is no longer declared in {path}. \
         Re-point `engine_linked` at whatever replaced it, or D.4's tripwire is \
         dead code that reads as an assertion — which is the failure mode this \
         predicate has already had twice."
    );
}

/// The directory `d4_unblocked_because` probes, checked to be real.
///
/// A tripwire keyed on a path that has moved is worse than no tripwire: it is
/// green forever and reads as an assertion. `main.rs` is the one file under
/// `crates/stratum-cli/src/` that is guaranteed present for as long as the
/// binary exists at all, so anchoring on it fails loudly if W09 relocates or
/// renames the crate, instead of letting D.4 rot quietly.
fn cli_src_dir() -> Utf8PathBuf {
    let dir = workspace_root().join("crates/stratum-cli/src");
    assert!(
        dir.join("main.rs").is_file(),
        "the `stratum` binary's source root is no longer {dir}; \
         re-point `d4_unblocked_because` at it or D.4's tripwire is dead code"
    );
    dir
}

// ---------------------------------------------------------------------------

/// The repository root, found by walking up to the `[workspace]` manifest.
///
/// Not `CARGO_MANIFEST_DIR`: this file is compiled today by `stratum-workspace`
/// and later by W25's e2e crate, and the two sit at different depths.
fn workspace_root() -> Utf8PathBuf {
    let mut dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        let manifest = dir.join("Cargo.toml");
        if std::fs::read_to_string(&manifest)
            .unwrap_or_default()
            .lines()
            .any(|l| l.trim() == "[workspace]")
        {
            return dir;
        }
        assert!(
            dir.pop(),
            "no [workspace] Cargo.toml above {}",
            env!("CARGO_MANIFEST_DIR")
        );
    }
}

/// Count the physical lines that differ between two files, ignoring the line
/// terminator itself — so a wholesale EOL rewrite shows up as content changes
/// only where content changed, and the "one-line diff" claim means what a
/// reviewer would mean by it.
fn differing_lines(before: &[u8], after: &[u8]) -> usize {
    let split = |b: &[u8]| -> Vec<Vec<u8>> {
        b.split(|&c| c == b'\n')
            .map(|l| l.strip_suffix(b"\r").unwrap_or(l).to_vec())
            .collect()
    };
    let (a, b) = (split(before), split(after));
    (0..a.len().max(b.len()))
        .filter(|&i| a.get(i) != b.get(i))
        .count()
}

fn walk(root: &Utf8Path) -> Vec<Utf8PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(p) = Utf8PathBuf::from_path_buf(e.path()) else {
                continue;
            };
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}
