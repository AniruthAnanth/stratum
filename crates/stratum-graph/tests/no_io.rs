//! ARCHITECTURE §8.14 / ADR A14, as a test rather than as a promise.
//!
//! > `rg 'std::fs|Utf8Path|include_str!' crates/stratum-graph/src` is empty;
//! > scheme colours come from `stratum_tokens::SCHEMES`.
//!
//! CI greps for this. A grep in a workflow file is a check that only fires where
//! the workflow runs; the same check as a unit test fires on every developer's
//! machine the moment they add the import, which is when it is cheap to undo.
//!
//! This file may of course read the filesystem — it is in `tests/`, and the
//! invariant is scoped to `src/`. That asymmetry is the point: the crate that
//! renders is I/O-free, the crate that *checks* it does not have to be.

use std::fs;
use std::path::{Path, PathBuf};

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("readable source directory") {
        let path = entry.expect("readable entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The banned tokens, and why each one is banned.
const BANNED: &[(&str, &str)] = &[
    (
        "std::fs",
        "the crate must render on a machine with no `apps/` directory",
    ),
    ("Utf8Path", "A14: this crate does no path resolution"),
    (
        "include_str!",
        "a compiled-in file is still a file; schemes come from stratum-tokens",
    ),
    ("include_bytes!", "same, for a font or an image"),
    (
        "std::env",
        "an environment lookup is ambient input a headless render must not have",
    ),
    ("std::net", "a renderer has no business on a socket"),
];

#[test]
fn the_source_tree_reaches_no_filesystem() {
    let mut files = Vec::new();
    rust_files(&src_dir(), &mut files);
    assert!(
        files.len() > 8,
        "found only {} source files — did the walk work?",
        files.len()
    );

    let mut offences = Vec::new();
    for file in &files {
        let text = fs::read_to_string(file).expect("utf-8 source");
        for (line_no, line) in text.lines().enumerate() {
            // Skip the doc comments that NAME the banned tokens in order to
            // explain the rule; a rule that cannot be written down is worse than
            // a slightly narrower grep.
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with('*') {
                continue;
            }
            for (needle, why) in BANNED {
                if line.contains(needle) {
                    offences.push(format!(
                        "{}:{}: `{needle}` — {why}",
                        file.display(),
                        line_no + 1
                    ));
                }
            }
        }
    }
    assert!(
        offences.is_empty(),
        "ARCHITECTURE §8.14 violated:\n{}",
        offences.join("\n")
    );
}

/// The other half of A14: the colours are the design system's, not literals.
#[test]
fn colours_come_from_the_generated_tokens() {
    let mut files = Vec::new();
    rust_files(&src_dir(), &mut files);

    let mut offences = Vec::new();
    for file in &files {
        // The test modules legitimately spell a colour to assert against.
        let text = fs::read_to_string(file).expect("utf-8 source");
        let production = text.split("#[cfg(test)]").next().unwrap_or("");
        for (line_no, line) in production.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            // A `#RRGGBB` literal in production code is a colour that will not
            // move when `design/tokens.json` does.
            if let Some(pos) = line.find('#') {
                let rest = &line[pos + 1..];
                let hex: String = rest.chars().take(6).collect();
                if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    offences.push(format!("{}:{}: {line}", file.display(), line_no + 1));
                }
            }
        }
    }
    assert!(
        offences.is_empty(),
        "hardcoded colour(s); use stratum_tokens:\n{}",
        offences.join("\n")
    );
}

/// The scheme table this crate draws from is the committed, generated one.
#[test]
fn the_three_schemes_are_present_and_compiled_in() {
    assert_eq!(
        stratum_graph::scheme_ids(),
        ["stratum", "stratum-dark", "print"]
    );
    assert_eq!(stratum_graph::DEFAULT_SCHEME, "stratum");
    // `print` is pure white ground and black ink whatever the app looks like,
    // because the figure is going into a paper.
    let print = stratum_tokens::scheme("print").expect("print scheme");
    assert_eq!(print.background.hex, "#FFFFFF");
    assert_eq!(print.foreground.hex, "#000000");
}
