//! The first W10 acceptance bullet, as a test rather than a comment:
//! "`stratum-platform` compiles for all three targets with zero OS deps
//! (`cargo tree` assertion)".
//!
//! Two assertions, and the second is the stronger one:
//!
//! 1. The resolved normal-dependency set is exactly [`ALLOWED`]. An allow-list
//!    rather than a ban-list, because a ban-list only catches the OS crates
//!    somebody already thought of — `directories` reaches `windows-sys`, `url`
//!    reaches an ICU surface, and neither is named "os".
//! 2. The set is IDENTICAL on all three release targets. A `cfg(windows)`
//!    dependency is invisible on a macOS developer's machine and shows up as a
//!    `cargo deny` failure on a release runner three weeks later; if the graph
//!    cannot vary by target, that cannot happen.

use std::collections::BTreeSet;
use std::process::Command;

/// Every crate `stratum-platform` is allowed to reach, with default features.
/// Adding a row here is a deliberate act; see 08 §5.0 rule 1.
const ALLOWED: &[&str] = &[
    "stratum-platform",
    // 08 §5.3: SecretString.
    "secrecy",
    "zeroize",
    // The contract crate's own permitted surface, mirrored.
    "serde",
    "serde_core",
    "serde_derive",
    "camino",
    "thiserror",
    "thiserror-impl",
    // Dyn-compatible async traits (08 §5.4, §5.7).
    "async-trait",
    // Proc-macro plumbing under the four derives above.
    "proc-macro2",
    "quote",
    "syn",
    "unicode-ident",
];

/// 08 §6.2's release matrix, minus the two second-architecture rows whose graph
/// is identical to their sibling by construction.
const TARGETS: [&str; 3] = [
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
];

fn tree(target: &str) -> BTreeSet<String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
    let out = Command::new(cargo)
        .args([
            "tree",
            "--manifest-path",
            manifest,
            "-p",
            "stratum-platform",
            "--edges",
            "normal",
            "--prefix",
            "none",
            "--target",
            target,
        ])
        .output()
        .expect("cargo tree could not be run");
    assert!(
        out.status.success(),
        "cargo tree --target {target} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|n| !n.is_empty())
        .map(str::to_owned)
        .collect()
}

#[test]
fn dependency_set_is_exactly_the_allow_list_on_every_target() {
    let allowed: BTreeSet<String> = ALLOWED.iter().map(|s| (*s).to_owned()).collect();
    for target in TARGETS {
        let got = tree(target);
        let unexpected: Vec<_> = got.difference(&allowed).collect();
        assert!(
            unexpected.is_empty(),
            "{target}: stratum-platform reached {unexpected:?}, which is not in the \
             allow-list in tests/no_os_deps.rs. If the new dependency is genuinely \
             OS-free, add it there and say why in the commit; if it is not, it belongs \
             in stratum-platform-{{macos,windows,linux}} instead (08 §5.0 rule 1)."
        );
    }
}

#[test]
fn the_graph_does_not_vary_by_target() {
    let mac = tree(TARGETS[0]);
    for target in &TARGETS[1..] {
        let other = tree(target);
        assert_eq!(
            mac, other,
            "the dependency graph differs between {} and {target}; a cfg-gated OS \
             dependency has appeared",
            TARGETS[0]
        );
    }
}
