//! Shape assertions that only a compiler can make.
//!
//! Every trait in this layer is reached through `&dyn`, because
//! [`Platform`] hands out borrowed trait objects; a signature that quietly
//! breaks dyn-compatibility (a generic method, a native `async fn`, `Self:
//! Sized`) would not be caught by `cargo build` until the impl crate tried to
//! coerce it, in a different unit, weeks later.

use stratum_platform::{
    CredentialStore, FileDialogs, Keymap, MenuHost, Notifier, Platform, PlatformError, ProcessHost,
    ShellIntegration, SupervisedChild, UpdateFeed, Updater,
};

#[allow(dead_code)]
struct EveryTraitObject<'a> {
    platform: &'a dyn Platform,
    credentials: &'a dyn CredentialStore,
    dialogs: &'a dyn FileDialogs,
    menus: &'a dyn MenuHost,
    updater: &'a dyn Updater,
    feed: &'a dyn UpdateFeed,
    shell: &'a dyn ShellIntegration,
    processes: &'a dyn ProcessHost,
    notifier: &'a dyn Notifier,
    keymap: &'a dyn Keymap,
    child: Box<dyn SupervisedChild>,
}

const fn assert_send_sync<T: Send + Sync>() {}
const fn assert_send<T: Send>() {}

#[test]
fn adapters_can_cross_a_thread_boundary() {
    // The engine supervisor lives on a worker; the menu host is touched from
    // the main thread. Anything less than Send + Sync makes that a refactor.
    assert_send_sync::<&dyn Platform>();
    assert_send_sync::<&dyn CredentialStore>();
    assert_send_sync::<&dyn FileDialogs>();
    assert_send_sync::<&dyn MenuHost>();
    assert_send_sync::<&dyn Updater>();
    assert_send_sync::<&dyn ShellIntegration>();
    assert_send_sync::<&dyn ProcessHost>();
    assert_send_sync::<&dyn Notifier>();
    // A child is owned by one supervisor at a time, so Send is enough and Sync
    // would be a lie about `&mut self` stdio.
    assert_send::<Box<dyn SupervisedChild>>();
    // An error crossing a channel back to the UI is the normal case.
    assert_send_sync::<PlatformError>();
}
