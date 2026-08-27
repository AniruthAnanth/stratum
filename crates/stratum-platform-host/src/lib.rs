//! `host() -> &'static dyn Platform` — 08 §5.1.
//!
//! One process, one platform, selected at compile time and built once. The
//! `OnceLock` is not a lazy-initialisation convenience: [`Platform`] is
//! `Send + Sync + 'static` and is handed out as a shared borrow to the engine
//! supervisor, the menu code and the AI credential path simultaneously, so a
//! process-lifetime singleton is the only shape that does not require every
//! consumer to hold an `Arc`.
//!
//! # Configuration
//!
//! [`host`] builds a platform with no injected parts: the built-in keymap
//! tables, no menu sink, no update feed. That is the right default for the CLI
//! and for tests. The desktop calls [`init`] once, before anything else, with
//! the sink and feed it can provide; calling [`host`] first and [`init`] after
//! is a programming error and [`init`] says so rather than silently ignoring
//! the configuration.

use std::sync::OnceLock;

use stratum_platform::{Platform, PlatformError, Result};

// One alias pair per shipped OS. The three `*Config` types are field-for-field
// identical by design (see `WindowsConfig`'s own doc comment), which is what
// lets the desktop's startup code build `HostConfig` once instead of three
// times behind its own `cfg`s.
#[cfg(target_os = "macos")]
pub use stratum_platform_macos::{MacosConfig as HostConfig, MacosPlatform as HostPlatform};

#[cfg(target_os = "windows")]
pub use stratum_platform_windows::{WindowsConfig as HostConfig, WindowsPlatform as HostPlatform};

#[cfg(target_os = "linux")]
pub use stratum_platform_linux::{LinuxConfig as HostConfig, LinuxPlatform as HostPlatform};

// Still a hard error off the three, and deliberately not a stub that compiles:
// every `Platform` method below the alias is a credential store, a file dialog
// or an updater, and a fourth OS silently getting no-ops for all three is how an
// installer ships around a keychain that does not exist. Adding one means a new
// `crates/stratum-platform-<os>` plus two lines here and two in Cargo.toml.
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
compile_error!(
    "stratum-platform-host has no implementation for this target. The product \
     ships macOS, Windows and Linux (08 §1); a fourth needs its own \
     crates/stratum-platform-<os>, a cfg-gated path dependency in Cargo.toml and \
     the matching `HostPlatform`/`HostConfig` aliases here."
);

static HOST: OnceLock<HostPlatform> = OnceLock::new();

/// The process's platform adapter, built with defaults on first use.
///
/// # Panics
/// If the platform cannot be constructed — today that means the home directory
/// could not be resolved. Everything below this call has been handed a
/// `&dyn Platform` since startup, so there is no useful recovery: use [`init`]
/// at startup if you need to report the failure to a human.
#[must_use]
pub fn host() -> &'static dyn Platform {
    try_host().unwrap_or_else(|e| panic!("the platform adapter could not be built: {e}"))
}

/// [`host`], without the panic.
///
/// # Errors
/// Whatever [`HostPlatform::new`] reports.
pub fn try_host() -> Result<&'static dyn Platform> {
    if let Some(p) = HOST.get() {
        return Ok(p);
    }
    let built = HostPlatform::new(HostConfig::default())?;
    // Losing the race is fine: the other thread's platform is equivalent, and
    // the one we just built is dropped.
    let _ = HOST.set(built);
    HOST.get()
        .map(|p| p as &'static dyn Platform)
        .ok_or_else(|| {
            PlatformError::BackendUnavailable("the platform singleton vanished".to_owned())
        })
}

/// Build the platform with injected parts. Call once, at startup, before
/// [`host`].
///
/// # Errors
/// [`PlatformError::BackendUnavailable`] if the singleton is already built —
/// which means someone called [`host`] first and is holding a platform without
/// the menu sink or update feed this call was going to supply. Silently
/// ignoring that produces an application whose menu bar never installs and
/// whose Check for Updates does nothing, with no error anywhere.
pub fn init(config: HostConfig) -> Result<&'static dyn Platform> {
    let built = HostPlatform::new(config)?;
    HOST.set(built).map_err(|_| {
        PlatformError::BackendUnavailable(
            "the platform singleton was already built; init() must run before host()".to_owned(),
        )
    })?;
    host_built()
}

fn host_built() -> Result<&'static dyn Platform> {
    HOST.get()
        .map(|p| p as &'static dyn Platform)
        .ok_or_else(|| {
            PlatformError::BackendUnavailable("the platform singleton vanished".to_owned())
        })
}
