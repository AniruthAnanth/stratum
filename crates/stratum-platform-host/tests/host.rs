//! `host()` is one platform, built once, for this OS.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use stratum_platform::{CredentialBackend, PlatformId};

#[test]
fn the_host_is_this_os_and_is_a_singleton() {
    let a = stratum_platform_host::host();
    let b = stratum_platform_host::host();
    assert_eq!(a.id(), PlatformId::HOST);
    assert!(std::ptr::eq(
        std::ptr::from_ref::<dyn stratum_platform::Platform>(a).cast::<u8>(),
        std::ptr::from_ref::<dyn stratum_platform::Platform>(b).cast::<u8>(),
    ));

    // Everything the aggregate promises is reachable through the singleton.
    assert!(a.processes().physical_cores() >= 1);
    assert!(!a.paths().config_dir().as_str().is_empty());
    #[cfg(target_os = "macos")]
    assert_eq!(a.credentials().backend(), CredentialBackend::MacosKeychain);
    let _ = CredentialBackend::EncryptedFile;
}

/// Building the singleton twice is a programming error with a visible
/// consequence — the second caller's menu sink and update feed would be
/// silently dropped — so it is reported rather than ignored.
#[test]
fn init_after_host_is_refused() {
    let _ = stratum_platform_host::host();
    let Err(err) = stratum_platform_host::init(stratum_platform_host::HostConfig::default()) else {
        panic!("init() after host() must be refused");
    };
    assert!(matches!(
        err,
        stratum_platform::PlatformError::BackendUnavailable(_)
    ));
}
