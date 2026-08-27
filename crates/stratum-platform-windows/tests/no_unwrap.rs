//! The textual half of "no impl `unwrap`s `Unsupported` or `Cancelled`".
//!
//! `Cargo.toml` already denies `clippy::unwrap_used`, `expect_used` and `panic`
//! for this crate, so this scan is belt and braces — but it is the belt that
//! survives someone adding an `#[allow]` to make a build green, because it
//! names the file and the line rather than disappearing along with the lint. It
//! also covers `todo!`/`unimplemented!`, which clippy's three lints do not.
//!
//! # It has to run on every host, and that is the point
//!
//! Two thirds of this crate's shipped lines are behind
//! `#[cfg(target_os = "windows")]` and are therefore invisible to clippy on the
//! machine this was written on. A *textual* scan does not care what the target
//! is, so the `windows`-calling code is held to the no-abort rule from a macOS
//! test run — which is the only place it can be held to it today.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

/// Spellings that turn a first-class platform outcome into an abort.
const BANNED: &[&str] = &[
    ".unwrap()",
    ".expect(",
    "panic!(",
    "unreachable!(",
    "todo!(",
    "unimplemented!(",
];

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.filter_map(Result::ok) {
        let p = e.path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Strip `#[cfg(test)] mod … { … }` blocks by brace depth.
///
/// The macOS crate keeps its tests in `tests/`, so its scanner needs no such
/// thing. This one cannot: the pure-policy functions are the substance of this
/// crate and their tests belong next to them, so the scan has to tell a test
/// module from a shipped one. `#[cfg(target_os = "windows")] mod sys` is
/// **not** skipped — that is the shipped path, and it is what this exists for.
fn shipped_lines(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut skip_depth: Option<i32> = None;
    let mut pending_cfg_test = false;

    for (i, line) in text.lines().enumerate() {
        if let Some(depth) = skip_depth.as_mut() {
            *depth += i32::try_from(line.matches('{').count()).unwrap_or(0);
            *depth -= i32::try_from(line.matches('}').count()).unwrap_or(0);
            if *depth <= 0 {
                skip_depth = None;
            }
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("#[cfg(test)]") {
            pending_cfg_test = true;
            continue;
        }
        if pending_cfg_test {
            pending_cfg_test = false;
            if trimmed.starts_with("mod ") && line.contains('{') {
                let opens = i32::try_from(line.matches('{').count()).unwrap_or(0);
                let closes = i32::try_from(line.matches('}').count()).unwrap_or(0);
                if opens - closes > 0 {
                    skip_depth = Some(opens - closes);
                }
                continue;
            }
        }
        out.push((i + 1, line));
    }
    out
}

#[test]
fn no_shipped_path_in_this_crate_can_abort() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&src, &mut files);
    assert!(
        !files.is_empty(),
        "no sources found under {}",
        src.display()
    );

    let mut hits = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap();
        for (line_no, line) in shipped_lines(&text) {
            // Prose in a doc comment may legitimately name these.
            let code = line.trim_start();
            if code.starts_with("//") || code.starts_with('*') {
                continue;
            }
            for pat in BANNED {
                if code.contains(pat) {
                    hits.push(format!("{}:{line_no}: {}", file.display(), line.trim()));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "a platform adapter must return PlatformError, never abort:\n{}",
        hits.join("\n")
    );
}

/// The scanner is load-bearing, so it gets its own test: a scanner that skipped
/// too much would report a clean crate that is not one, and the failure would
/// be silent.
#[test]
fn the_scanner_skips_test_modules_and_nothing_else() {
    let src = "\
fn shipped() { let _ = x.unwrap(); }
#[cfg(test)]
mod tests {
    fn inner() { let _ = y.unwrap(); }
    mod deeper { fn z() { let _ = z.unwrap(); } }
}
#[cfg(target_os = \"windows\")]
mod sys {
    fn also_shipped() { let _ = w.unwrap(); }
}
";
    let kept: Vec<&str> = shipped_lines(src).into_iter().map(|(_, l)| l).collect();
    let joined = kept.join("\n");
    assert!(joined.contains("fn shipped()"));
    assert!(
        joined.contains("also_shipped"),
        "the cfg(windows) path is the shipped path"
    );
    assert!(!joined.contains("fn inner()"));
    assert!(!joined.contains("fn z()"));
    // And the module that follows the skipped one is not swallowed with it.
    assert!(joined.contains("mod sys {"));
}
