//! ARCHITECTURE §8.11, run as a unit test so the failure is LOCAL.
//!
//! `scripts/check-topology.sh` runs the same scan in CI. Duplicating it here is
//! deliberate: a contributor who calls `ln` as a method inside a kernel finds
//! out from `cargo test -p stratum-core`, in the crate that owns the rule,
//! rather than from a red required check twenty minutes later with no
//! explanation attached.
//!
//! The method names below are stored WITHOUT their dot and parenthesis and
//! reassembled at run time. Spelled out, this file would be the first thing its
//! own scan — and CI's — reported.
//!
//! The rule (ADR-004): no `mul_add` and no `std` `f64` transcendental anywhere
//! under `crates/` outside `stratum_core::math`. `sqrt` is exempt because
//! IEEE-754 requires it to be correctly rounded, so every implementation
//! returns the same bits.

use std::path::{Path, PathBuf};

/// The methods §8.11 names, plus `mul_add`. See the module note on the spelling.
const BANNED: &[&str] = &[
    "ln", "ln_1p", "log", "log10", "log2", "exp", "exp_m1", "exp2", "powf", "powi", "sin", "cos",
    "tan", "asin", "acos", "atan", "atan2", "sinh", "cosh", "tanh", "asinh", "acosh", "atanh",
    "cbrt", "hypot", "mul_add",
];

/// The one file allowed to contain them: it is the `libm` adapter itself.
const EXEMPT: &str = "crates/stratum-core/src/math.rs";

/// And this file, which would otherwise report its own reassembled needles.
const SELF: &str = "crates/stratum-core/tests/source_invariants.rs";

#[test]
fn no_std_transcendental_and_no_mul_add_outside_math_rs() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let banned: Vec<String> = BANNED.iter().map(|m| format!(".{m}(")).collect();
    let mut hits = Vec::new();
    for file in rust_files(&root.join("crates")) {
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        if rel == EXEMPT || rel == SELF {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (n, line) in text.lines().enumerate() {
            for b in &banned {
                if line.contains(b.as_str()) {
                    hits.push(format!("{rel}:{}: {}", n + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "ADR-004 / ARCHITECTURE §8.11: transcendentals come from \
         stratum_core::math (the libm crate), and mul_add is banned outright \
         because it fuses to one rounding step only where the target has FMA. \
         Found:\n{}",
        hits.join("\n")
    );
}

/// `stratum-core` itself must not reach the host-only surface §8.4 forbids, so
/// that `cargo check --target wasm32-unknown-unknown` cannot regress quietly
/// between CI runs.
#[test]
fn core_does_not_reach_host_only_surface() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();
    for file in rust_files(&src) {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (n, line) in text.lines().enumerate() {
            for needle in ["std::fs", "std::net", "std::process", "std::time"] {
                if line.contains(needle) {
                    hits.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "ARCHITECTURE §8.4 — stratum-core builds for wasm32-unknown-unknown \
         and reaches no filesystem, socket, subprocess or clock:\n{}",
        hits.join("\n")
    );
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}
