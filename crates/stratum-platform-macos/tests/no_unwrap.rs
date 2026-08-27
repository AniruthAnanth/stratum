//! The textual half of "no impl `unwrap`s `Unsupported` or `Cancelled`".
//!
//! `Cargo.toml` already denies `clippy::unwrap_used`, `expect_used` and `panic`
//! for this crate, so this scan is belt and braces — but it is the belt that
//! survives someone adding an `#[allow]` to make a build green, because it
//! names the file and the line rather than disappearing along with the lint.
//! It also covers `todo!`/`unimplemented!`, which clippy's three lints do not.
#![cfg(target_os = "macos")]
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
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        for (i, line) in text.lines().enumerate() {
            // Prose in a doc comment may legitimately name these.
            let code = line.trim_start();
            if code.starts_with("//") || code.starts_with("*") {
                continue;
            }
            for pat in BANNED {
                if code.contains(pat) {
                    hits.push(format!("{}:{}: {}", file.display(), i + 1, line.trim()));
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
