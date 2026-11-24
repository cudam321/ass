//! macOS application plumbing: the `NSApplication` subclass CEF requires, the
//! app delegate, and the main menu.

use crate::handler::Handler;
use crate::shell;
use crate::tabs;
use crate::test_window;
use cef::application_mac::{CefAppProtocol, CrAppControlProtocol, CrAppProtocol};
use objc2::{
    ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, extern_methods,
    msg_send,
    rc::Retained,
    runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, ProtocolObject, Sel},
    sel,
};
use objc2_app_kit::{
    NSApp, NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate,
    NSApplicationTerminateReply, NSEvent, NSEventModifierFlags, NSMenu, NSMenuItem,
};
use objc2_foundation::{NSObjectNSThreadPerformAdditions, NSString};
use std::cell::Cell;

define_class! {
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    pub struct AssAppDelegate;

    impl AssAppDelegate {
        /// Finish application setup on the UI thread.
        #[unsafe(method(createApplication:))]
        unsafe fn create_application(&self, _object: Option<&AnyObject>) {
            let mtm = MainThreadMarker::new().expect("Not running on the main thread");
            let app = NSApp(mtm);
            assert!(app.isKindOfClass(AssApplication::class()));
            assert!(
                app.delegate()
                    .unwrap()
                    .isKindOfClass(AssAppDelegate::class())
            );

            build_main_menu(mtm, &app, self);
        }

        /// File > New Tab (Cmd+T): the shell page takes the address.
        #[unsafe(method(newTab:))]
        fn new_tab(&self, _sender: Option<&AnyObject>) {
            shell::new_tab();
        }

        /// File > Open Location (Cmd+L): the shell page's address input.
        #[unsafe(method(openLocation:))]
        fn open_location(&self, _sender: Option<&AnyObject>) {
            shell::focus_address();
        }

        /// File > Close Tab (Cmd+W): the active tab; nothing with no tab.
        #[unsafe(method(closeTab:))]
        fn close_tab(&self, _sender: Option<&AnyObject>) {
            tabs::close_active();
        }

        /// View > Reload Page (Cmd+R).
        #[unsafe(method(reloadPage:))]
        fn reload_page(&self, _sender: Option<&AnyObject>) {
            let _ = tabs::go(tabs::Go::Reload);
        }

        /// History > Back (Cmd+[).
        #[unsafe(method(goBack:))]
        fn go_back(&self, _sender: Option<&AnyObject>) {
            let _ = tabs::go(tabs::Go::Back);
        }

        /// History > Forward (Cmd+]).
        #[unsafe(method(goForward:))]
        fn go_forward(&self, _sender: Option<&AnyObject>) {
            let _ = tabs::go(tabs::Go::Forward);
        }

        /// Tab > Show Next Tab (Ctrl+Tab).
        #[unsafe(method(showNextTab:))]
        fn show_next_tab(&self, _sender: Option<&AnyObject>) {
            tabs::activate_next(true);
        }

        /// Tab > Show Previous Tab (Ctrl+Shift+Tab).
        #[unsafe(method(showPreviousTab:))]
        fn show_previous_tab(&self, _sender: Option<&AnyObject>) {
            tabs::activate_next(false);
        }

        /// Tab > Tab 1 to Tab 8 and Last Tab (Cmd+1 to Cmd+9). The item's tag
        /// is the number.
        #[unsafe(method(selectTabByNumber:))]
        fn select_tab_by_number(&self, sender: Option<&AnyObject>) {
            if let Some(item) = sender.and_then(|sender| sender.downcast_ref::<NSMenuItem>()) {
                tabs::activate_number(item.tag());
            }
        }
    }

    unsafe impl NSObjectProtocol for AssAppDelegate {}

    unsafe impl NSApplicationDelegate for AssAppDelegate {
        #[unsafe(method(applicationShouldTerminate:))]
        unsafe fn application_should_terminate(&self, _sender: &NSApplication) -> NSApplicationTerminateReply {
            NSApplicationTerminateReply::TerminateNow
        }

        /// Called when the user clicks the app dock icon while the application is
        /// already running.
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        unsafe fn application_should_handle_reopen(&self, _sender: &NSApplication, _has_visible_windows: Bool) -> Bool {
            // The tabs lock is released before the Handler lock is taken.
            if !tabs::is_quitting()
                && let Some(handler) = Handler::instance()
            {
                let mut handler = handler.lock().expect("Failed to lock Handler");
                handler.show_main_window();
            }
            Bool::NO
        }

        /// Requests that any state restoration archive be created with secure encoding
        /// (macOS 12+ only). See https://crrev.com/c737387656 for details. This also
        /// fixes an issue with macOS default behavior incorrectly restoring windows
        /// after hard reset (holding down the power button).
        #[unsafe(method(applicationSupportsSecureRestorableState:))]
        unsafe fn application_supports_secure_restorable_state(&self, _sender: &NSApplication) -> Bool {
            Bool::YES
        }

        /// Test-only: shows the main window held back until launch finished
        /// (`test_window::hold_show`). Does nothing outside test window mode.
        #[unsafe(method(applicationDidFinishLaunching:))]
        unsafe fn application_did_finish_launching(&self, _notification: &AnyObject) {
            if test_window::launch_finished()
                && !tabs::is_quitting()
                && let Some(handler) = Handler::instance()
            {
                let mut handler = handler.lock().expect("Failed to lock Handler");
                handler.show_main_window();
            }
        }
    }
}

impl AssAppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = AssAppDelegate::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

/// Instance variables of `AssApplication`.
#[derive(Default)]
pub struct AssApplicationIvars {
    handling_send_event: Cell<Bool>,
}

define_class!(
    /// A `NSApplication` subclass that implements the required CEF protocols.
    ///
    /// This class provides the necessary `CefAppProtocol` conformance to
    /// ensure that events are handled correctly by the Chromium framework on macOS.
    #[unsafe(super(NSApplication))]
    #[ivars = AssApplicationIvars]
    pub struct AssApplication;

    impl AssApplication {
        #[unsafe(method(sendEvent:))]
        unsafe fn send_event(&self, event: &NSEvent) {
            let was_sending_event = self.is_handling_send_event();
            if !was_sending_event {
                self.set_handling_send_event(true);
            }

            let _: () = msg_send![super(self), sendEvent:event];

            if !was_sending_event {
                self.set_handling_send_event(false);
            }
        }

        /// `-terminate:` is the entry point for orderly "quit" operations in Cocoa:
        /// the Quit menu item and Cmd+Q, the Dock menu, logout and shutdown.
        ///
        /// The default implementation ends the process with `exit()` and never
        /// leaves the main run loop. Chromium needs to leave the run loop to shut
        /// down cleanly, so instead this asks the tabs to close, one at a time
        /// (`tabs::quit`). Then the window closes with the shell, the last
        /// browser; `Handler::on_before_close` quits the message loop and
        /// `main()` runs the normal shutdown.
        #[unsafe(method(terminate:))]
        unsafe fn terminate(&self, _sender: &AnyObject) {
            tabs::quit();
        }

        /// Chromium sends `-cancelTerminate:` to the application whenever the
        /// user answers "Stay" on a leave-site prompt, whether or not a quit
        /// was in progress. Without this method that message ends the process
        /// (`unrecognized selector`).
        ///
        /// It ends a quit in progress: the tabs not yet asked stay open. The
        /// shell is asked only once no tab is left, so the window stays as it
        /// was.
        #[unsafe(method(cancelTerminate:))]
        unsafe fn cancel_terminate(&self, _sender: Option<&AnyObject>) {
            tabs::quit_cancelled();
        }
    }

    unsafe impl CrAppControlProtocol for AssApplication {
        #[unsafe(method(setHandlingSendEvent:))]
        unsafe fn _set_handling_send_event(&self, handling_send_event: Bool) {
            self.ivars().handling_send_event.set(handling_send_event);
        }
    }

    unsafe impl CrAppProtocol for AssApplication {
        #[unsafe(method(isHandlingSendEvent))]
        unsafe fn _is_handling_send_event(&self) -> Bool {
            self.ivars().handling_send_event.get()
        }
    }

    unsafe impl CefAppProtocol for AssApplication {}
);

impl AssApplication {
    extern_methods! {
        #[unsafe(method(sharedApplication))]
        fn shared_application() -> Retained<Self>;

        #[unsafe(method(setHandlingSendEvent:))]
        fn set_handling_send_event(&self, handling_send_event: bool);

        #[unsafe(method(isHandlingSendEvent))]
        fn is_handling_send_event(&self) -> bool;
    }
}

define_class!(
    /// Test-only: the shared application in test window mode, and nowhere
    /// else (see `test_window.rs`). The messages Chromium sends to `NSApp` to
    /// bring the app to the front do nothing.
    #[unsafe(super(AssApplication))]
    pub struct AssAccessoryApplication;

    impl AssAccessoryApplication {
        #[unsafe(method(activateIgnoringOtherApps:))]
        unsafe fn activate_ignoring_other_apps(&self, _ignore_other_apps: Bool) {}

        #[unsafe(method(activate))]
        unsafe fn activate(&self) {}
    }
);

impl AssAccessoryApplication {
    extern_methods! {
        #[unsafe(method(sharedApplication))]
        fn shared_application() -> Retained<Self>;
    }
}

/// Must run before anything else touches `NSApp`, so the shared application is
/// an `AssApplication`.
pub fn setup_application() {
    // Test-only: the first `sharedApplication` call picks the class.
    if test_window::enabled() {
        let _ = AssAccessoryApplication::shared_application();
    }
    let _ = AssApplication::shared_application();

    assert!(
        NSApp(MainThreadMarker::new().expect("Not running on the main thread"))
            .isKindOfClass(AssApplication::class())
    );

    // Test-only: set before any window exists, so the app gets no Dock icon
    // and no menu bar.
    if test_window::enabled() {
        let app = NSApp(MainThreadMarker::new().expect("Not running on the main thread"));
        assert!(
            app.setActivationPolicy(NSApplicationActivationPolicy::Accessory),
            "cannot set the activation policy"
        );
    }
}

pub fn setup_app_delegate() -> Retained<AssAppDelegate> {
    let mtm = MainThreadMarker::new().expect("Not running on the main thread");

    let delegate = AssAppDelegate::new(mtm);
    let delegate_proto =
        ProtocolObject::<dyn NSApplicationDelegate>::from_retained(delegate.clone());
    let app = NSApp(mtm);
    assert!(app.isKindOfClass(AssApplication::class()));
    app.setDelegate(Some(&delegate_proto));

    unsafe {
        delegate.performSelectorOnMainThread_withObject_waitUntilDone(
            sel!(createApplication:),
            None,
            false,
        );
    }

    delegate
}

/// The main menu, built in code (no `.xib`, so no Xcode needed).
/// Standard selectors travel the responder chain, so Cut/Copy/Paste reach the
/// focused web content.
fn build_main_menu(mtm: MainThreadMarker, app: &NSApplication, delegate: &AssAppDelegate) {
    let main_menu = NSMenu::new(mtm);

    let app_menu = add_submenu(mtm, &main_menu, "Ass");
    add_item(
        &app_menu,
        "About Ass",
        sel!(orderFrontStandardAboutPanel:),
        "",
    );
    app_menu.addItem(&NSMenuItem::separatorItem(mtm));
    add_item(&app_menu, "Hide Ass", sel!(hide:), "h");
    add_item(&app_menu, "Show All", sel!(unhideAllApplications:), "");
    app_menu.addItem(&NSMenuItem::separatorItem(mtm));
    add_item(&app_menu, "Quit Ass", sel!(terminate:), "q");

    // Sent to the app delegate directly, not along the responder chain.
    let file_menu = add_submenu(mtm, &main_menu, "File");
    add_action(&file_menu, "New Tab", sel!(newTab:), "t", delegate);
    add_action(
        &file_menu,
        "Open Location…",
        sel!(openLocation:),
        "l",
        delegate,
    );
    file_menu.addItem(&NSMenuItem::separatorItem(mtm));
    add_action(&file_menu, "Close Tab", sel!(closeTab:), "w", delegate);
    add_item(&file_menu, "Close Window", sel!(performClose:), "W");

    let edit_menu = add_submenu(mtm, &main_menu, "Edit");
    add_item(&edit_menu, "Undo", sel!(undo:), "z");
    add_item(&edit_menu, "Redo", sel!(redo:), "Z");
    edit_menu.addItem(&NSMenuItem::separatorItem(mtm));
    add_item(&edit_menu, "Cut", sel!(cut:), "x");
    add_item(&edit_menu, "Copy", sel!(copy:), "c");
    add_item(&edit_menu, "Paste", sel!(paste:), "v");
    add_item(&edit_menu, "Select All", sel!(selectAll:), "a");

    let view_menu = add_submenu(mtm, &main_menu, "View");
    add_action(&view_menu, "Reload Page", sel!(reloadPage:), "r", delegate);

    let history_menu = add_submenu(mtm, &main_menu, "History");
    add_action(&history_menu, "Back", sel!(goBack:), "[", delegate);
    add_action(&history_menu, "Forward", sel!(goForward:), "]", delegate);

    let tab_menu = add_submenu(mtm, &main_menu, "Tab");
    let next = add_action(
        &tab_menu,
        "Show Next Tab",
        sel!(showNextTab:),
        "\t",
        delegate,
    );
    next.setKeyEquivalentModifierMask(NSEventModifierFlags::Control);
    let previous = add_action(
        &tab_menu,
        "Show Previous Tab",
        sel!(showPreviousTab:),
        "\t",
        delegate,
    );
    previous
        .setKeyEquivalentModifierMask(NSEventModifierFlags::Control | NSEventModifierFlags::Shift);
    tab_menu.addItem(&NSMenuItem::separatorItem(mtm));
    for number in 1..=9 {
        let title = match number {
            9 => "Last Tab".to_owned(),
            _ => format!("Tab {number}"),
        };
        let key = number.to_string();
        let item = add_action(&tab_menu, &title, sel!(selectTabByNumber:), &key, delegate);
        item.setTag(number);
    }

    let window_menu = add_submenu(mtm, &main_menu, "Window");
    add_item(&window_menu, "Minimize", sel!(performMiniaturize:), "m");
    add_item(&window_menu, "Zoom", sel!(performZoom:), "");

    app.setMainMenu(Some(&main_menu));
    app.setWindowsMenu(Some(&window_menu));
}

fn add_submenu(mtm: MainThreadMarker, parent: &NSMenu, title: &str) -> Retained<NSMenu> {
    let item = NSMenuItem::new(mtm);
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
    item.setSubmenu(Some(&menu));
    parent.addItem(&item);
    menu
}

fn add_item(menu: &NSMenu, title: &str, action: Sel, key: &str) {
    // SAFETY: every action passed in is a standard AppKit selector.
    unsafe {
        menu.addItemWithTitle_action_keyEquivalent(
            &NSString::from_str(title),
            Some(action),
            &NSString::from_str(key),
        );
    }
}

/// An item whose action is one of the app delegate's own methods.
fn add_action(
    menu: &NSMenu,
    title: &str,
    action: Sel,
    key: &str,
    delegate: &AssAppDelegate,
) -> Retained<NSMenuItem> {
    // SAFETY: every action passed in is a method `AssAppDelegate` defines,
    // taking the sender. The menu holds its target weakly; the delegate lives
    // until the process exits (`main`).
    unsafe {
        let item = menu.addItemWithTitle_action_keyEquivalent(
            &NSString::from_str(title),
            Some(action),
            &NSString::from_str(key),
        );
        let target: &AnyObject = delegate;
        item.setTarget(Some(target));
        item
    }
}
