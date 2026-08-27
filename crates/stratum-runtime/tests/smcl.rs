//! SMCL, and the classic stored-result listing — W06's SMCL acceptance bullet.
//!
//! > SMCL → `Vec<StyledRun>` round-trips against a golden corpus, and the same
//! > function backs the log file writer and the CLI text mode. It parses SMCL
//! > that **user code** emits; it does not wrap W05's tables, which arrive
//! > already styled (A12).
//!
//! # Where the corpus lives, and why it is here
//!
//! `docs/ownership.toml` gives W06b exactly one file for this bullet — this one
//! — so the corpus is a `const` table rather than a `tests/golden/smcl/`
//! directory W06b is not allowed to create. That is not a loss: the fixtures are
//! one line each, and pinning the **runs** beside the bytes in the same table is
//! what makes a style regression exactly as loud as a spacing regression, which
//! is the property W05's renderer goldens state for the other half of the styled
//! surface.
//!
//! Every fixture is SMCL a *user* can emit — `display as result`, an ado-file
//! drawing its own table, a `.sthlp` file — because that is the only input this
//! parser is for. `stratum-stats` tables never come through here (A12).
//!
//! # The `ereturn list` fixture is not a corpus entry
//!
//! It is compared against `tests/golden/stata18/core_surface.log` itself, byte
//! for byte, because that file is authoritative and irreplaceable — the Stata
//! licence that produced it has expired. `results::classic_list` and the SMCL
//! parser share one flattening function (`stratum_proto::styled::to_plain`), so
//! this file is where both halves of "the log and the goldens cannot drift" are
//! asserted together.

use std::path::PathBuf;

use stratum_proto::{styled::to_plain, StyleId, StyledRun};
use stratum_runtime::results::{classic_list, Class, Matrix, ResultSet};
use stratum_runtime::smcl::{parse, parse_with, to_smcl, LinkKind, LINESIZE};

// ---------------------------------------------------------------------------
// The corpus
// ---------------------------------------------------------------------------

/// `(name, smcl, plain, runs as (text, style))`.
///
/// `style` is spelled with the helper constructors below so the table stays
/// readable; `L(n)` is `StyleId::Link { target_index: n }`.
struct Case {
    name: &'static str,
    smcl: &'static str,
    plain: &'static str,
    runs: &'static [(&'static str, StyleId)],
    targets: &'static [(LinkKind, &'static str)],
}

const T: StyleId = StyleId::Text;
const R: StyleId = StyleId::Result;
const E: StyleId = StyleId::Error;
const I: StyleId = StyleId::Input;
const H: StyleId = StyleId::Hilite;
const RULE: StyleId = StyleId::Rule;
const HEAD: StyleId = StyleId::Heading;
const fn l(i: u32) -> StyleId {
    StyleId::Link { target_index: i }
}

/// 80 dashes — `{hline}` with no argument at `c(linesize)`.
const HLINE80: &str =
    "--------------------------------------------------------------------------------";

const CORPUS: &[Case] = &[
    Case {
        name: "a plain string is one text run",
        smcl: "74 observations",
        plain: "74 observations",
        runs: &[("74 observations", T)],
        targets: &[],
    },
    Case {
        // `display as result` — the single most common thing user code emits.
        name: "channel switches split runs and nothing else",
        smcl: "{txt}mean = {res}21.3{txt} units",
        plain: "mean = 21.3 units",
        runs: &[("mean = ", T), ("21.3", R), (" units", T)],
        targets: &[],
    },
    Case {
        name: "an error message keeps the offending name on its own channel",
        smcl: "{err}variable {res}mpg{err} not found",
        plain: "variable mpg not found",
        runs: &[("variable ", E), ("mpg", R), (" not found", E)],
        targets: &[],
    },
    Case {
        // An ado-file drawing its own header the way `summarize` does.
        name: "col pads to a one-based column",
        smcl: "{txt}    Variable{col 14}{c |}{res}        Obs",
        plain: "    Variable |        Obs",
        runs: &[("    Variable |", T), ("        Obs", R)],
        targets: &[],
    },
    Case {
        name: "col never moves backwards",
        smcl: "abcdefgh{col 3}!",
        plain: "abcdefgh!",
        runs: &[("abcdefgh!", T)],
        targets: &[],
    },
    Case {
        name: "hline takes a count, and without one fills the line",
        smcl: "{hline 13}{c +}{hline 20}",
        plain: "-------------+--------------------",
        runs: &[
            ("-------------", RULE),
            ("+", T),
            ("--------------------", RULE),
        ],
        targets: &[],
    },
    Case {
        name: "a bare hline fills to c(linesize)",
        smcl: "{hline}",
        plain: HLINE80,
        runs: &[(HLINE80, RULE)],
        targets: &[],
    },
    Case {
        name: "dup repeats its body",
        smcl: "{dup 3:ab}",
        plain: "ababab",
        runs: &[("ababab", T)],
        targets: &[],
    },
    Case {
        name: "space emits exactly its count",
        smcl: "{space 4}indented",
        plain: "    indented",
        runs: &[("    indented", T)],
        targets: &[],
    },
    Case {
        name: "break is a newline and resets the column",
        smcl: "line1{break}{col 3}line2",
        plain: "line1\n  line2",
        runs: &[("line1\n  line2", T)],
        targets: &[],
    },
    Case {
        name: "a comment directive produces nothing",
        smcl: "{* internal note}visible",
        plain: "visible",
        runs: &[("visible", T)],
        targets: &[],
    },
    Case {
        // Stata errors on an unknown directive. Swallowing it would make a
        // user's own output quietly disappear, which is the worse failure for a
        // product whose thesis is fidelity.
        name: "an unknown directive is printed, not swallowed",
        smcl: "x{nosuchthing}y",
        plain: "x{nosuchthing}y",
        runs: &[("x{nosuchthing}y", T)],
        targets: &[],
    },
    Case {
        name: "an unmatched brace is literal text",
        smcl: "100% {of it",
        plain: "100% {of it",
        runs: &[("100% {of it", T)],
        targets: &[],
    },
    Case {
        name: "the character directive escapes braces",
        smcl: "{c -(}foreach{c )-}",
        plain: "{foreach}",
        runs: &[("{foreach}", T)],
        targets: &[],
    },
    Case {
        name: "title is a heading",
        smcl: "{title:Description}",
        plain: "Description",
        runs: &[("Description", HEAD)],
        targets: &[],
    },
    Case {
        name: "bf and it are the highlight channel",
        smcl: "{bf:bold}{it:ital}",
        plain: "boldital",
        runs: &[("boldital", H)],
        targets: &[],
    },
    Case {
        // `[P] smcl`: the colon marks the minimum abbreviation. It does not
        // delimit an argument, and both halves are displayed.
        name: "opt shows the whole option name, abbreviation marker and all",
        smcl: "{opt d:etail}",
        plain: "detail",
        runs: &[("detail", I)],
        targets: &[],
    },
    Case {
        name: "cmdab shows the whole command name",
        smcl: "{cmdab:reg:ress}",
        plain: "regress",
        runs: &[("regress", I)],
        targets: &[],
    },
    Case {
        name: "opt with no abbreviation marker still shows its argument",
        smcl: "{opt vce(vcetype)}",
        plain: "vce(vcetype)",
        runs: &[("vce(vcetype)", I)],
        targets: &[],
    },
    Case {
        name: "a bare help link prints its own topic",
        smcl: "see {help summarize}",
        plain: "see summarize",
        runs: &[("see ", T), ("summarize", l(0))],
        targets: &[(LinkKind::Help, "summarize")],
    },
    Case {
        // A style switch inside a link body decorates; it must not replace the
        // link, or `targets` holds a destination no run points at.
        name: "a style switch inside a link body keeps the link",
        smcl: "{help summarize:{bf:summarize}}",
        plain: "summarize",
        runs: &[("summarize", l(0))],
        targets: &[(LinkKind::Help, "summarize")],
    },
    Case {
        name: "two references to one topic share one target",
        smcl: "{help summarize} and {help summarize:it}",
        plain: "summarize and it",
        runs: &[("summarize", l(0)), (" and ", T), ("it", l(0))],
        targets: &[(LinkKind::Help, "summarize")],
    },
    Case {
        name: "a stata link carries the command line verbatim",
        smcl: "{stata regress price mpg:click me}",
        plain: "click me",
        runs: &[("click me", l(0))],
        targets: &[(LinkKind::Stata, "regress price mpg")],
    },
    Case {
        // The Viewer's paragraph engine owns these; emitting a newline for them
        // here would corrupt every `.sthlp` line width.
        name: "paragraph directives contribute no bytes",
        smcl: "{p 4 8 2}{marker syntax}text{p_end}{...}",
        plain: "text",
        runs: &[("text", T)],
        targets: &[],
    },
];

fn expected_runs(c: &Case) -> Vec<StyledRun> {
    c.runs
        .iter()
        .map(|(t, s)| StyledRun {
            text: (*t).to_owned(),
            style: *s,
        })
        .collect()
}

#[test]
fn the_corpus_flattens_to_its_golden_bytes() {
    for c in CORPUS {
        assert_eq!(parse(c.smcl).to_plain(), c.plain, "case: {}", c.name);
    }
}

#[test]
fn the_corpus_produces_its_golden_runs() {
    // Run boundaries and styles, pinned beside the bytes: a style regression is
    // exactly as loud as a spacing regression.
    for c in CORPUS {
        assert_eq!(parse(c.smcl).runs, expected_runs(c), "case: {}", c.name);
    }
}

#[test]
fn the_corpus_produces_its_golden_link_targets() {
    for c in CORPUS {
        let got = parse(c.smcl);
        let want: Vec<(LinkKind, &str)> = c.targets.to_vec();
        let have: Vec<(LinkKind, &str)> = got
            .targets
            .iter()
            .map(|t| (t.kind, t.arg.as_str()))
            .collect();
        assert_eq!(have, want, "case: {}", c.name);
    }
}

#[test]
fn every_link_target_is_reachable_from_a_run() {
    // A destination no run points at is a `{help}` the Viewer cannot make
    // clickable — invisible in `to_plain`, so only this assertion catches it.
    for c in CORPUS {
        let got = parse(c.smcl);
        for (i, target) in got.targets.iter().enumerate() {
            let i = i as u32;
            assert!(
                got.runs
                    .iter()
                    .any(|r| r.style == StyleId::Link { target_index: i }),
                "case {}: target {i} ({target:?}) is unreachable",
                c.name
            );
        }
        for r in &got.runs {
            if let StyleId::Link { target_index } = r.style {
                assert!(
                    (target_index as usize) < got.targets.len(),
                    "case {}: run {r:?} indexes past the target table",
                    c.name
                );
            }
        }
    }
}

#[test]
fn to_plain_is_the_workspaces_single_flattening_function() {
    // A12/C22: the log writer, the CLI text mode, `log_copy` and the goldens all
    // go through `stratum_proto::styled::to_plain`. A second implementation
    // here — even a correct one — is how the log and the goldens drift apart.
    for c in CORPUS {
        let got = parse(c.smcl);
        assert_eq!(got.to_plain(), to_plain(&got.runs), "case: {}", c.name);
    }
}

#[test]
fn the_lossless_channels_round_trip_through_to_smcl() {
    // `to_smcl` is lossless for the five channel styles that have a bare
    // directive. `Heading`, `Rule`, `Comment` and `Link` come from body forms
    // and have none, so they are written on their nearest channel and are NOT
    // claimed to round-trip — the bytes still must.
    let lossless = |s: StyleId| {
        matches!(
            s,
            StyleId::Text | StyleId::Result | StyleId::Error | StyleId::Input | StyleId::Hilite
        )
    };
    for c in CORPUS {
        let first = parse(c.smcl);
        let again = parse(&to_smcl(&first.runs));
        assert_eq!(
            again.to_plain(),
            first.to_plain(),
            "case {}: bytes must survive a round trip through SMCL",
            c.name
        );
        if first.runs.iter().all(|r| lossless(r.style)) {
            assert_eq!(
                again.runs, first.runs,
                "case {}: styles must survive too",
                c.name
            );
        }
    }
}

#[test]
fn re_encoding_is_idempotent() {
    // `log using …, smcl` writes what `to_smcl` produces and may replay it; a
    // second pass must not accumulate directives.
    for c in CORPUS {
        let once = to_smcl(&parse(c.smcl).runs);
        let twice = to_smcl(&parse(&once).runs);
        assert_eq!(once, twice, "case: {}", c.name);
    }
}

#[test]
fn braces_in_user_text_survive_re_encoding() {
    // `display "{c -(}"` prints a brace. If `to_smcl` wrote it back raw, the
    // next parse would read it as the start of a directive and eat the rest of
    // the line.
    let s = parse("a {c -(}b{c )-} c");
    assert_eq!(s.to_plain(), "a {b} c");
    let round = parse(&to_smcl(&s.runs));
    assert_eq!(round.to_plain(), "a {b} c");
    assert_eq!(round.runs, s.runs);
}

// ---------------------------------------------------------------------------
// Layout that depends on the line width
// ---------------------------------------------------------------------------

#[test]
fn linesize_is_eighty_in_every_code_path() {
    // ADR-016/A16: `c(linesize)` is 80, always. `parse` is the path everything
    // else uses, and it must not have picked up a different width.
    assert_eq!(LINESIZE, 80);
    assert_eq!(parse("{hline}").to_plain().chars().count(), 80);
    assert_eq!(parse_with("{hline}", 20).to_plain().chars().count(), 20);
    assert_eq!(
        parse("{right:x}").to_plain(),
        format!("{}x", " ".repeat(79))
    );
    assert_eq!(
        parse_with("{right:x}", 10).to_plain(),
        format!("{}x", " ".repeat(9))
    );
    assert_eq!(parse_with("{center:xx}", 10).to_plain(), "    xx");
}

#[test]
fn hline_after_text_fills_only_the_rest_of_the_line() {
    let out = parse("abc{hline}").to_plain();
    assert_eq!(out.chars().count(), LINESIZE);
    assert!(out.starts_with("abc-"));
}

// ---------------------------------------------------------------------------
// Adversarial input
// ---------------------------------------------------------------------------

#[test]
fn adversarial_input_neither_panics_nor_runs_away() {
    // A `.sthlp` is a file on disk that a `net install` put there. It is not
    // trusted input, and the parser is the only thing between it and the
    // Viewer.
    let deep = format!("{}x{}", "{bf:".repeat(500), "}".repeat(500));
    let cases: Vec<String> = vec![
        deep,
        "{".repeat(1000),
        "}".repeat(1000),
        "{help".to_owned(),
        "{:}".to_owned(),
        "{c}".to_owned(),
        "{hline -3}".to_owned(),
        "{col -1}x".to_owned(),
        "{col 99999999999999999999}x".to_owned(),
        "{dup 4294967295:x}".to_owned(),
        "{space 99999999999999999999}".to_owned(),
        "{opt :}".to_owned(),
        "café {res}€{txt} ⟨⟩".to_owned(),
        "{res}{res}{res}".to_owned(),
    ];
    for c in &cases {
        let got = parse(c);
        // A bounded `{dup}`: the ceiling is the point, and it is a counter.
        assert!(
            got.to_plain().len() <= c.len().max(4096) * 8 + LINESIZE,
            "input {c:?} produced {} bytes",
            got.to_plain().len()
        );
        assert_eq!(got.to_plain(), to_plain(&got.runs));
        // Every run is non-empty: an empty run is invisible in `to_plain` and
        // would make the run count a lie for anything styling off it.
        assert!(got.runs.iter().all(|r| !r.text.is_empty()), "input {c:?}");
    }
    assert!(parse("café {res}€{txt} ⟨⟩").to_plain().contains('€'));
}

#[test]
fn a_deeply_nested_body_still_yields_its_text() {
    let deep = format!("{}x{}", "{bf:".repeat(200), "}".repeat(200));
    assert!(parse(&deep).to_plain().contains('x'));
}

// ---------------------------------------------------------------------------
// `ereturn list` against the committed StataMP 18.5 capture
// ---------------------------------------------------------------------------

/// The `. ereturn list` block of `tests/golden/stata18/core_surface.log`,
/// verbatim: from the blank line that follows the command to the last output
/// line, exclusive of the blank line before the next `. ` prompt.
fn golden_block(command: &str) -> String {
    let log = repo_root().join("tests/golden/stata18/core_surface.log");
    let text = std::fs::read_to_string(&log).unwrap_or_else(|e| {
        panic!("the golden is irreplaceable and must be present: {log:?}: {e}")
    });
    let marker = format!("\n. {command}\n");
    let start = text
        .find(&marker)
        .unwrap_or_else(|| panic!("`{command}` is not in {log:?}"))
        + marker.len();
    let rest = &text[start..];
    let end = rest.find("\n. ").expect("a following prompt");
    rest[..end].to_owned()
}

fn repo_root() -> PathBuf {
    // `crates/stratum-runtime` -> repo root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("the crate lives two levels under the repo root")
        .to_path_buf()
}

#[test]
fn ereturn_list_is_byte_exact_against_the_stata18_golden() {
    // Insertion order is the layout (C31): `e(cmdline)`, `e(title)`,
    // `e(marginsok)`, `e(vce)`, `e(depvar)`, `e(cmd)`, … is neither alphabetical
    // nor sorted by anything else, so a `HashMap` here would produce a different
    // transcript on every run and break `--deterministic` (A8).
    //
    // The scalar VALUES are read out of the golden and fed back in, which is the
    // property under test for them: Stata printed those doubles with `%18.0g`,
    // and `stratum_core::fmt::fmt_g` must reproduce the same text from the same
    // double. The NAMES and their ORDER are the golden's, and the layout is
    // entirely ours — a wrong field width, separator or section header moves the
    // bytes and fails here.
    let golden = golden_block("ereturn list");
    let mut set = ResultSet::new();
    let mut section = "";
    for line in golden.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(s) = trimmed.strip_suffix(':') {
            if matches!(s, "scalars" | "macros" | "matrices" | "functions") {
                section = match s {
                    "scalars" => "scalars",
                    "macros" => "macros",
                    "matrices" => "matrices",
                    _ => "functions",
                };
                continue;
            }
        }
        match section {
            "scalars" => {
                let (label, value) = trimmed.split_once(" = ").expect("scalar row");
                set.set_scalar(&name_of(label), value.trim().parse().expect("a double"));
            }
            "macros" => {
                let (label, value) = trimmed.split_once(" : ").expect("macro row");
                let value = value.trim().trim_matches('"');
                set.set_macro(&name_of(label), value);
            }
            "matrices" => {
                let (label, dims) = trimmed.split_once(" : ").expect("matrix row");
                let (rows, cols) = dims.trim().split_once(" x ").expect("`R x C`");
                let (rows, cols): (u32, u32) =
                    (rows.trim().parse().unwrap(), cols.trim().parse().unwrap());
                set.set_matrix(
                    &name_of(label),
                    Matrix {
                        rows,
                        cols,
                        rownames: Vec::new(),
                        colnames: Vec::new(),
                        data: vec![0.0; (rows * cols) as usize],
                    },
                );
            }
            "functions" => set.set_function(&name_of(trimmed)),
            _ => panic!("a row outside any section: {line:?}"),
        }
    }

    let rendered = to_plain(&classic_list(Class::E, &set));
    assert_eq!(rendered, golden);
}

#[test]
fn the_classic_listing_puts_values_on_the_result_channel() {
    // The Classic pane prints result values in a distinct ink without
    // regex-scraping a rendered table, and the log writer flattens the same runs
    // to the same bytes. Both properties at once.
    let mut set = ResultSet::new();
    set.set_scalar("N", 74.0);
    set.set_macro("cmd", "regress");
    let runs = classic_list(Class::E, &set);
    assert!(runs.iter().any(|r| r.style == StyleId::Result));
    assert_eq!(
        to_plain(&runs),
        "\nscalars:\n                  e(N) =  74\n\nmacros:\n                e(cmd) : \"regress\"\n"
    );
}

/// `e(cmdline)` -> `cmdline`.
fn name_of(label: &str) -> String {
    label
        .trim()
        .trim_start_matches(['r', 'e', 's'])
        .trim_start_matches('(')
        .trim_end_matches(')')
        .to_owned()
}
