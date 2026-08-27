//! The **volatile** sidecar — `.stratum/cache/<hash>/`, ARCHITECTURE C19 / A4.
//!
//! The other half of the two-artifact split. Everything here is derived,
//! machine-specific, or both, and none of it is committed:
//!
//! * cached result blobs and graph renders — output, which spec §6 keeps out of
//!   the source and C19 keeps out of the committed sidecar;
//! * measured card heights — they depend on font size and pane width, so one
//!   person's numbers are wrong for everybody else;
//! * execution timestamps and staleness bookkeeping — they change on every run,
//!   and a committed file that changes on every run conflicts on every run.
//!
//! **Deleting this whole tree loses nothing.** Section names and collapse intent
//! live in the durable sidecar; results are recomputed by re-running.
//!
//! # What this module does and does not own
//!
//! It owns the *shape* of the tree and the self-ignoring `.gitignore`, plus a
//! small JSON document for the UI-side scratch state that has no reason to be a
//! database row. The two `redb` stores named in A4 —
//! `engine/session.redb` (written only by the `stratum serve` process) and
//! `ui/ui.redb` (written only by the desktop process) — are opened by their
//! owning processes; this module only guarantees the directories exist and that
//! neither ever lands inside a commit.

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use stratum_proto::CodeHash;

use crate::write::write_bytes_atomic;

/// The project-relative root of everything volatile.
pub const CACHE_ROOT: &str = ".stratum";

/// Schema of [`CacheSidecar`]. Versioned separately from the durable sidecar,
/// because this one may be thrown away at any time and that one may not.
pub const SCHEMA: u32 = 1;

/// Where the volatile state for one document lives.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CachePaths {
    /// `<project>/.stratum`
    pub stratum: Utf8PathBuf,
    /// `<project>/.stratum/cache/<blake3-of-abs-path>`
    pub root: Utf8PathBuf,
    /// The engine process's directory. Holds `session.redb`.
    pub engine: Utf8PathBuf,
    /// The desktop process's directory. Holds `ui.redb` and [`CacheSidecar`].
    pub ui: Utf8PathBuf,
}

impl CachePaths {
    /// Resolve the cache paths for `doc` inside `project_root`.
    ///
    /// The directory name is blake3 of the document's **absolute** path, so two
    /// files with the same name in different directories never share a cache and
    /// the name is filesystem-safe whatever the document is called.
    pub fn for_document(project_root: &Utf8Path, doc: &Utf8Path) -> Self {
        let abs = if doc.is_absolute() {
            doc.to_owned()
        } else {
            project_root.join(doc)
        };
        // Hashed over a separator-normalised spelling: on Windows `join` itself
        // inserts `\`, and `a\b` and `a/b` name one file, which must be one
        // cache. On Unix a filename can contain a literal `\`; folding it means
        // such a file shares a cache with its `/` twin — for a throwaway cache,
        // the right side of that trade.
        let key = blake3::hash(abs.as_str().replace('\\', "/").as_bytes()).to_hex();
        let stratum = project_root.join(CACHE_ROOT);
        let root = stratum.join("cache").join(&key[..32]);
        CachePaths {
            engine: root.join("engine"),
            ui: root.join("ui"),
            stratum,
            root,
        }
    }

    /// Create the tree, and the self-ignoring `.gitignore` if it is missing.
    ///
    /// The `.gitignore` containing `*` is the `target/` trick: the directory
    /// excludes itself, so nobody has to remember to add it to the project's
    /// `.gitignore`, and a researcher who clones the repo never finds somebody
    /// else's cached graph renders in their history.
    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.engine)?;
        std::fs::create_dir_all(&self.ui)?;
        let ignore = self.stratum.join(".gitignore");
        if !ignore.exists() {
            write_bytes_atomic(&ignore, b"*\n")?;
        }
        Ok(())
    }

    /// The JSON scratch document.
    pub fn scratch(&self) -> Utf8PathBuf {
        self.ui.join("cache.json")
    }
}

/// A measured inline-result card height, in CSS pixels.
///
/// Keyed by code hash rather than by `BlockId` so it survives a reconcile, and
/// by pane width so a measurement taken in a 340 px pane is not reused in a
/// 900 px one — reusing it is how a card opens at the wrong height and then
/// jumps.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeasuredHeight {
    /// The block this height was measured for.
    #[serde(with = "crate::sidecar_durable::hex::one")]
    pub block_hash: CodeHash,
    /// Pane width the measurement was taken at, in CSS pixels.
    pub pane_width: u32,
    /// The measured height, in CSS pixels.
    pub height: u32,
}

/// UI-side volatile state that does not need a database.
///
/// Deliberately small. Anything that grows without bound (transcripts, result
/// blobs, graph renders) belongs in `ui.redb`, which has eviction; a JSON file
/// that is rewritten in full on every change does not.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheSidecar {
    /// Always [`SCHEMA`].
    pub schema: u32,
    /// Measured card heights.
    pub measured_heights: Vec<MeasuredHeight>,
    /// Scroll offset of the editor pane, in lines.
    pub scroll_line: u32,
}

impl Default for CacheSidecar {
    fn default() -> Self {
        CacheSidecar {
            schema: SCHEMA,
            measured_heights: Vec::new(),
            scroll_line: 0,
        }
    }
}

impl CacheSidecar {
    /// Read the scratch document, or return the default.
    ///
    /// **A corrupt cache is not an error.** Unlike the durable sidecar, nothing
    /// here is irreplaceable, so a file we cannot parse is discarded rather than
    /// escalated: the alternative is refusing to open a document because a
    /// measured card height did not deserialise.
    pub fn load(paths: &CachePaths) -> Self {
        std::fs::read(paths.scratch())
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or_default()
    }

    /// Write the scratch document, creating the tree if needed.
    pub fn save(&self, paths: &CachePaths) -> std::io::Result<()> {
        paths.ensure()?;
        let mut s = serde_json::to_string(self).expect("CacheSidecar is always encodable");
        s.push('\n');
        write_bytes_atomic(&paths.scratch(), s.as_bytes())
    }

    /// Record a measurement, replacing any previous one for the same block and
    /// pane width.
    pub fn record_height(&mut self, m: MeasuredHeight) {
        match self
            .measured_heights
            .iter_mut()
            .find(|x| x.block_hash == m.block_hash && x.pane_width == m.pane_width)
        {
            Some(slot) => *slot = m,
            None => self.measured_heights.push(m),
        }
    }

    /// The best available height estimate for a block at a given pane width.
    pub fn height_for(&self, block: CodeHash, pane_width: u32) -> Option<u32> {
        self.measured_heights
            .iter()
            .find(|x| x.block_hash == block && x.pane_width == pane_width)
            .map(|x| x.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u8) -> CodeHash {
        CodeHash([n; 16])
    }

    #[test]
    fn two_documents_with_the_same_name_get_different_caches() {
        let root = Utf8Path::new("/p");
        let a = CachePaths::for_document(root, Utf8Path::new("/p/a/analysis.do"));
        let b = CachePaths::for_document(root, Utf8Path::new("/p/b/analysis.do"));
        assert_ne!(a.root, b.root);
    }

    #[test]
    fn a_relative_document_path_resolves_against_the_project_root() {
        let root = Utf8Path::new("/p");
        let a = CachePaths::for_document(root, Utf8Path::new("analysis.do"));
        let b = CachePaths::for_document(root, Utf8Path::new("/p/analysis.do"));
        assert_eq!(a.root, b.root);
    }

    /// The backslash literal is what a Windows caller produces (`join` inserts
    /// `\` there); on Unix it is a filename byte, folded by the same rule — so
    /// this asserts the key's separator-blindness on every host.
    #[test]
    fn the_cache_key_ignores_separator_spelling() {
        let root = Utf8Path::new("/p");
        let a = CachePaths::for_document(root, Utf8Path::new(r"sub\analysis.do"));
        let b = CachePaths::for_document(root, Utf8Path::new("sub/analysis.do"));
        assert_eq!(a.root, b.root);
    }

    #[test]
    fn ensure_writes_the_self_ignoring_gitignore() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let p = CachePaths::for_document(&root, Utf8Path::new("analysis.do"));
        p.ensure().unwrap();
        assert_eq!(
            std::fs::read_to_string(p.stratum.join(".gitignore")).unwrap(),
            "*\n"
        );
        assert!(p.engine.is_dir() && p.ui.is_dir());
    }

    #[test]
    fn a_corrupt_cache_is_discarded_not_escalated() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let p = CachePaths::for_document(&root, Utf8Path::new("analysis.do"));
        p.ensure().unwrap();
        std::fs::write(p.scratch(), b"{not json").unwrap();
        assert_eq!(CacheSidecar::load(&p), CacheSidecar::default());
    }

    #[test]
    fn heights_are_keyed_by_block_and_pane_width() {
        let mut c = CacheSidecar::default();
        c.record_height(MeasuredHeight {
            block_hash: h(1),
            pane_width: 340,
            height: 120,
        });
        c.record_height(MeasuredHeight {
            block_hash: h(1),
            pane_width: 900,
            height: 80,
        });
        c.record_height(MeasuredHeight {
            block_hash: h(1),
            pane_width: 340,
            height: 130,
        });
        assert_eq!(c.measured_heights.len(), 2);
        assert_eq!(c.height_for(h(1), 340), Some(130));
        assert_eq!(c.height_for(h(1), 900), Some(80));
        assert_eq!(c.height_for(h(2), 340), None);
    }

    #[test]
    fn scratch_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let p = CachePaths::for_document(&root, Utf8Path::new("analysis.do"));
        let mut c = CacheSidecar {
            scroll_line: 42,
            ..Default::default()
        };
        c.record_height(MeasuredHeight {
            block_hash: h(3),
            pane_width: 340,
            height: 99,
        });
        c.save(&p).unwrap();
        assert_eq!(CacheSidecar::load(&p), c);
    }
}
