//! Test-only window mode, so app tests do not take the screen or the keyboard.
//!
//! # How it is turned on
//!
//! Only by `ASS_TEST_WINDOW=invisible` in the process environment, with
//! `ASS_PROFILE_DIR` naming a folder that exists and is not the default profile
//! folder. It is read once, in `main`, before `NSApp` or any window exists.
//! Nothing a page, the shell page or `ass://shell/api/` sends can turn it on or
//! off. When it is off, every function here returns without doing anything.
//! When it is on, stderr gets the line `Ass: test window mode: invisible`.
//!
//! When `ASS_TEST_WINDOW` is set and not empty but the mode cannot run (an
//! unknown value, `ASS_PROFILE_DIR` missing or empty, or naming the default
//! profile folder), stderr gets the reason and the process exits with code 64
//! before `NSApp` exists: nothing is shown, no profile is opened, the keychain
//! is not touched. So a test that asks for the mode gets the mode or no app:
//! never a visible app, never the real profile. An empty or missing
//! `ASS_TEST_WINDOW` is a normal launch. `main` refuses the mode the same way,
//! right after `init`, when the command line lacks `--use-mock-keychain`, so
//! that a test does not reach the real keychain (what that rests on:
//! `keychain.rs`).
//!
//! The folder check catches mistakes, not attacks. The default folder is the
//! one `main` would use, found through `HOME`, and also the one under the
//! account's home directory as the system has it; folders are compared by
//! device and inode, which is why the folder must exist (`profile.rs`; `main`
//! has refused a folder that does not, before `init`).
//! And every profile's cookies are encrypted with the same keychain item
//! (`Ass Safe Storage`), so a copy of the real profile in any other folder
//! decrypts the same way: the check keeps a test off the real profile, not the
//! real profile's data from a test.
//!
//! # What it changes
//!
//! Every window is created, laid out, shown, handed off and closed by the same
//! code as for users. The mode changes five things and nothing else:
//!
//! 1. The activation policy is Accessory: no Dock icon, no menu bar
//!    (`mac::setup_application`).
//! 2. `NSApp` is `AssAccessoryApplication`, whose `activateIgnoringOtherApps:`
//!    and `activate` do nothing. They are the calls Chromium makes before
//!    `makeKeyAndOrderFront:` on every `show()` (Chromium
//!    `native_widget_ns_window_bridge.mm:1076-1083`). Accessory apps may still
//!    be activated in code or by a click (`NSRunningApplication.h:38`): this
//!    stops the app's own code, and point 3 stops clicks. Another process can
//!    still activate the app (`open`, AppleScript); the tests' probe
//!    (`scripts/appprobe.swift`) catches that.
//! 3. Before each `show()`, the window's `alphaValue` is 0 and it ignores mouse
//!    events (`prepare`). It is really ordered in, so its compositor runs
//!    (`native_widget_mac_ns_window_host.mm:1132-1157` draws only while
//!    `isVisible`). Chromium itself keeps ordered-in windows invisible this way
//!    (bridge `:1036-1047`), and it keeps an alpha set before the first show:
//!    it saves the current value and restores that value when the first frame
//!    arrives (bridge `:1044`, `:2063-2065`). So the alpha must be set before the
//!    first `show()`, never after it.
//! 4. The main window's first `show()` waits until launch has finished
//!    (`hold_show`, `launch_finished`). Users get that window shown inside
//!    `initialize`, before the run loop starts. Here `on_window_created`
//!    returns without showing it, and the app delegate's
//!    `applicationDidFinishLaunching:` shows it through
//!    `Handler::show_main_window`, which calls `prepare` first.
//!
//!    Why: macOS activates an app at the end of launch if one of its windows
//!    was ordered in before launch finished, even with points 1 to 3. A bare
//!    AppKit program showed this, and the same window shown from
//!    `applicationDidFinishLaunching:` was not activated (`docs/ARCHITECTURE.md`,
//!    verified fact 21). With this point the app was never activated
//!    (verified fact 22).
//!
//!    The order comes from the structure, not from timing. The main window,
//!    both of its browsers and the hold all happen inside `initialize`: CEF
//!    calls `on_context_initialized` there, synchronously, once the profile
//!    exists (CEF `ChromeBrowserContext::StoreOrTriggerInitCallback`). The
//!    callback can only run inside `[NSApp run]`, which Chromium's message pump
//!    enters from `run_message_loop` (base `MessagePumpNSApplication::DoRun`);
//!    `run` calls `finishLaunching` before its event loop (Apple's
//!    `NSApplication.run()` documentation). Measured: the callback came about
//!    20 ms (warm start) to 88 ms (cold) after `initialize` returned, and the
//!    window was shown about 14 ms after the callback (fact 22).
//!
//!    If the callback finds no browser in the `Handler`'s list, it shows
//!    nothing: a held window stays hidden and the smoke test's window check
//!    fails. A second launch handing off before the callback is held too
//!    (`Handler::show_main_window`); the callback then shows the window. After
//!    the callback, every window is shown at once.
//! 5. Before each `show()`, the window is put above normal windows (`LEVEL`) and
//!    on every desktop (`BEHAVIOR`), by `prepare`. Why: at the normal level,
//!    coming back to the window's desktop made macOS activate the app, and a
//!    window of the user's over ours made macOS count ours as covered, so a tab
//!    opened behind was never told that it is hidden. A bare AppKit program
//!    showed both, and with these two settings neither, over 14 desktop changes
//!    (`docs/ARCHITECTURE.md`, known issues 23 and 24). They are set on the
//!    `NSWindow`, never through CEF's `set_always_on_top`: that path in
//!    Chromium adds a collection behaviour that conflicts with ours and
//!    rebuilds the page's tracking area (bridge `:2006-2016`). After each
//!    `show()`, `shown` prints what the window really has, and the smoke test
//!    reads it.
//!
//! The window stays where users get it. It is not moved off the displays:
//! moving it there before the first `show()` did not hold (it ended at `0,33`,
//! `docs/ARCHITECTURE.md` verified fact 17), and in the same run the system
//! slid windows past the display edge when the user switched apps.
//!
//! # What the tests must pass
//!
//! `--disable-backgrounding-occluded-windows`. macOS does not promise how it
//! reports a transparent window (`NSWindow.h:186`), and a window covered by
//! other windows or on a sleeping display counts as occluded too (Chromium
//! `web_contents_occlusion_checker_mac.mm:509-521`); an occluded page counts as
//! hidden and is throttled. With that switch Chromium
//! treats occluded as visible (Chromium `web_contents_impl.cc:12773-12781`).
//! It does not change hidden.
//!
//! # What it does not cover
//!
//! The first window shown inside `initialize`, as users get it: the mode shows
//! it later (point 4).
//!
//! Windows and panels this app does not create itself are shown as for users:
//! JavaScript dialogs and the leave-site prompt (Chrome's tab-modal dialogs),
//! `<select>` and context menus, file and color pickers, print dialogs, page
//! popups that are separate windows (date and time pickers, `<datalist>`,
//! autofill), picture-in-picture, fullscreen, and any window a DevTools client
//! opens. So are the system prompts the app can trigger: keychain, local
//! network, camera, and the crash dialog.
//!
//! No test run has covered popups or those windows in this mode yet.

use crate::profile;
use cef::*;
use objc2::{msg_send, rc::Retained, runtime::AnyObject};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: OnceLock<bool> = OnceLock::new();
/// Set once `applicationDidFinishLaunching:` has run, in test window mode only.
static LAUNCHED: AtomicBool = AtomicBool::new(false);

/// Reads the mode from the environment. Call once, at startup, before anything
/// creates `NSApp` or a window. Returns false when the mode is asked for and
/// refused: the reason is on stderr, and the caller must end the process at
/// once.
pub fn init() -> bool {
    let mode = read();
    assert!(
        ENABLED.set(mode == Ok(true)).is_ok(),
        "test window mode read twice"
    );
    match mode {
        Ok(true) => log_line!("Ass: test window mode: invisible"),
        Ok(false) => {}
        Err(reason) => {
            log_line!("Ass: ASS_TEST_WINDOW refused: {reason}");
            return false;
        }
    }
    true
}

/// Whether the mode is on (false also when `init` was never called).
pub fn enabled() -> bool {
    ENABLED.get().copied().unwrap_or(false)
}

/// `Ok(true)` when the mode is on, `Ok(false)` when it is not asked for, and
/// the reason when it is asked for and refused. The profile folder is looked
/// at only when the mode is asked for (`profile::is_default`).
fn read() -> Result<bool, &'static str> {
    let Some(value) = std::env::var_os("ASS_TEST_WINDOW").filter(|value| !value.is_empty()) else {
        return Ok(false);
    };
    if value != "invisible" {
        return Err("unrecognised value");
    }
    if std::env::var_os("ASS_PROFILE_DIR").is_none_or(|dir| dir.is_empty()) {
        return Err("ASS_PROFILE_DIR is not set");
    }
    if profile::is_default() {
        return Err("ASS_PROFILE_DIR is the default profile folder");
    }
    Ok(true)
}

/// Whether the first `show()` of the main window must wait: true in test
/// window mode until launch has finished. See point 4 of the module doc.
pub fn hold_show() -> bool {
    enabled() && !LAUNCHED.load(Ordering::Relaxed)
}

/// Call from `applicationDidFinishLaunching:`. In test window mode, records
/// that launch has finished and returns true: the caller then shows the main
/// window that `hold_show` held back. Outside the mode, returns false.
pub fn launch_finished() -> bool {
    if !enabled() {
        return false;
    }
    LAUNCHED.store(true, Ordering::Relaxed);
    true
}

/// The window's level in test window mode: `NSModalPanelWindowLevel`
/// (`NSWindow.h`), above normal windows, so that none of the user's covers it,
/// and below the Dock (20) and the menu bar (24). Not the floating level (3):
/// at exactly that level Chromium gives the page hover events while the app is
/// inactive (`bridged_content_view.mm:446-459`).
const LEVEL: isize = 8;

/// The window's collection behaviour in test window mode, as one exact value
/// (`NSWindow.h`, `NSWindowCollectionBehavior`): on every desktop (1 << 0),
/// stationary (1 << 4), out of the window cycle (1 << 6), allowed beside
/// full-screen windows (1 << 8) and in every app's full-screen space (1 << 18).
/// It replaces the value CEF's window has (full screen primary, 1 << 7), which
/// excludes the fourth.
const BEHAVIOR: usize = 0x40151;

/// The `NSWindow` of a CEF `Window`.
fn ns_window(window: &Window) -> Retained<AnyObject> {
    // SAFETY: on macOS the handle of a CEF `Window` is the content view of its
    // `NSWindow` (CEF `libcef/browser/views/view_util_mac.mm`,
    // `GetWindowHandle`), alive while the window is. CEF calls us on its UI
    // thread, which is the main thread.
    let view = unsafe { window.window_handle().cast::<AnyObject>().as_ref() }
        .expect("test window mode: the window has no view");
    // SAFETY: `-[NSView window]` takes no arguments and returns an `NSWindow`
    // or nil.
    let ns_window: Option<Retained<AnyObject>> = unsafe { msg_send![view, window] };
    ns_window.expect("test window mode: the view has no window")
}

/// In test window mode, makes `window` fully transparent, lets mouse events
/// pass through it, and puts it above normal windows on every desktop. Call
/// right before every `show()` of a window we create; see points 3 and 5 of the
/// module doc, and point 3 for why it must come before the first one.
pub fn prepare(window: &Window) {
    if !enabled() {
        return;
    }
    let ns_window = ns_window(window);
    // SAFETY: four `NSWindow` property setters (`NSWindow.h`); `alphaValue` is
    // a `CGFloat`, which is `f64` on 64-bit macOS, `level` an `NSInteger` and
    // `collectionBehavior` an `NSUInteger`.
    unsafe {
        let _: () = msg_send![&*ns_window, setAlphaValue: 0.0_f64];
        let _: () = msg_send![&*ns_window, setIgnoresMouseEvents: true];
        let _: () = msg_send![&*ns_window, setLevel: LEVEL];
        let _: () = msg_send![&*ns_window, setCollectionBehavior: BEHAVIOR];
    }
}

/// In test window mode, prints what `window` really has, read back from its
/// `NSWindow`. Call right after every `show()` that `prepare` came before: the
/// smoke test fails unless every such line carries the values `prepare` set,
/// so a change in CEF or Chromium that resets one of them shows at once.
pub fn shown(window: &Window) {
    if !enabled() {
        return;
    }
    let ns_window = ns_window(window);
    // SAFETY: the getters of the four properties `prepare` sets.
    let (level, behavior, ignores, alpha): (isize, usize, bool, f64) = unsafe {
        (
            msg_send![&*ns_window, level],
            msg_send![&*ns_window, collectionBehavior],
            msg_send![&*ns_window, ignoresMouseEvents],
            msg_send![&*ns_window, alphaValue],
        )
    };
    log_line!(
        "Ass: test window shown: level {level}, behaviour {behavior:#x}, ignores the mouse {ignores}, alpha {alpha}"
    );
}
