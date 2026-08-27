//! The dock tile badge.
//!
//! Split out of [`crate::notify`] because it is AppKit rather than
//! `UserNotifications`, and because it is the one call in this crate that MUST
//! happen on the main thread: `NSApplication.sharedApplication` creates the
//! application object if it does not exist, and doing that from a worker is
//! undefined. `dispatch_async` to the main queue keeps the caller thread-free
//! without blocking it — a badge is advisory, so fire-and-forget is right.

use objc2_app_kit::NSApplication;
use objc2_foundation::{MainThreadMarker, NSString};
use stratum_platform::Result;

/// Set or clear the dock badge.
///
/// # Errors
/// Never today: the call is dispatched to the main queue and cannot report
/// back. The signature keeps [`stratum_platform::Notifier::set_badge`]'s
/// contract intact for the platforms where it can fail.
pub fn set_badge_label(label: Option<String>) -> Result<()> {
    let apply = move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        let tile = app.dockTile();
        // `setBadgeLabel:` takes a nullable NSString; the main-thread
        // requirement is discharged by `MainThreadMarker` above.
        tile.setBadgeLabel(label.as_deref().map(NSString::from_str).as_deref());
    };

    if MainThreadMarker::new().is_some() {
        apply();
    } else {
        dispatch_to_main(apply);
    }
    Ok(())
}

/// Run `f` on the main queue. One tiny `dispatch_async` shim rather than a
/// dependency: `dispatch2` would be a third way of reaching libdispatch in a
/// tree that already has two.
fn dispatch_to_main<F: FnOnce() + Send + 'static>(f: F) {
    use std::ffi::c_void;

    extern "C" {
        static _dispatch_main_q: [u64; 0];
        fn dispatch_async_f(
            queue: *const c_void,
            context: *mut c_void,
            work: extern "C" fn(*mut c_void),
        );
    }

    extern "C" fn trampoline<F: FnOnce()>(ctx: *mut c_void) {
        // SAFETY: `ctx` is the Box we leaked below, and libdispatch calls this
        // exactly once.
        let f: Box<F> = unsafe { Box::from_raw(ctx.cast()) };
        f();
    }

    let ctx = Box::into_raw(Box::new(f)).cast::<c_void>();
    // SAFETY: `_dispatch_main_q` is libdispatch's main queue object; passing
    // its address is how the C macro `dispatch_get_main_queue()` expands.
    unsafe {
        dispatch_async_f(
            std::ptr::addr_of!(_dispatch_main_q).cast(),
            ctx,
            trampoline::<F>,
        );
    }
}
