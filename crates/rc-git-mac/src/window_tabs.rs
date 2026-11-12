//! Puts a newly opened repository window into the tab group of the window in front, as the
//! Swift app's `AppDelegate.newWindow` did with `addTabbedWindow(_:ordered:)`.
//!
//! GPUI sets each window's tabbing identifier but adds it as a tab only when the user's
//! system-wide "Prefer tabs when opening documents" setting is "Always". rc-git always wants
//! tabs, so the app snapshots the windows before it opens one ([`TabHost::before_open`]) and
//! joins the new one to the front window's group afterwards ([`TabHost::join`]).
//!
//! The `AppKit` calls go through the Objective-C runtime (`objc_msgSend`), with no binding
//! crate. Every call runs on the main thread, where the app opens its windows. With no
//! `NSApplication` (a headless test), both steps do nothing.

use std::ffi::{CStr, c_char, c_void};

type Id = *mut c_void;
type Sel = *const c_void;

#[link(name = "objc")]
unsafe extern "C" {
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_msgSend();
}

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    static NSApp: Id;
}

/// `NSWindowTabbingModePreferred`.
const TABBING_MODE_PREFERRED: isize = 1;
/// `NSWindowAbove`.
const ORDERED_ABOVE: isize = 1;

/// The windows of one tab group as they stood before a new window opened.
#[derive(Debug)]
pub struct TabHost {
    host: Option<Id>,
    before: Vec<Id>,
}

impl TabHost {
    /// The windows carrying `identifier` now, and the one a new window joins: the main window
    /// when it carries `identifier`, otherwise the last such window that is on screen. Call
    /// on the main thread, right before opening the window.
    #[must_use]
    pub fn before_open(identifier: &str) -> Self {
        let Some(app) = application() else {
            return Self {
                host: None,
                before: Vec::new(),
            };
        };
        let before = windows_with(app, identifier);
        // SAFETY: `app` is the live `NSApplication`; `mainWindow` takes no arguments and
        // answers an `NSWindow` or nil.
        let main = unsafe { send_id(app, c"mainWindow") };
        let host = if !main.is_null() && before.contains(&main) {
            Some(main)
        } else {
            before
                .iter()
                .rev()
                .copied()
                .find(|window| is_visible(*window))
        };
        Self { host, before }
    }

    /// Adds the window that opened since [`Self::before_open`] (the one carrying `identifier`
    /// that was not there) to the host's tab group and brings it forward. Does nothing when
    /// there was no host, which is the first window, or no new window. Call on the main thread.
    pub fn join(self, identifier: &str) {
        let (Some(app), Some(host)) = (application(), self.host) else {
            return;
        };
        let Some(new) = windows_with(app, identifier)
            .into_iter()
            .find(|window| !self.before.contains(window))
        else {
            return;
        };
        // SAFETY: `host` and `new` are live `NSWindow`s from `-[NSApplication windows]`, read on
        // the main thread in this same turn; each selector takes the argument types passed.
        unsafe {
            send_void_isize(new, c"setTabbingMode:", TABBING_MODE_PREFERRED);
            send_void_id_isize(host, c"addTabbedWindow:ordered:", new, ORDERED_ABOVE);
            send_void_id(new, c"makeKeyAndOrderFront:", std::ptr::null_mut());
        }
    }
}

/// The running `NSApplication`, or `None` before `AppKit` started one.
fn application() -> Option<Id> {
    // SAFETY: `NSApp` is AppKit's global; it is nil until `NSApplication` is created and only
    // read here, on the main thread.
    let app = unsafe { NSApp };
    (!app.is_null()).then_some(app)
}

/// Every window of `app` whose tabbing identifier is `identifier`, in the app's window order.
fn windows_with(app: Id, identifier: &str) -> Vec<Id> {
    let mut found = Vec::new();
    // SAFETY: `windows` answers an `NSArray` of `NSWindow`; `count` and `objectAtIndex:` read
    // it within bounds, and `tabbingIdentifier` answers an `NSString` or nil.
    unsafe {
        let windows = send_id(app, c"windows");
        if windows.is_null() {
            return found;
        }
        let count = send_usize(windows, c"count");
        for index in 0..count {
            let window = send_id_usize(windows, c"objectAtIndex:", index);
            let tabbing = send_id(window, c"tabbingIdentifier");
            if !window.is_null() && string(tabbing).as_deref() == Some(identifier) {
                found.push(window);
            }
        }
    }
    found
}

fn is_visible(window: Id) -> bool {
    // SAFETY: `window` is a live `NSWindow`; `isVisible` answers a `BOOL`.
    unsafe { send_bool(window, c"isVisible") }
}

/// The text of an `NSString`, or `None` for nil.
///
/// # Safety
/// `value` is nil or a live `NSString`.
unsafe fn string(value: Id) -> Option<String> {
    if value.is_null() {
        return None;
    }
    // SAFETY: `UTF8String` answers a NUL-terminated buffer the string owns, valid while it
    // lives; it is copied out before returning.
    let text = unsafe { send_ptr(value, c"UTF8String") };
    if text.is_null() {
        return None;
    }
    // SAFETY: as above: non-null and NUL-terminated.
    Some(
        unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned(),
    )
}

/// `objc_msgSend` as a function of the given signature.
macro_rules! message {
    ($name:ident, ($($arg:ident: $ty:ty),*) -> $ret:ty) => {
        /// Sends `selector` to `receiver`.
        ///
        /// # Safety
        /// `receiver` is a live object that answers `selector` with this signature.
        unsafe fn $name(receiver: Id, selector: &CStr $(, $arg: $ty)*) -> $ret {
            // SAFETY: `objc_msgSend` takes the receiver, the selector, then the method's own
            // arguments, and must be called through the method's exact signature; the caller
            // vouches for that signature.
            unsafe {
                let send = std::mem::transmute::<
                    unsafe extern "C" fn(),
                    unsafe extern "C" fn(Id, Sel $(, $ty)*) -> $ret,
                >(objc_msgSend);
                send(receiver, sel_registerName(selector.as_ptr()) $(, $arg)*)
            }
        }
    };
}

message!(send_id, () -> Id);
message!(send_ptr, () -> *const c_char);
message!(send_usize, () -> usize);
message!(send_bool, () -> bool);
message!(send_id_usize, (index: usize) -> Id);
message!(send_void_isize, (value: isize) -> ());
message!(send_void_id, (value: Id) -> ());
message!(send_void_id_isize, (value: Id, order: isize) -> ());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_an_application_both_steps_do_nothing() {
        let host = TabHost::before_open("rc-git.repository");
        assert!(host.host.is_none());
        assert!(host.before.is_empty());
        host.join("rc-git.repository");
    }
}
