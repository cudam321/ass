//! Browser-process app: registers the internal scheme, then creates the main
//! window (the shell view and the first tab) once CEF is ready.

use ass::scheme::{self, SHELL_URL};
use cef::*;
use objc2::{msg_send, rc::Retained, runtime::AnyObject};
use objc2_app_kit::{NSApp, NSAppearanceNameAqua, NSAppearanceNameDarkAqua};
use objc2_foundation::{MainThreadMarker, NSArray, NSRect};
use std::cell::RefCell;

use crate::handler::{Handler, HandlerClient};
use crate::shell;
use crate::tabs;
use crate::test_window;

const HOME_URL: &str = "https://www.google.com/";
/// Width of the shell view, in density-independent pixels: the sidebar's 16-pixel margin, its
/// 244-pixel column and the 12-pixel gap to the page card (`design/shell-prototype.html`).
pub const SHELL_WIDTH: i32 = 272;
/// The window has no title bar. CEF still gives macOS this height for one, and macOS centres
/// the close, minimise and zoom buttons on half of it: 28, the middle of the sidebar's top row.
const TITLEBAR_HEIGHT: f32 = 56.0;
/// The margin around the page card, inside each tab's view: top, right and bottom, as in the
/// prototype. The gap on the card's left is the shell view's own.
pub const PAGE_MARGIN: Insets = Insets {
    top: 12,
    left: 0,
    bottom: 12,
    right: 16,
};
/// Size of the main window, in density-independent pixels.
const WINDOW_WIDTH: i32 = 1280;
const WINDOW_HEIGHT: i32 = 800;
/// The smallest the main window can be made.
const MIN_WINDOW_WIDTH: i32 = 760;
const MIN_WINDOW_HEIGHT: i32 = 480;

/// The page card's corner radius (`--radius` in the prototype).
const PAGE_RADIUS: f64 = 10.0;

/// Rounds the corners of the page card that `view` shows. CEF has no call for it and gives no
/// handle to the page's native view, so the view is found as the window's native subview that
/// covers the card exactly, and its layer is clipped to the radius. Public AppKit calls only.
pub fn round_page_card(window: &Window, view: &BrowserView) {
    let bounds = view.bounds();
    let card = (
        f64::from(bounds.x + PAGE_MARGIN.left),
        f64::from(bounds.y + PAGE_MARGIN.top),
        f64::from(bounds.width - PAGE_MARGIN.left - PAGE_MARGIN.right),
        f64::from(bounds.height - PAGE_MARGIN.top - PAGE_MARGIN.bottom),
    );
    // SAFETY: the handle of a CEF window is its NSWindow's content view; on the main thread.
    let Some(content) = (unsafe { window.window_handle().cast::<AnyObject>().as_ref() }) else {
        return;
    };
    unsafe {
        let frame: NSRect = msg_send![content, frame];
        // The content view's y axis runs up.
        let y = frame.size.height - card.1 - card.3;
        let subviews: Retained<NSArray<AnyObject>> = msg_send![content, subviews];
        for subview in subviews.iter() {
            let f: NSRect = msg_send![&*subview, frame];
            if (f.origin.x, f.origin.y, f.size.width, f.size.height) != (card.0, y, card.2, card.3)
            {
                continue;
            }
            let layer: Option<Retained<AnyObject>> = msg_send![&*subview, layer];
            if let Some(layer) = layer {
                let _: () = msg_send![&*layer, setCornerRadius: PAGE_RADIUS];
                let _: () = msg_send![&*layer, setMasksToBounds: true];
            }
        }
    }
}

/// The sidebar's ground (`--chrome` in `src/shell_ui/shell.css`) as ARGB, in the app's current
/// appearance. CEF paints it around the page card, so the margin and the sidebar are one surface.
pub fn chrome_color() -> u32 {
    // `color-mix(in srgb, #000 5%, #fff)` when light, `#000` when dark.
    if is_dark() { 0xFF00_0000 } else { 0xFFF2_F2F2 }
}

/// Light or dark, decided as Chromium decides it for the pages (`os_settings_provider_mac.mm`):
/// the app's effective appearance, matched against the two.
fn is_dark() -> bool {
    let mtm = MainThreadMarker::new().expect("Not running on the main thread");
    // SAFETY: AppKit's constant appearance names, read on the main thread.
    let (light, dark) = unsafe { (NSAppearanceNameAqua, NSAppearanceNameDarkAqua) };
    NSApp(mtm)
        .effectiveAppearance()
        .bestMatchFromAppearancesWithNames(&NSArray::from_slice(&[light, dark]))
        .is_some_and(|name| &*name == dark)
}

/// Settings for every browser: the shell view and each tab.
pub fn browser_settings() -> BrowserSettings {
    BrowserSettings {
        // A page may close itself only if script opened it, or if it has no
        // back/forward history, as the HTML standard allows. CEF's default
        // lets any page close itself. A tab that closes itself closes only
        // that tab (`tabs::closing`).
        javascript_close_windows: State::DISABLED,
        ..Default::default()
    }
}

// The main window: the shell view (the browser's own UI) at a fixed width on
// the left, and the tabs' views filling the rest (`tabs.rs`). No title bar: the
// window's buttons sit in the sidebar's top row, and the sidebar's strip moves
// the window (`HandlerDragHandler`).
wrap_window_delegate! {
    struct MainWindowDelegate {
        shell_view: RefCell<Option<BrowserView>>,
        client: Option<Client>,
        first_url: String,
    }

    impl ViewDelegate {
        fn preferred_size(&self, _view: Option<&mut View>) -> Size {
            Size {
                width: WINDOW_WIDTH,
                height: WINDOW_HEIGHT,
            }
        }

        fn minimum_size(&self, _view: Option<&mut View>) -> Size {
            Size {
                width: MIN_WINDOW_WIDTH,
                height: MIN_WINDOW_HEIGHT,
            }
        }

        fn on_theme_changed(&self, view: Option<&mut View>) {
            // CEF clears the background on every theme change. It shows where
            // no view covers the window: beside the sidebar when no tab is open.
            if let Some(view) = view {
                view.set_background_color(chrome_color());
            }
        }
    }

    impl PanelDelegate {}

    impl WindowDelegate {
        fn on_window_created(&self, window: Option<&mut Window>) {
            let shell_view = self.shell_view.borrow().clone();
            let (Some(window), Some(shell_view)) = (window, shell_view) else {
                return;
            };

            // Side by side, full height. Only the tabs take up slack.
            let settings = BoxLayoutSettings {
                horizontal: 1,
                cross_axis_alignment: AxisAlignment::STRETCH,
                ..Default::default()
            };
            let layout = window
                .set_to_box_layout(Some(&settings))
                .expect("BoxLayout is None");
            // Set once here as well: CEF does not call `on_theme_changed` for a
            // window that is still being created.
            window.set_background_color(chrome_color());
            window.add_child_view(Some(&mut View::from(&shell_view)));
            tabs::init(window, &layout, &shell_view, self.client.clone());
            // The strip above the page card moves the window before the shell page loads.
            tabs::set_shell_drag_regions(&[]);
            if tabs::open(&self.first_url).is_err() {
                log_line!("Ass: the first tab could not be opened");
            }
            // Test-only: shown once launch has finished, by the app delegate.
            if test_window::hold_show() {
                return;
            }
            test_window::prepare(window);
            window.show();
            test_window::shown(window);
        }

        fn initial_bounds(&self, _window: Option<&mut Window>) -> Rect {
            // Centred in the main display's work area, and no larger. Without bounds CEF puts
            // the window at the screen's corner (0,0). Test window mode keeps that default.
            if test_window::enabled() {
                return Rect::default();
            }
            let Some(area) = display_get_primary().map(|display| display.work_area()) else {
                return Rect::default();
            };
            let width = WINDOW_WIDTH.min(area.width);
            let height = WINDOW_HEIGHT.min(area.height);
            Rect {
                x: area.x + (area.width - width) / 2,
                y: area.y + (area.height - height) / 2,
                width,
                height,
            }
        }

        fn is_frameless(&self, _window: Option<&mut Window>) -> i32 {
            1
        }

        fn with_standard_window_buttons(&self, _window: Option<&mut Window>) -> i32 {
            1
        }

        fn titlebar_height(&self, _window: Option<&mut Window>, height: Option<&mut f32>) -> i32 {
            match height {
                Some(height) => {
                    *height = TITLEBAR_HEIGHT;
                    1
                }
                None => 0,
            }
        }

        // The bindings answer 0 for each of these unless they are given, which
        // makes a window that cannot be resized, made full screen or minimised.
        fn can_resize(&self, _window: Option<&mut Window>) -> i32 {
            1
        }

        fn can_maximize(&self, _window: Option<&mut Window>) -> i32 {
            1
        }

        fn can_minimize(&self, _window: Option<&mut Window>) -> i32 {
            1
        }

        fn on_window_destroyed(&self, _window: Option<&mut Window>) {
            *self.shell_view.borrow_mut() = None;
            tabs::window_destroyed();
        }

        fn can_close(&self, _window: Option<&mut Window>) -> i32 {
            // The tabs close first, one at a time, each through its page's
            // unload handlers (`tabs::window_may_close`). The quit closes the
            // window again once the last tab is gone.
            if !tabs::window_may_close() {
                return 0;
            }
            // Then the shell, with the window.
            let shell_view = self.shell_view.borrow();
            match shell_view.as_ref().and_then(|view| view.browser()) {
                Some(browser) => browser.host().map_or(1, |host| host.try_close_browser()),
                None => 1,
            }
        }

        fn window_runtime_style(&self) -> RuntimeStyle {
            RuntimeStyle::ALLOY
        }
    }
}

// The shell view keeps a fixed width.
wrap_browser_view_delegate! {
    struct ShellViewDelegate {}

    impl ViewDelegate {
        fn preferred_size(&self, _view: Option<&mut View>) -> Size {
            // CEF ignores a preferred size with a zero dimension, and the view
            // then takes the whole window. The height here only has to be
            // positive: the layout stretches the view to the window's height.
            Size {
                width: SHELL_WIDTH,
                height: 1,
            }
        }
    }

    impl BrowserViewDelegate {
        fn on_browser_created(
            &self,
            _browser_view: Option<&mut BrowserView>,
            browser: Option<&mut Browser>,
        ) {
            // This is where a browser becomes the shell: the one created for
            // this view, before it loads anything.
            if let Some(browser) = browser {
                shell::set_shell_browser(browser);
            }
        }

        fn browser_runtime_style(&self) -> RuntimeStyle {
            RuntimeStyle::ALLOY
        }
    }
}

wrap_app! {
    pub struct AssApp;

    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            // No HTTP auth dialog of the engine in a tab: it is tab-modal and cannot be torn
            // down safely (known issue, `docs/ARCHITECTURE.md`). CEF then asks the client,
            // whose default answer cancels the challenge and the page gets its 401 response.
            if let Some(command_line) = command_line {
                command_line.append_switch(Some(&CefString::from("disable-chrome-login-prompt")));
                // WebAuthn off (known issue 15): its sheet is tab-modal on the same host and no
                // hook refuses it. This hides `PublicKeyCredential`, which sites check before
                // offering a passkey; a page that calls `navigator.credentials` with `publicKey`
                // still reaches the sheet. Added to any value already given, so none is dropped.
                let name = CefString::from("disable-blink-features");
                let given = CefString::from(&command_line.switch_value(Some(&name))).to_string();
                let value = if given.is_empty() {
                    "WebAuth".to_owned()
                } else {
                    format!("{given},WebAuth")
                };
                command_line
                    .append_switch_with_value(Some(&name), Some(&CefString::from(value.as_str())));
            }
        }

        fn on_register_custom_schemes(&self, registrar: Option<&mut SchemeRegistrar>) {
            // Must match the helper processes: see `ass::scheme`.
            scheme::register(registrar);
        }

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(AssBrowserProcessHandler::new(RefCell::new(None)))
        }
    }
}

/// Pages may not open the File System Access pickers or ask for Bluetooth, USB, HID or
/// serial devices: the pickers lead to tab-modal dialogs on the host that cannot be torn down
/// safely (known issue 15, `docs/ARCHITECTURE.md`). The default is saved in the profile, so
/// the build that brings any of these back must set its default again.
fn block_file_and_device_access() {
    const BLOCKED: [ContentSettingTypes; 6] = [
        ContentSettingTypes::FILE_SYSTEM_READ_GUARD,
        ContentSettingTypes::FILE_SYSTEM_WRITE_GUARD,
        ContentSettingTypes::BLUETOOTH_GUARD,
        ContentSettingTypes::USB_GUARD,
        ContentSettingTypes::HID_GUARD,
        ContentSettingTypes::SERIAL_GUARD,
    ];
    // Read back at once: on the UI thread, with the context initialized, each set has run.
    let blocked = request_context_get_global_context().is_some_and(|context| {
        for kind in BLOCKED {
            context.set_content_setting(None, None, kind, ContentSettingValues::BLOCK);
        }
        BLOCKED
            .into_iter()
            .all(|kind| context.content_setting(None, None, kind) == ContentSettingValues::BLOCK)
    });
    if !blocked {
        log_line!("Ass: file and device access could not be blocked");
    }
}

wrap_browser_process_handler! {
    struct AssBrowserProcessHandler {
        client: RefCell<Option<Client>>,
    }

    impl BrowserProcessHandler {
        fn on_context_initialized(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            block_file_and_device_access();

            {
                let mut client = self.client.borrow_mut();
                *client = Some(HandlerClient::new(Handler::new()));
            }
            shell::register_scheme_handler();

            // A "--url=" value on the command line replaces the home page.
            let command_line = command_line_get_global().expect("Failed to get command line");
            let url = CefString::from(&command_line.switch_value(Some(&CefString::from("url"))))
                .to_string();
            let url = if url.is_empty() { HOME_URL.to_owned() } else { url };

            let settings = browser_settings();
            let mut client = self.default_client();
            let mut delegate = ShellViewDelegate::new();
            let shell_view = browser_view_create(
                client.as_mut(),
                Some(&CefString::from(SHELL_URL)),
                Some(&settings),
                None,
                None,
                Some(&mut delegate),
            );

            // Create the Window. It adds the first tab and shows itself.
            let mut delegate =
                MainWindowDelegate::new(RefCell::new(shell_view), self.default_client(), url);
            window_create_top_level(Some(&mut delegate));
        }

        fn on_already_running_app_relaunch(
            &self,
            _command_line: Option<&mut CommandLine>,
            _current_directory: Option<&CefString>,
        ) -> i32 {
            // A second launch on the same profile: bring the existing window forward
            // instead of letting CEF open a default Chrome window.
            if let Some(handler) = Handler::instance() {
                let mut handler = handler.lock().expect("Failed to lock Handler");
                handler.show_main_window();
            }
            1
        }

        fn default_client(&self) -> Option<Client> {
            self.client.borrow().clone()
        }
    }
}
