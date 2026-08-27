//! Am I running inside a `.app`?
//!
//! Three of the adapters need this and none of them can guess. macOS refuses
//! `UNUserNotificationCenter` to a process with no main-bundle identifier — not
//! with an error, with an Objective-C exception, which aborts — and the update
//! strategy is `AppBundleSwap` only when there is a bundle to swap. `cargo run`
//! and `cargo test` are never bundled, so the unbundled branch is the one every
//! developer and every CI job exercises; it has to be a first-class
//! [`stratum_platform::PlatformError::Unsupported`], not a crash.

use camino::Utf8PathBuf;
use core_foundation::base::TCFType;
use core_foundation::bundle::CFBundle;
use core_foundation::string::{CFString, CFStringRef};

/// The main bundle's `CFBundleIdentifier`, or `None` when this process is a
/// bare executable.
///
/// `CFBundleGetMainBundle` succeeds for an unbundled binary too — it
/// synthesises a bundle rooted at the executable's directory — so the presence
/// of the *identifier* is the check, not the presence of the bundle.
#[must_use]
pub fn identifier() -> Option<String> {
    let bundle = CFBundle::main_bundle();
    // SAFETY: `CFBundleGetIdentifier` returns a +0 CFString or NULL.
    let raw: CFStringRef = unsafe { CFBundleGetIdentifier(bundle.as_concrete_TypeRef()) };
    if raw.is_null() {
        return None;
    }
    Some(unsafe { CFString::wrap_under_get_rule(raw) }.to_string())
}

/// True when this process is inside an app bundle.
#[must_use]
pub fn is_bundled() -> bool {
    identifier().is_some()
}

/// The `.app` directory itself, e.g. `/Applications/Stratum.app`.
///
/// Derived from the executable path rather than from `CFBundle::path`, because
/// the latter answers for the synthesised bundle of an unbundled binary as
/// well and would hand back `target/debug`.
#[must_use]
pub fn app_bundle_path() -> Option<Utf8PathBuf> {
    if !is_bundled() {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    let exe = Utf8PathBuf::from_path_buf(exe).ok()?;
    // …/Stratum.app/Contents/MacOS/stratum
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    (macos.file_name() == Some("MacOS")
        && contents.file_name() == Some("Contents")
        && app.extension() == Some("app"))
    .then(|| app.to_owned())
}

extern "C" {
    fn CFBundleGetIdentifier(bundle: core_foundation::bundle::CFBundleRef) -> CFStringRef;
}
