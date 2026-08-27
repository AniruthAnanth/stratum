//! Shared scaffolding for this crate's integration tests.
//!
//! `tests/common/mod.rs` rather than a sixth `tests/*.rs`: cargo compiles every
//! direct child of `tests/` as its own test binary, and a subdirectory module is
//! the standard way to share code between them without shipping an empty binary.

#![allow(dead_code)] // each test binary uses a different subset

use camino::{Utf8Path, Utf8PathBuf};
use stratum_workspace::keymap::KeymapStore;
use stratum_workspace::layout::LayoutStore;
use stratum_workspace::project::Project;
use stratum_workspace::Workspace;

/// A throwaway directory, plus its UTF-8 path.
///
/// The `TempDir` must be kept alive by the caller: dropping it deletes the tree.
pub fn tmp() -> (tempfile::TempDir, Utf8PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let p = Utf8PathBuf::from_path_buf(t.path().to_path_buf()).unwrap();
    (t, p)
}

/// A workspace over `root`, with its config directories inside the same tree so
/// a test never touches the developer's real config.
pub fn project_at(root: &Utf8Path) -> Workspace {
    let project = Project::load(root).unwrap();
    Workspace::new(
        project,
        LayoutStore::new(root.join("resources/layouts"), root.join("config/layouts")),
        KeymapStore::new(root.join("resources/keymaps"), root.join("config/keymaps")),
    )
}

/// Count the physical lines that differ between two files.
///
/// Split on `\n` after stripping `\r`, so a file whose line endings changed
/// wholesale reports *every* line as differing — which is exactly the failure
/// this crate exists to prevent, and it should be loud when it happens.
pub fn lines_differing(before: &[u8], after: &[u8]) -> usize {
    let split = |b: &[u8]| -> Vec<Vec<u8>> {
        b.split(|&c| c == b'\n')
            .map(|l| l.strip_suffix(b"\r").unwrap_or(l).to_vec())
            .collect()
    };
    let (a, b) = (split(before), split(after));
    let n = a.len().max(b.len());
    (0..n).filter(|&i| a.get(i) != b.get(i)).count()
}
