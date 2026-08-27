//! The project model — `workspace_load` / `workspace_save` / `doc_claim`.
//!
//! A Stratum project is **a directory with `.do` files in it**. There is no
//! project file you have to create, no import step, and nothing that stops
//! working if you move the folder: [`WorkspaceState`] is a small, optional,
//! committed preferences file, and everything in it has a working default.
//!
//! That is the same argument as ADR-010 one level up. A researcher's analysis
//! must not become trapped in *our* project format any more than in our notebook
//! format, so `.stratum/workspace.json` records preferences and never structure.
//! Delete it and you have the same project.

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use stratum_proto::{DocumentId, SessionId};

use crate::sidecar_cache::CACHE_ROOT;
use crate::write::write_bytes_atomic;

/// `WorkspaceState.schema`.
pub const SCHEMA: u32 = 1;

/// Committed project preferences.
///
/// Written to `.stratum/workspace.json`. Note that `.stratum/` also holds the
/// gitignored `cache/` tree — the `.gitignore` this crate writes there says `*`,
/// so a project that wants to commit its workspace state adds a
/// `!workspace.json` negation. That is deliberately the user's decision: a team
/// that shares an entry point wants it committed, and a solo researcher does not
/// care either way.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceState {
    /// Always [`SCHEMA`].
    pub schema: u32,
    /// **A23.** The `.do` that `RunIntent::ProjectEntryPoint` resolves to,
    /// project-relative. `None` means the project has no declared entry point
    /// and the run target falls back to the active document.
    ///
    /// *Ordering several* entry points is deferred to v1.1 (ARCHITECTURE §9);
    /// running *the* one is v1, which is why this is a single path and not a
    /// list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_point: Option<Utf8PathBuf>,
    /// The layout id this project opens with.
    pub layout: String,
    /// The keymap preset this project opens with.
    pub keymap: String,
    /// Documents open when the project was last closed, project-relative, in tab
    /// order.
    pub open_documents: Vec<Utf8PathBuf>,
}

impl Default for WorkspaceState {
    fn default() -> Self {
        WorkspaceState {
            schema: SCHEMA,
            entry_point: None,
            layout: crate::layout::Preset::Modern.id().to_owned(),
            keymap: crate::keymap::KeymapPreset::Modern.id().to_owned(),
            open_documents: Vec::new(),
        }
    }
}

/// What went wrong.
#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    /// `.stratum/workspace.json` exists but is not readable.
    #[error("{path} is not a readable workspace file: {source}")]
    Malformed {
        /// The file's path.
        path: Utf8PathBuf,
        /// The parse error.
        #[source]
        source: serde_json::Error,
    },
    /// The filesystem said no.
    #[error("{path}: {source}")]
    Io {
        /// Path involved.
        path: Utf8PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// A path escaped the project root.
    #[error("{path} is outside the project root")]
    Escapes {
        /// The offending path.
        path: Utf8PathBuf,
    },
}

/// One open project.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Project {
    /// Absolute path of the project directory.
    pub root: Utf8PathBuf,
    /// Its preferences.
    pub state: WorkspaceState,
}

/// `<root>/.stratum/workspace.json`.
pub fn state_path(root: &Utf8Path) -> Utf8PathBuf {
    root.join(CACHE_ROOT).join("workspace.json")
}

impl Project {
    /// `workspace_load { projectRoot }`.
    ///
    /// An absent state file is the normal case, not an error.
    pub fn load(root: &Utf8Path) -> Result<Self, ProjectError> {
        let path = state_path(root);
        let state = match std::fs::read(&path) {
            Ok(raw) => serde_json::from_slice(&raw)
                .map_err(|source| ProjectError::Malformed { path, source })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => WorkspaceState::default(),
            Err(source) => return Err(ProjectError::Io { path, source }),
        };
        Ok(Project {
            root: root.to_owned(),
            state,
        })
    }

    /// `workspace_save { projectRoot, state }`.
    pub fn save(&self) -> Result<Utf8PathBuf, ProjectError> {
        let path = state_path(&self.root);
        let dir = self.root.join(CACHE_ROOT);
        std::fs::create_dir_all(&dir).map_err(|source| ProjectError::Io {
            path: dir.clone(),
            source,
        })?;
        let ignore = dir.join(".gitignore");
        if !ignore.exists() {
            write_bytes_atomic(&ignore, b"*\n").map_err(|source| ProjectError::Io {
                path: ignore,
                source,
            })?;
        }
        let mut s =
            serde_json::to_string_pretty(&self.state).expect("WorkspaceState is always encodable");
        s.push('\n');
        write_bytes_atomic(&path, s.as_bytes()).map_err(|source| ProjectError::Io {
            path: path.clone(),
            source,
        })?;
        Ok(path)
    }

    /// Resolve a project-relative path, refusing anything that escapes the root.
    ///
    /// The check is lexical and runs on `..` components before touching the
    /// filesystem, because the caller may be resolving a path that came from a
    /// sidecar somebody else committed — and on whatever host they committed it
    /// from. So the refusals below are byte-oriented rather than delegated to
    /// the host's path parsing: `is_absolute("/x")` is false on Windows, `C:\x`
    /// is one ordinary filename on Unix, and a check that is only right on one
    /// host is not a check.
    pub fn resolve(&self, rel: &Utf8Path) -> Result<Utf8PathBuf, ProjectError> {
        let escapes = || ProjectError::Escapes {
            path: rel.to_owned(),
        };
        if rel.is_absolute() {
            return if rel.starts_with(&self.root) {
                Ok(rel.to_owned())
            } else {
                Err(escapes())
            };
        }
        // What this host calls relative can still re-anchor `join` on Windows:
        // `/x` and `\x` replace everything but the drive, `C:x` replaces the
        // whole path, and `\\server\share` reaches the network.
        let b = rel.as_str().as_bytes();
        if b.first().is_some_and(|&c| c == b'/' || c == b'\\')
            || (b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic())
        {
            return Err(escapes());
        }
        // Both separators on every host, not `components()`: on Unix `..\x` is
        // one filename, but the sidecar that names it may be opened on Windows
        // next, where it climbs. Refusing costs an edge-case filename;
        // honouring it costs the boundary.
        let mut depth = 0i32;
        for part in rel.as_str().split(['/', '\\']) {
            match part {
                "" | "." => {}
                ".." => {
                    depth -= 1;
                    if depth < 0 {
                        return Err(escapes());
                    }
                }
                _ => depth += 1,
            }
        }
        Ok(self.root.join(rel))
    }

    /// The absolute entry point, if one is configured (A23).
    pub fn entry_point(&self) -> Option<Utf8PathBuf> {
        self.state
            .entry_point
            .as_ref()
            .and_then(|p| self.resolve(p).ok())
    }
}

/// `doc_claim { session, path } -> Claim { ownerLabel }`.
///
/// **A `.do` is editable in exactly one window.** Two editors over one file with
/// independent undo stacks and independent save timing is a data-loss bug, not a
/// feature, and the second window discovers it by being told who holds the file
/// rather than by overwriting them.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Claim {
    /// The window label that holds the document.
    pub owner_label: String,
    /// True if the caller is the owner. `false` means "open it read-only and
    /// offer to focus `owner_label`".
    pub granted: bool,
}

/// Who currently owns which document, per project.
///
/// In-memory only: a claim is a fact about running windows, so persisting it
/// would leave a stale lock behind after a crash — the failure mode that makes
/// people delete lock files without reading them.
#[derive(Clone, Debug, Default)]
pub struct Claims {
    held: Vec<(Utf8PathBuf, String, SessionId, DocumentId)>,
}

impl Claims {
    /// An empty claim table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Claim `path` for `label`, or report who already has it.
    pub fn claim(
        &mut self,
        path: &Utf8Path,
        label: &str,
        session: SessionId,
        doc: DocumentId,
    ) -> Claim {
        if let Some((_, owner, ..)) = self.held.iter().find(|(p, ..)| p == path) {
            return Claim {
                granted: owner == label,
                owner_label: owner.clone(),
            };
        }
        self.held
            .push((path.to_owned(), label.to_owned(), session, doc));
        Claim {
            owner_label: label.to_owned(),
            granted: true,
        }
    }

    /// Release a claim — `doc_close`, or the window going away.
    pub fn release(&mut self, path: &Utf8Path) {
        self.held.retain(|(p, ..)| p != path);
    }

    /// Release every claim held by one window.
    pub fn release_window(&mut self, label: &str) {
        self.held.retain(|(_, l, ..)| l != label);
    }

    /// Who holds `path`, if anybody.
    pub fn owner(&self, path: &Utf8Path) -> Option<&str> {
        self.held
            .iter()
            .find(|(p, ..)| p == path)
            .map(|(_, l, ..)| l.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> (tempfile::TempDir, Utf8PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let p = Utf8PathBuf::from_path_buf(t.path().to_path_buf()).unwrap();
        (t, p)
    }

    #[test]
    fn a_directory_with_no_state_file_is_a_valid_project() {
        let (_t, root) = tmp();
        let p = Project::load(&root).unwrap();
        assert_eq!(p.state, WorkspaceState::default());
        assert_eq!(p.entry_point(), None);
    }

    #[test]
    fn state_round_trips_and_the_cache_dir_ignores_itself() {
        let (_t, root) = tmp();
        let mut p = Project::load(&root).unwrap();
        p.state.entry_point = Some(Utf8PathBuf::from("analysis.do"));
        p.state.open_documents = vec!["analysis.do".into(), "clean.do".into()];
        p.state.layout = "classic".into();
        p.save().unwrap();

        let back = Project::load(&root).unwrap();
        assert_eq!(back.state, p.state);
        assert_eq!(back.entry_point(), Some(root.join("analysis.do")));
        assert_eq!(
            std::fs::read_to_string(root.join(".stratum/.gitignore")).unwrap(),
            "*\n"
        );
    }

    #[test]
    fn a_path_that_escapes_the_root_is_refused() {
        let (_t, root) = tmp();
        let p = Project::load(&root).unwrap();
        assert!(p.resolve(Utf8Path::new("../../etc/passwd")).is_err());
        assert!(p.resolve(Utf8Path::new("a/../../b")).is_err());
        assert!(p.resolve(Utf8Path::new("a/../b")).is_ok());
        assert!(p.resolve(Utf8Path::new("/elsewhere/x.do")).is_err());
    }

    /// Every spelling here parses differently on the two hosts — on Unix each
    /// backslash form is one ordinary filename, on Windows it re-anchors
    /// `join` or climbs — so the refusals are asserted host-independently:
    /// they must hold wherever the test runs, and real Windows is the host
    /// they protect.
    #[test]
    fn windows_spellings_that_re_anchor_join_are_refused_on_every_host() {
        let (_t, root) = tmp();
        let p = Project::load(&root).unwrap();
        for rel in [
            r"\elsewhere\x.do",
            r"C:\elsewhere\x.do",
            "C:relative.do",
            r"\\server\share\x.do",
            r"..\..\etc\passwd",
            r"a\..\..\b",
        ] {
            assert!(p.resolve(Utf8Path::new(rel)).is_err(), "{rel}");
        }
        // The backslash spelling of a contained path still resolves.
        assert!(p.resolve(Utf8Path::new(r"sub\x.do")).is_ok());
    }

    #[test]
    fn a_document_is_editable_in_exactly_one_window() {
        let mut c = Claims::new();
        let path = Utf8Path::new("/p/analysis.do");
        let first = c.claim(path, "main:p", SessionId(1), DocumentId(1));
        assert!(first.granted);

        let second = c.claim(path, "editor:p:2", SessionId(1), DocumentId(2));
        assert!(!second.granted);
        assert_eq!(second.owner_label, "main:p");

        // Re-claiming from the owning window is idempotent, so a reload is not a
        // lockout.
        assert!(c.claim(path, "main:p", SessionId(1), DocumentId(1)).granted);

        c.release_window("main:p");
        assert_eq!(c.owner(path), None);
        assert!(
            c.claim(path, "editor:p:2", SessionId(1), DocumentId(2))
                .granted
        );
    }
}
