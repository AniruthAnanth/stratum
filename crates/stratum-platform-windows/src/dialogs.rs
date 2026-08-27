//! Windows file panels, Reveal in Explorer, and Open in browser — 08 §5.4.
//!
//! The three pickers go through `rfd`, which is `IFileOpenDialog` /
//! `IFileSaveDialog` — the *modern* dialogs 08 §5.4 prescribes, "not the legacy
//! `GetOpenFileName`, because the modern dialog gets long-path and library
//! support" — plus the COM apartment initialisation and the modal message pump
//! they require. `deny.toml` names this crate in `rfd`'s `wrappers` list, so
//! the dependency is the sanctioned one rather than a shortcut.
//!
//! `parent` is not yet honoured: an owner `HWND` has to come from the Tauri
//! window, and this layer has no way to synthesise one. The dialogs are
//! application-modal in the meantime, which is correct behaviour, just not the
//! nicest.
//!
//! # `explorer.exe /select,` is a raw argument, and that is not an oversight
//!
//! Explorer does not parse its command line the way `CommandLineToArgvW` does.
//! `/select,C:\path\file.do` is one token to it, comma included, and the path
//! must be quoted if it contains a space — but `std::process::Command`'s own
//! quoting would wrap the *whole* token, producing `"/select,C:\a b\c.do"`,
//! which Explorer treats as a path and opens the user's Documents folder
//! instead. `raw_arg` is the only way to spell what Explorer actually wants.

use camino::Utf8Path;
use stratum_platform::{PlatformError, Result};

/// The single argument `explorer.exe` needs to select a file.
///
/// # Errors
/// [`PlatformError::Unsupported`] for a path containing `"` — impossible in a
/// real Windows filename, since `"` is one of the reserved characters, but this
/// string is spliced into a command line unescaped and the check is the reason
/// that is safe rather than merely likely to be safe.
pub fn reveal_argument(path: &Utf8Path) -> Result<String> {
    let p = path.as_str();
    if p.contains('"') || p.contains('\n') || p.contains('\r') {
        return Err(PlatformError::Unsupported(
            "a path containing a quote or a newline cannot be passed to Explorer",
        ));
    }
    Ok(format!("/select,\"{p}\""))
}

#[cfg(target_os = "windows")]
pub use sys::WindowsFileDialogs;

#[cfg(target_os = "windows")]
mod sys {
    use camino::{Utf8Path, Utf8PathBuf};
    use rfd::AsyncFileDialog;
    use stratum_platform::{
        ExternalUrl, FileDialogs, FolderRequest, OpenRequest, PlatformError, Result, SaveRequest,
    };
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    use super::reveal_argument;
    use crate::win;

    /// [`FileDialogs`] for Windows.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct WindowsFileDialogs;

    impl WindowsFileDialogs {
        /// Construct.
        #[must_use]
        pub const fn new() -> Self {
            Self
        }
    }

    #[async_trait::async_trait]
    impl FileDialogs for WindowsFileDialogs {
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
            // `None` is a cancel. An empty selection is not a thing
            // `IFileOpenDialog` can produce, and if it ever were it would still
            // be a cancel — see the trait docs for why the two must not
            // collapse.
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
            // Canonicalised first: a relative path selects nothing, and
            // `reveal` is reachable from a link in a rendered help page.
            let abs = std::fs::canonicalize(path)?;
            let abs = Utf8PathBuf::from_path_buf(abs)
                .map_err(|_| PlatformError::Unsupported("path is not valid UTF-8"))?;
            // `\\?\`-prefixed extended paths are what `canonicalize` produces on
            // Windows, and Explorer does not understand them.
            let plain = abs.as_str().strip_prefix(r"\\?\UNC\").map_or_else(
                || {
                    abs.as_str()
                        .strip_prefix(r"\\?\")
                        .unwrap_or(abs.as_str())
                        .to_owned()
                },
                |rest| format!(r"\\{rest}"),
            );
            let arg = reveal_argument(Utf8Path::new(&plain))?;

            use std::os::windows::process::CommandExt as _;
            let status = std::process::Command::new("explorer.exe")
                .raw_arg(&arg)
                .status()?;
            // Explorer's exit code is famously not a success indicator — it
            // returns 1 on a perfectly successful selection — so the only
            // failure worth reporting is one where the process could not be
            // started at all, which `status()?` has already covered.
            let _ = status;
            Ok(())
        }

        fn open_external(&self, url: &ExternalUrl) -> Result<()> {
            // `ExternalUrl` has already rejected every scheme but
            // https/http/mailto and every string containing whitespace, quotes
            // or control characters, which is what makes handing it to the
            // shell's URL handler safe.
            let verb = win::wide("open");
            let target = win::wide(url.as_str());
            // SAFETY: both buffers are NUL-terminated and outlive the call.
            let rc = unsafe {
                ShellExecuteW(
                    None,
                    PCWSTR(verb.as_ptr()),
                    PCWSTR(target.as_ptr()),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    SW_SHOWNORMAL,
                )
            };
            // The documented contract: a value greater than 32 is success, and
            // anything at or below it is an error code that happens to be
            // shaped like an HINSTANCE.
            let code = rc.0 as isize;
            if code > 32 {
                return Ok(());
            }
            Err(win::classify(
                i32::try_from(code).unwrap_or(-1),
                format!("ShellExecuteW could not open {url}"),
            ))
        }
    }

    fn utf8(p: &std::path::Path) -> Result<Utf8PathBuf> {
        Utf8PathBuf::from_path_buf(p.to_path_buf())
            .map_err(|_| PlatformError::Unsupported("path is not valid UTF-8"))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// The comma is inside the token and the quotes are inside the argument.
    /// `Command::arg` would produce `"/select,C:\a b\c.do"`, which Explorer
    /// reads as a path and answers by opening Documents.
    #[test]
    fn the_explorer_argument_quotes_the_path_and_not_the_switch() {
        assert_eq!(
            reveal_argument(Utf8Path::new(r"C:\My Data\wave 2.do")).unwrap(),
            "/select,\"C:\\My Data\\wave 2.do\""
        );
    }

    #[test]
    fn a_path_that_could_break_out_of_the_quoting_is_refused() {
        for bad in ["C:\\a\"b", "C:\\a\nb", "C:\\a\rb"] {
            assert!(reveal_argument(Utf8Path::new(bad)).is_err(), "{bad}");
        }
    }
}
