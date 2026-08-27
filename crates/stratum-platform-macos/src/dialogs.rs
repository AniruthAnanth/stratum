//! macOS file panels, Reveal in Finder, and Open in browser — 08 §5.4.
//!
//! `NSOpenPanel`/`NSSavePanel` are reached through `rfd`, which is the
//! `objc2-app-kit` implementation 08 §5.4 prescribes plus the main-queue
//! marshalling those panels require (they must run on the main thread, and the
//! engine calls this from a worker). `deny.toml` names `rfd` with this crate in
//! its `wrappers` list, so the dependency is the sanctioned one rather than a
//! shortcut.
//!
//! `parent` is not yet honoured: sheet presentation needs a
//! `raw-window-handle` from the Tauri window, which the desktop obtains and
//! this layer has no way to synthesise. The panels are application-modal in the
//! meantime, which is correct behaviour, just not the nicest.

use camino::{Utf8Path, Utf8PathBuf};
use rfd::AsyncFileDialog;
use stratum_platform::{
    ExternalUrl, FileDialogs, FolderRequest, OpenRequest, PlatformError, Result, SaveRequest,
};

/// [`FileDialogs`] for macOS.
#[derive(Clone, Copy, Debug, Default)]
pub struct MacosFileDialogs;

impl MacosFileDialogs {
    /// Construct.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl FileDialogs for MacosFileDialogs {
    async fn open_files(&self, req: OpenRequest) -> Result<Vec<Utf8PathBuf>> {
        let mut d = AsyncFileDialog::new().set_title(&req.title);
        if let Some(dir) = &req.start_dir {
            d = d.set_directory(dir);
        }
        for f in &req.filters {
            d = d.add_filter(&f.name, &f.extensions);
        }

        let picked = if req.multiple {
            d.pick_files().await
        } else {
            d.pick_file().await.map(|h| vec![h])
        };
        // `None` is a cancel. An empty selection is not a thing NSOpenPanel can
        // produce, and if it ever were it would still be a cancel — see the
        // trait docs for why the two must not collapse.
        let Some(handles) = picked.filter(|h| !h.is_empty()) else {
            return Err(PlatformError::Cancelled);
        };
        handles.iter().map(|h| utf8(h.path())).collect()
    }

    async fn save_file(&self, req: SaveRequest) -> Result<Utf8PathBuf> {
        let mut d = AsyncFileDialog::new().set_title(&req.title);
        if let Some(dir) = &req.start_dir {
            d = d.set_directory(dir);
        }
        if let Some(name) = &req.suggested_name {
            d = d.set_file_name(name);
        }
        for f in &req.filters {
            d = d.add_filter(&f.name, &f.extensions);
        }
        match d.save_file().await {
            Some(h) => utf8(h.path()),
            None => Err(PlatformError::Cancelled),
        }
    }

    async fn pick_folder(&self, req: FolderRequest) -> Result<Utf8PathBuf> {
        let mut d = AsyncFileDialog::new().set_title(&req.title);
        if let Some(dir) = &req.start_dir {
            d = d.set_directory(dir);
        }
        match d.pick_folder().await {
            Some(h) => utf8(h.path()),
            None => Err(PlatformError::Cancelled),
        }
    }

    fn reveal(&self, path: &Utf8Path) -> Result<()> {
        // Absolute on purpose: `open` would read a leading `-` as a flag, and
        // `reveal` is reachable from a rendered help page's link.
        let abs = std::fs::canonicalize(path)?;
        let abs = Utf8PathBuf::from_path_buf(abs)
            .map_err(|_| PlatformError::Unsupported("path is not valid UTF-8"))?;
        run_open(&["-R", abs.as_str()])
    }

    fn open_external(&self, url: &ExternalUrl) -> Result<()> {
        // `ExternalUrl` has already rejected every scheme but https/http/mailto
        // and every string containing whitespace or quotes, and `Command` does
        // not go through a shell, so there is nothing left to escape.
        run_open(&[url.as_str()])
    }
}

fn utf8(p: &std::path::Path) -> Result<Utf8PathBuf> {
    Utf8PathBuf::from_path_buf(p.to_path_buf())
        .map_err(|_| PlatformError::Unsupported("path is not valid UTF-8"))
}

fn run_open(args: &[&str]) -> Result<()> {
    let status = std::process::Command::new("/usr/bin/open")
        .args(args)
        .status()?;
    if status.success() {
        return Ok(());
    }
    Err(PlatformError::Os {
        code: i64::from(status.code().unwrap_or(-1)),
        message: format!("/usr/bin/open {} failed", args.join(" ")),
    })
}
