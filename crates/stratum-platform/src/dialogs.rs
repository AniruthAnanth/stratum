//! File dialogs and "open this elsewhere" — 08 §5.4.
//!
//! The three pickers are `async` because macOS `NSOpenPanel` has to run on the
//! main thread and the Linux portal is inherently asynchronous; `#[async_trait]`
//! rather than a native `async fn` because [`crate::Platform::dialogs`] hands
//! back a `&dyn FileDialogs` and a native async fn in a trait is not
//! dyn-compatible.
//!
//! **`Cancelled` is returned, never an empty `Vec`.** A cancelled Open and an
//! Open that selected nothing are different events: one leaves the current
//! document alone, the other is impossible. Collapsing them is how a "Save As"
//! that the user escaped out of ends up silently discarding a buffer.

use camino::{Utf8Path, Utf8PathBuf};

use crate::{PlatformError, Result};

/// An opaque native window handle, used to parent a dialog so it appears as a
/// sheet (macOS) or a modal owned by the right window (Windows/Linux).
///
/// Deliberately just an integer: `NSWindow*`, `HWND` and an X11 `Window` /
/// Wayland surface id all fit, and the trait crate must not name any of those
/// types. The desktop obtains it from its window and passes it through.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct WindowHandle(pub u64);

/// One entry in a dialog's format dropdown.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FileFilter {
    /// Shown to the user: "Stata do-files".
    pub name: String,
    /// Extensions WITHOUT the leading dot: `["do", "ado"]`.
    pub extensions: Vec<String>,
}

impl FileFilter {
    /// Convenience constructor; strips a leading dot so both spellings work.
    #[must_use]
    pub fn new(name: impl Into<String>, extensions: &[&str]) -> Self {
        Self {
            name: name.into(),
            extensions: extensions
                .iter()
                .map(|e| e.trim_start_matches('.').to_owned())
                .collect(),
        }
    }
}

/// An Open panel request.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct OpenRequest {
    /// Panel title. Ignored by platforms that do not show one.
    pub title: String,
    /// Where to start. `None` means "wherever the OS last left the user".
    pub start_dir: Option<Utf8PathBuf>,
    /// Format filters, first is selected.
    pub filters: Vec<FileFilter>,
    /// Allow a multiple selection.
    pub multiple: bool,
    /// Parent window, for sheet presentation.
    pub parent: Option<WindowHandle>,
}

/// A Save panel request.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SaveRequest {
    /// Panel title.
    pub title: String,
    /// Where to start.
    pub start_dir: Option<Utf8PathBuf>,
    /// Pre-filled file name, extension included.
    pub suggested_name: Option<String>,
    /// Format filters.
    pub filters: Vec<FileFilter>,
    /// Parent window.
    pub parent: Option<WindowHandle>,
}

/// A "choose a directory" request.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct FolderRequest {
    /// Panel title.
    pub title: String,
    /// Where to start.
    pub start_dir: Option<Utf8PathBuf>,
    /// Parent window.
    pub parent: Option<WindowHandle>,
}

/// A URL that [`FileDialogs::open_external`] is allowed to hand to the OS.
///
/// 08 §5.4 writes this parameter as `&Url`. We use a validating newtype
/// instead, for two reasons. The dependency reason: the `url` crate is not in
/// the workspace table and pulls an IDNA/ICU surface that this crate, whose
/// acceptance bullet is "zero OS deps", has no use for. The better reason:
/// `open_external` is a *URL handler launcher*. Handing an unvalidated string
/// to `open`/`ShellExecute`/`xdg-open` means any scheme registered on the
/// machine can be invoked from anything that can produce a link — a rendered
/// help page, an AI response, a `.do` file's comment. The allow-list here is
/// the security boundary; the confirmation the desktop shows before navigating
/// (CONTRACTS §11, `platform_open_external`) is the second one.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ExternalUrl(String);

impl ExternalUrl {
    /// Schemes we will ever ask the OS to open.
    pub const ALLOWED_SCHEMES: [&'static str; 3] = ["https", "http", "mailto"];

    /// Validate a URL for external navigation.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] when the scheme is not one of
    /// [`ExternalUrl::ALLOWED_SCHEMES`], when the string carries a control
    /// character or whitespace (the classic way to smuggle a second argument
    /// past a shell-adjacent handler), or when it has no scheme at all.
    pub fn parse(raw: &str) -> Result<Self> {
        if raw.is_empty() || raw.len() > 8192 {
            return Err(PlatformError::Unsupported("URL is empty or absurdly long"));
        }
        if raw
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '"' || c == '\'')
        {
            return Err(PlatformError::Unsupported(
                "URL contains whitespace, quotes or control characters",
            ));
        }
        let Some((scheme, rest)) = raw.split_once(':') else {
            return Err(PlatformError::Unsupported("URL has no scheme"));
        };
        let scheme = scheme.to_ascii_lowercase();
        if !Self::ALLOWED_SCHEMES.contains(&scheme.as_str()) {
            return Err(PlatformError::Unsupported(
                "only https, http and mailto URLs may be opened externally",
            ));
        }
        if rest.is_empty() {
            return Err(PlatformError::Unsupported("URL has no body"));
        }
        Ok(Self(raw.to_owned()))
    }

    /// The validated URL.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ExternalUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Native file panels plus the two synchronous "hand this to the OS" verbs.
#[async_trait::async_trait]
pub trait FileDialogs: Send + Sync {
    /// Show an Open panel.
    ///
    /// # Errors
    /// [`PlatformError::Cancelled`] when the user dismissed it — never an empty
    /// `Vec`. [`PlatformError::Unsupported`] on a headless session.
    async fn open_files(&self, req: OpenRequest) -> Result<Vec<Utf8PathBuf>>;

    /// Show a Save panel. The returned path may not exist yet.
    ///
    /// # Errors
    /// [`PlatformError::Cancelled`] when dismissed.
    async fn save_file(&self, req: SaveRequest) -> Result<Utf8PathBuf>;

    /// Show a folder picker.
    ///
    /// # Errors
    /// [`PlatformError::Cancelled`] when dismissed.
    async fn pick_folder(&self, req: FolderRequest) -> Result<Utf8PathBuf>;

    /// Select the path in Finder / Explorer / the desktop's file manager.
    ///
    /// # Errors
    /// [`PlatformError::Io`] when the path does not exist,
    /// [`PlatformError::Unsupported`] with no file manager.
    fn reveal(&self, path: &Utf8Path) -> Result<()>;

    /// Hand a URL to the user's browser or mail client.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] when there is no handler.
    fn open_external(&self, url: &ExternalUrl) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_three_schemes() {
        for u in [
            "https://example.invalid/docs",
            "http://localhost:1420/",
            "mailto:support@example.invalid",
            "HTTPS://EXAMPLE.INVALID",
        ] {
            assert!(ExternalUrl::parse(u).is_ok(), "{u}");
        }
    }

    /// Every one of these is a real way a URL handler has been abused. They are
    /// `Unsupported`, not `Os`: nothing went wrong, we simply will not do it.
    #[test]
    fn refuses_everything_else() {
        for u in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "vscode://file/etc/passwd",
            "data:text/html,<script>",
            "smb://host/share",
            "stratum-asset://localhost/frame/1",
            "https://example.com/ a",
            "https://example.com/\nX-Injected: 1",
            "notaurl",
            "https:",
            "",
        ] {
            let err = ExternalUrl::parse(u).unwrap_err();
            assert!(err.is_unsupported(), "{u} -> {err}");
        }
    }

    #[test]
    fn filter_extensions_lose_the_dot() {
        let f = FileFilter::new("Stata do-files", &[".do", "ado"]);
        assert_eq!(f.extensions, ["do", "ado"]);
    }
}
