//! Browser-level callbacks: tracks open browsers, quits once the last one has
//! closed, and passes each event to the shell (`shell.rs`) or the tabs
//! (`tabs.rs`).

use cef::*;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::address;
use crate::flush;
use crate::shell;
use crate::tabs;
use crate::test_window;

static HANDLER_INSTANCE: OnceLock<Weak<Mutex<Handler>>> = OnceLock::new();

pub struct Handler {
    browser_list: Vec<Browser>,
    weak_self: Weak<Mutex<Self>>,
}

impl Handler {
    pub fn instance() -> Option<Arc<Mutex<Self>>> {
        HANDLER_INSTANCE.get().and_then(|weak| weak.upgrade())
    }

    pub fn new() -> Arc<Mutex<Self>> {
        Arc::new_cyclic(|weak| {
            if let Err(instance) = HANDLER_INSTANCE.set(weak.clone()) {
                assert_eq!(instance.strong_count(), 0, "Replacing a viable instance");
            }

            Mutex::new(Self {
                browser_list: Vec::new(),
                weak_self: weak.clone(),
            })
        })
    }

    fn on_after_created(&mut self, browser: Option<&mut Browser>) {
        debug_assert_ne!(currently_on(ThreadId::UI), 0);

        // Add to the list of existing browsers.
        let browser = browser.cloned().expect("Browser is None");
        self.browser_list.push(browser);
    }

    fn on_before_close(&mut self, browser: Option<&mut Browser>) {
        debug_assert_ne!(currently_on(ThreadId::UI), 0);

        // Remove from the list of existing browsers.
        let mut browser = browser.cloned().expect("Browser is None");
        if let Some(index) = self
            .browser_list
            .iter()
            .position(move |elem| elem.is_same(Some(&mut browser)) != 0)
        {
            self.browser_list.remove(index);
        }

        if self.browser_list.is_empty() {
            // The shell closes last, with the window (`tabs::window_may_close`).
            // Closing the last tab leaves the shell, so the app keeps running.
            // The message loop ends once the cookie store is flushed (`flush.rs`).
            flush::flush_then_quit();
        }
    }

    pub fn show_main_window(&mut self) {
        let thread_id = ThreadId::UI;
        if currently_on(thread_id) == 0 {
            // Execute on the UI thread.
            let this = self
                .weak_self
                .upgrade()
                .expect("Weak reference to Handler is None");
            let mut task = ShowMainWindow::new(this);
            post_task(thread_id, Some(&mut task));
            return;
        }

        // Test-only: until launch has finished, the app delegate shows the
        // window (`test_window::hold_show`), also after a second launch.
        if test_window::hold_show() {
            return;
        }

        let Some(mut main_browser) = self.browser_list.first().cloned() else {
            return;
        };

        if let Some(browser_view) = browser_view_get_for_browser(Some(&mut main_browser))
            && let Some(window) = browser_view.window()
        {
            test_window::prepare(&window);
            window.show();
            test_window::shown(&window);
        }
    }
}

wrap_client! {
    pub struct HandlerClient {
        inner: Arc<Mutex<Handler>>,
    }

    impl Client {
        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(HandlerDisplayHandler::new())
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(HandlerLifeSpanHandler::new(self.inner.clone()))
        }

        fn request_handler(&self) -> Option<RequestHandler> {
            Some(HandlerRequestHandler::new())
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(HandlerLoadHandler::new())
        }

        fn jsdialog_handler(&self) -> Option<JsdialogHandler> {
            Some(HandlerJsdialogHandler::new())
        }

        fn drag_handler(&self) -> Option<DragHandler> {
            Some(HandlerDragHandler::new())
        }
    }
}

// What moves the window: the regions the shell page marks with
// `-webkit-app-region` (its top row), and the strip above the page card, which
// `tabs::set_shell_drag_regions` adds. Every browser reports its page's regions
// here. Only the shell's main frame is listened to, so no web page can make any
// part of the window a handle, or take the handle away.
wrap_drag_handler! {
    struct HandlerDragHandler;

    impl DragHandler {
        fn on_draggable_regions_changed(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            regions: Option<&[DraggableRegion]>,
        ) {
            let (Some(browser), Some(frame)) = (browser, frame) else {
                return;
            };
            if !shell::is_shell(browser) || frame.is_main() == 0 {
                // Test-only, so that tests can count it. Fixed text and a number.
                if test_window::enabled() {
                    let id = browser.identifier();
                    log_line!("Ass: dropped the draggable regions of browser {id}");
                }
                return;
            }
            tabs::set_shell_drag_regions(regions.unwrap_or_default());
        }
    }
}

// Navigation policy. The rules are in `shell`; they depend on which browser
// is asking.
wrap_request_handler! {
    struct HandlerRequestHandler;

    impl RequestHandler {
        fn on_before_browse(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _user_gesture: i32,
            _is_redirect: i32,
        ) -> i32 {
            // 1 cancels the navigation. Cancel when in doubt.
            let (Some(browser), Some(request)) = (browser, request) else {
                return 1;
            };
            let url = CefString::from(&request.url()).to_string();
            shell::blocks_navigation(browser, &url).into()
        }

        fn on_open_urlfrom_tab(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            target_url: Option<&CefString>,
            target_disposition: WindowOpenDisposition,
            user_gesture: i32,
        ) -> i32 {
            // 1 cancels; 0 loads the URL in this same browser, whatever the
            // disposition says. The shell view never opens a URL anywhere else.
            let Some(browser) = browser else {
                return 1;
            };
            if shell::is_shell(browser) {
                return 1;
            }
            if target_disposition == WindowOpenDisposition::CURRENT_TAB {
                return 0;
            }
            // A new tab (a link clicked with Cmd held): opened by us, as a
            // load the browser starts, so only a web address qualifies.
            if let Some(background) = tabs::placement(target_disposition)
                && user_gesture != 0
                && let Some(url) =
                    address::web_address(&target_url.map(ToString::to_string).unwrap_or_default())
            {
                tabs::open_from_page(browser, url, background);
            }
            1
        }

        fn on_select_client_certificate(
            &self,
            _browser: Option<&mut Browser>,
            _is_proxy: i32,
            _host: Option<&CefString>,
            _port: i32,
            _certificates: Option<&[Option<X509Certificate>]>,
            callback: Option<&mut SelectClientCertificateCallback>,
        ) -> i32 {
            // Always without a certificate, never the engine's picker: it is tab-modal (known
            // issue, `docs/ARCHITECTURE.md`) and would list the keychain's identities to a page.
            if let Some(callback) = callback {
                callback.select(None);
            }
            1
        }

        fn on_render_process_terminated(
            &self,
            browser: Option<&mut Browser>,
            _status: TerminationStatus,
            _error_code: i32,
            _error_string: Option<&CefString>,
        ) {
            if let Some(browser) = browser {
                if shell::is_shell(browser) {
                    shell::render_process_ended();
                } else {
                    tabs::renderer_ended(browser);
                }
            }
        }
    }
}

wrap_display_handler! {
    struct HandlerDisplayHandler;

    impl DisplayHandler {
        fn on_title_change(&self, browser: Option<&mut Browser>, title: Option<&CefString>) {
            // The shell's own title is not shown anywhere.
            if let Some(browser) = browser {
                tabs::title_changed(browser, title);
            }
        }

        fn on_address_change(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            url: Option<&CefString>,
        ) {
            // Called only once a main-frame navigation has committed.
            let (Some(browser), Some(frame)) = (browser, frame) else {
                return;
            };
            if frame.is_main() == 0 {
                return;
            }
            if shell::is_shell(browser) {
                shell::address_changed(url);
            } else {
                tabs::address_changed(browser, url);
            }
        }
    }
}

wrap_load_handler! {
    struct HandlerLoadHandler;

    impl LoadHandler {
        fn on_loading_state_change(
            &self,
            browser: Option<&mut Browser>,
            is_loading: i32,
            can_go_back: i32,
            can_go_forward: i32,
        ) {
            if let Some(browser) = browser {
                tabs::loading_changed(browser, is_loading != 0, can_go_back != 0, can_go_forward != 0);
            }
        }

        fn on_load_error(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _error_code: Errorcode,
            _error_text: Option<&CefString>,
            _failed_url: Option<&CefString>,
        ) {
            // A failed load reports no address change. CEF refreshes the
            // frame's URL first, so it is the page on screen: the failed
            // address when its error page committed, the one before when
            // nothing committed.
            if let (Some(browser), Some(frame)) = (browser, frame)
                && frame.is_main() != 0
            {
                let url = CefString::from(&frame.url());
                tabs::address_changed(browser, Some(&url));
            }
        }
    }
}

wrap_jsdialog_handler! {
    struct HandlerJsdialogHandler;

    impl JsdialogHandler {
        fn on_jsdialog(
            &self,
            _browser: Option<&mut Browser>,
            _origin_url: Option<&CefString>,
            _dialog_type: JsdialogType,
            _message_text: Option<&CefString>,
            _default_prompt_text: Option<&CefString>,
            _callback: Option<&mut JsdialogCallback>,
            suppress_message: Option<&mut i32>,
        ) -> i32 {
            // Every `alert`, `confirm` and `prompt` is answered unseen: the engine's
            // tab-modal dialogs cannot be torn down safely in a tab of ours (known
            // issue 15, `docs/ARCHITECTURE.md`).
            if let Some(suppress_message) = suppress_message {
                *suppress_message = 1;
            }
            // 0, never 1: 1 with the message suppressed fails a `CHECK` in CEF.
            0
        }

        fn on_before_unload_dialog(
            &self,
            browser: Option<&mut Browser>,
            _message_text: Option<&CefString>,
            _is_reload: i32,
            callback: Option<&mut JsdialogCallback>,
        ) -> i32 {
            // 0 shows the engine's own prompt. A prompt `tabs::unload_dialog`
            // refuses is answered "leave" at once, which is what 1 tells CEF.
            let show = browser.as_deref().is_none_or(tabs::unload_dialog);
            match callback {
                Some(callback) if !show => {
                    callback.cont(1, Some(&CefString::from("")));
                    1
                }
                _ => 0,
            }
        }
    }
}

wrap_life_span_handler! {
    struct HandlerLifeSpanHandler {
        inner: Arc<Mutex<Handler>>,
    }

    impl LifeSpanHandler {
        fn on_before_popup(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            popup_id: i32,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            target_disposition: WindowOpenDisposition,
            user_gesture: i32,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut i32>,
        ) -> i32 {
            // 1 cancels the popup. Cancel when in doubt. An allowed popup
            // becomes a tab (`tabs::popup_created`).
            let Some(browser) = browser else {
                return 1;
            };
            let url = target_url.map(ToString::to_string).unwrap_or_default();
            if shell::blocks_popup(browser, &url) {
                return 1;
            }
            let allowed =
                tabs::popup_allowed(browser, popup_id, target_disposition, user_gesture != 0);
            (!allowed).into()
        }

        fn on_before_popup_aborted(&self, browser: Option<&mut Browser>, popup_id: i32) {
            if let Some(browser) = browser {
                tabs::popup_aborted(browser, popup_id);
            }
        }

        fn on_after_created(&self, browser: Option<&mut Browser>) {
            let mut inner = self.inner.lock().expect("Failed to lock inner");
            inner.on_after_created(browser);
        }

        fn do_close(&self, browser: Option<&mut Browser>) -> i32 {
            // 0 closes the window that holds the browser. For the shell that
            // is the main window, which quits the app. Every other browser
            // goes through `tabs::closing`, which answers 0 only for a
            // browser outside the main window.
            match browser {
                Some(browser) if !shell::is_shell(browser) => tabs::closing(browser).into(),
                _ => 0,
            }
        }

        fn on_before_close(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = &browser {
                shell::browser_closed(browser);
                tabs::browser_closed(browser);
            }

            let mut inner = self.inner.lock().expect("Failed to lock inner");
            inner.on_before_close(browser);
        }
    }
}

wrap_task! {
    struct ShowMainWindow {
        inner: Arc<Mutex<Handler>>,
    }

    impl Task {
        fn execute(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);

            let mut inner = self.inner.lock().expect("Failed to lock inner");
            inner.show_main_window();
        }
    }
}
