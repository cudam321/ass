//! The tabs of the main window: one Alloy-style browser view per tab (D2).
//!
//! - Every tab's view is a child of the window, after the shell view, and only
//!   the active one is visible. A view gets its browser only once it is in a
//!   window, and a dialog from a view outside one has no parent.
//! - The model lives here and is used on the UI thread only. The shell page
//!   shows it (`state`) and asks for changes (the commands in `shell.rs`).
//! - Closing a tab: CEF calls `do_close` once the page agreed to close. We
//!   answer that we close it ourselves ([`closing`]) and post [`DetachTab`],
//!   which takes the view out of the window and drops the last reference to
//!   it; CEF then destroys the browser (`on_before_close`). Answering 0 instead
//!   would close the whole window.
//! - Quit: the tabs are asked to close one at a time, the active tab first,
//!   then the rest in list order. When none is left the window closes, and the
//!   shell with it. "Stay" on a leave-site prompt ends the quit. Tabs asked
//!   before that one are already gone: CEF unloads a page as soon as it agrees
//!   to close, so asking every tab before closing any is not possible.
//!
//! The tabs lock is never held across a call into CEF that can call back
//! (closing a browser calls `do_close` before it returns; adding a view creates
//! its browser), and never together with the `Handler` or shell locks.

use cef::*;
use serde::Serialize;
use std::sync::{Mutex, MutexGuard};

use crate::shell;

/// The most tabs open at once. Past it, `open` is refused and popups are
/// cancelled.
const MAX_TABS: usize = 500;

/// The longest title shown, in the shell page and in the window title, in
/// characters.
const MAX_TITLE_CHARS: usize = 256;

/// The longest address shown in the shell page, in bytes. A page can make its
/// own address megabytes long with a fragment.
const MAX_ADDRESS_BYTES: usize = 2 * 1024;

/// Marks text that was cut.
const ELLIPSIS: char = '…';

struct Tab {
    /// Our own identifier: starts at 1 and is never reused in a run. It is
    /// not CEF's browser identifier.
    id: u32,
    view: BrowserView,
    /// Set once CEF has created the tab's browser.
    browser: Option<Browser>,
    browser_id: Option<i32>,
    /// Web pages control the title, the address and the host. They are cut
    /// to a fixed length and only ever travel to the shell page as JSON.
    title: String,
    address: String,
    /// The address's host, shown while the page has no title.
    host: String,
    loading: bool,
    can_go_back: bool,
    can_go_forward: bool,
    /// The tab that opened this one, if any. It becomes the active tab when
    /// this one closes.
    opener: Option<u32>,
    /// The user asked to close it (the `close` command, Cmd+W), so the tab
    /// shown next gets the keyboard. Cleared by "Stay".
    close_asked: bool,
}

impl Tab {
    fn new(id: u32, view: BrowserView, browser: Option<Browser>, opener: Option<u32>) -> Self {
        let browser_id = browser.as_ref().map(|browser| browser.identifier());
        Self {
            id,
            view,
            browser,
            browser_id,
            title: String::new(),
            address: String::new(),
            host: String::new(),
            loading: false,
            can_go_back: false,
            can_go_forward: false,
            opener,
            close_asked: false,
        }
    }

    fn shown_title(&self) -> &str {
        shown_title(&self.title, &self.address, &self.host)
    }
}

/// The title, or the host while the page has none. The engine reports a page
/// without a title by its address, so a title that is only the address counts
/// as none. Decided each time it is shown: the title can arrive before the
/// address it belongs to. With no host (`about:blank`), the title stays.
fn shown_title<'a>(title: &'a str, address: &str, host: &'a str) -> &'a str {
    if title.is_empty() || (!host.is_empty() && is_only_address(title, address)) {
        host
    } else {
        title
    }
}

/// True when `title` is `address` with or without its scheme, and with or
/// without a trailing slash. A title cut by [`display_title`] matches the
/// start of the address.
fn is_only_address(title: &str, address: &str) -> bool {
    let cut = title.chars().count() == MAX_TITLE_CHARS;
    let without_scheme = address.split_once("://").map_or(address, |(_, rest)| rest);
    [address, without_scheme]
        .into_iter()
        .any(|address| match title.strip_suffix(ELLIPSIS) {
            Some(start) if cut => address.starts_with(start),
            _ => title == address || address.strip_suffix('/') == Some(title),
        })
}

/// A popup the engine is about to create: allowed in `on_before_popup`, not
/// yet a tab.
struct PendingPopup {
    /// The browser identifier of the tab that opens it.
    opener: i32,
    popup_id: i32,
    background: bool,
}

struct Tabs {
    next_id: u32,
    list: Vec<Tab>,
    active: Option<u32>,
    window: Option<Window>,
    layout: Option<BoxLayout>,
    shell_view: Option<BrowserView>,
    client: Option<Client>,
    pending_popups: Vec<PendingPopup>,
    /// From a quit request until the app exits or the user cancels.
    quitting: bool,
    /// While quitting: the browser of the tab that was asked to close.
    asking: Option<i32>,
}

impl Tabs {
    fn get(&self, id: u32) -> Option<&Tab> {
        self.list.iter().find(|tab| tab.id == id)
    }

    fn by_browser(&self, browser_id: i32) -> Option<&Tab> {
        self.list
            .iter()
            .find(|tab| tab.browser_id == Some(browser_id))
    }

    fn by_browser_mut(&mut self, browser_id: i32) -> Option<&mut Tab> {
        self.list
            .iter_mut()
            .find(|tab| tab.browser_id == Some(browser_id))
    }

    fn active_tab(&self) -> Option<&Tab> {
        self.active.and_then(|id| self.get(id))
    }

    /// A fresh identifier, or `None` once they are used up.
    fn take_id(&mut self) -> Option<u32> {
        let id = self.next_id;
        self.next_id = id.checked_add(1)?;
        Some(id)
    }
}

static TABS: Mutex<Tabs> = Mutex::new(Tabs {
    next_id: 1,
    list: Vec::new(),
    active: None,
    window: None,
    layout: None,
    shell_view: None,
    client: None,
    pending_popups: Vec::new(),
    quitting: false,
    asking: None,
});

fn tabs() -> MutexGuard<'static, Tabs> {
    TABS.lock().expect("Failed to lock tabs")
}

/// Who made a tab the active one. The user's own action (a command, a menu
/// key) gives the tab the keyboard. One a page caused (a popup, a link opened
/// with Cmd held, a tab closing itself, a leave-site prompt, a tab that lost
/// its browser) does so only if
/// the shell view does not hold the keyboard: the user may be typing there,
/// and their keys must not go to a page they did not choose.
#[derive(Clone, Copy, PartialEq)]
enum Cause {
    User,
    Page,
}

/// Whether a tab about to be shown gets the keyboard. Asked before any view
/// is shown, hidden or removed, since hiding the view that has the keyboard
/// moves it elsewhere.
fn takes_keyboard(cause: Cause, shell_view: Option<&BrowserView>) -> bool {
    cause == Cause::User || shell_view.is_none_or(|shell| shell.has_focus() == 0)
}

/// Why a command was refused. A refused command changes nothing.
pub enum Refused {
    NoSuchTab,
    NoTab,
    TooManyTabs,
    Quitting,
    /// CEF did not create the view.
    Failed,
}

/// What the shell page shows: the reply to `state`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    tabs: Vec<TabState>,
    active_id: Option<u32>,
    address: String,
    can_go_back: bool,
    can_go_forward: bool,
}

#[derive(Serialize)]
struct TabState {
    id: u32,
    title: String,
    loading: bool,
}

pub fn state() -> State {
    let tabs = tabs();
    let active = tabs.active_tab();
    State {
        tabs: tabs
            .list
            .iter()
            .map(|tab| TabState {
                id: tab.id,
                title: tab.shown_title().to_owned(),
                loading: tab.loading,
            })
            .collect(),
        active_id: active.map(|tab| tab.id),
        address: active.map(|tab| tab.address.clone()).unwrap_or_default(),
        can_go_back: active.is_some_and(|tab| tab.can_go_back),
        can_go_forward: active.is_some_and(|tab| tab.can_go_forward),
    }
}

/// Called once, when the main window has been created with the shell view in
/// it.
pub fn init(window: &Window, layout: &BoxLayout, shell_view: &BrowserView, client: Option<Client>) {
    let mut tabs = tabs();
    tabs.window = Some(window.clone());
    tabs.layout = Some(layout.clone());
    tabs.shell_view = Some(shell_view.clone());
    tabs.client = client;
}

/// The most draggable regions taken from the shell page. A page that sends more gets none.
const MAX_SHELL_REGIONS: usize = 64;

/// Makes the shell page's draggable regions the window's, with the strip above the page card.
/// They arrive in the shell view's coordinates; each is cut to the shell view, so that not even
/// the shell page can make the page card a handle, and moved to the window's coordinates.
pub fn set_shell_drag_regions(regions: &[DraggableRegion]) {
    let (window, shell_view) = {
        let tabs = tabs();
        (tabs.window.clone(), tabs.shell_view.clone())
    };
    let (Some(window), Some(shell_view)) = (window, shell_view) else {
        return;
    };
    let regions = if regions.len() > MAX_SHELL_REGIONS {
        &[]
    } else {
        regions
    };
    let size = shell_view.bounds();
    let mut regions: Vec<DraggableRegion> = regions
        .iter()
        .filter_map(|region| {
            let r = &region.bounds;
            let (left, top) = (r.x.max(0), r.y.max(0));
            let right = r.x.saturating_add(r.width).min(size.width);
            let bottom = r.y.saturating_add(r.height).min(size.height);
            if right <= left || bottom <= top {
                return None;
            }
            let mut origin = Point { x: left, y: top };
            if shell_view.convert_point_to_window(Some(&mut origin)) == 0 {
                return None;
            }
            Some(DraggableRegion {
                bounds: Rect {
                    x: origin.x,
                    y: origin.y,
                    width: right - left,
                    height: bottom - top,
                },
                draggable: region.draggable,
            })
        })
        .collect();
    // The strip above the page card, right of the sidebar, is always a handle. It is wider than
    // any window, so a window that grows needs no new regions.
    regions.push(DraggableRegion {
        bounds: Rect {
            x: crate::app::SHELL_WIDTH,
            y: 0,
            width: 1 << 20,
            height: crate::app::PAGE_MARGIN.top,
        },
        draggable: 1,
    });
    // CEF keeps the rectangles of every earlier set until it is given an empty one.
    window.set_draggable_regions(None);
    window.set_draggable_regions(Some(&regions));
}

/// The main window is gone: release every view it held.
pub fn window_destroyed() {
    let released = {
        let mut tabs = tabs();
        tabs.active = None;
        (
            std::mem::take(&mut tabs.list),
            tabs.window.take(),
            tabs.layout.take(),
            tabs.shell_view.take(),
            tabs.client.take(),
        )
    };
    // The references go back to CEF here, after the lock is released.
    drop(released);
}

pub fn is_quitting() -> bool {
    tabs().quitting
}

/// Opens `url` in a new tab in front, for the user: the `open` command, the
/// first tab.
pub fn open(url: &str) -> Result<(), Refused> {
    open_tab(url, false, None, Cause::User)
}

/// Opens `url` in a new tab, in front or behind the active one.
fn open_tab(url: &str, background: bool, opener: Option<u32>, cause: Cause) -> Result<(), Refused> {
    let (id, mut client, window, layout) = {
        let mut tabs = tabs();
        if tabs.quitting {
            return Err(Refused::Quitting);
        }
        if tabs.list.len() + tabs.pending_popups.len() >= MAX_TABS {
            return Err(Refused::TooManyTabs);
        }
        let (Some(window), Some(layout)) = (tabs.window.clone(), tabs.layout.clone()) else {
            return Err(Refused::Quitting);
        };
        let id = tabs.take_id().ok_or(Refused::TooManyTabs)?;
        (id, tabs.client.clone(), window, layout)
    };

    let settings = crate::app::browser_settings();
    let mut delegate = TabViewDelegate::new();
    let view = browser_view_create(
        client.as_mut(),
        Some(&CefString::from(url)),
        Some(&settings),
        None,
        None,
        Some(&mut delegate),
    )
    .ok_or(Refused::Failed)?;
    // The browser is created while the view is added to the window, and
    // reported to `browser_created`.
    add(
        Tab::new(id, view, None, opener),
        background,
        cause,
        &window,
        &layout,
    );
    Ok(())
}

/// Loads `url` in the active tab, which gets the keyboard. With no tab, opens
/// one.
pub fn navigate(url: &str) -> Result<(), Refused> {
    let active = {
        let tabs = tabs();
        if tabs.quitting {
            return Err(Refused::Quitting);
        }
        match tabs.active_tab() {
            Some(tab) => Some((tab.browser.clone().ok_or(Refused::NoTab)?, tab.view.clone())),
            None => None,
        }
    };
    match active {
        Some((browser, view)) => {
            let frame = browser.main_frame().ok_or(Refused::NoTab)?;
            frame.load_url(Some(&CefString::from(url)));
            view.request_focus();
            Ok(())
        }
        None => open(url),
    }
}

/// Makes a tab the active one for the user (the `activate` command, the menu
/// keys): shows its view, hides the one before, and gives it the keyboard.
pub fn activate(id: u32) -> Result<(), Refused> {
    activate_by(id, Cause::User)
}

fn activate_by(id: u32, cause: Cause) -> Result<(), Refused> {
    let (new, old, window, shell_view) = {
        let mut tabs = tabs();
        let new = tabs.get(id).ok_or(Refused::NoSuchTab)?.view.clone();
        let old = tabs
            .active
            .filter(|active| *active != id)
            .and_then(|active| tabs.get(active))
            .map(|tab| tab.view.clone());
        tabs.active = Some(id);
        (new, old, tabs.window.clone(), tabs.shell_view.clone())
    };
    let keyboard = takes_keyboard(cause, shell_view.as_ref());
    show(
        Some(&new),
        old.as_ref(),
        window.as_ref(),
        shell_view.as_ref(),
        keyboard,
    );
    shell::schedule_notify();
    Ok(())
}

/// Asks a tab to close, for the user. Its page can show a leave-site prompt
/// and cancel.
pub fn close(id: u32) -> Result<(), Refused> {
    let browser = {
        let mut tabs = tabs();
        let tab = tabs
            .list
            .iter_mut()
            .find(|tab| tab.id == id)
            .ok_or(Refused::NoSuchTab)?;
        tab.close_asked = true;
        tab.browser.clone()
    };
    match browser.and_then(|browser| browser.host()) {
        // Calls `do_close` before it returns, unless the page has unload
        // handlers to run first.
        Some(host) => host.close_browser(0),
        // No browser to ask.
        None => detach(id),
    }
    Ok(())
}

/// Close Tab in the menu: the active tab, or nothing with no tab.
pub fn close_active() {
    let active = tabs().active;
    if let Some(id) = active {
        let _ = close(id);
    }
}

/// Ctrl+Tab and Ctrl+Shift+Tab: the next or the previous tab, wrapping
/// around.
pub fn activate_next(forward: bool) {
    let id = {
        let tabs = tabs();
        let count = tabs.list.len();
        let current = tabs
            .active
            .and_then(|active| tabs.list.iter().position(|tab| tab.id == active));
        let index = match current {
            Some(index) if forward => (index + 1) % count,
            Some(index) => (index + count - 1) % count,
            None => 0,
        };
        tabs.list.get(index).map(|tab| tab.id)
    };
    if let Some(id) = id {
        let _ = activate(id);
    }
}

/// Cmd+1 to Cmd+8: the tab at that place in the list. Cmd+9: the last tab.
pub fn activate_number(number: isize) {
    let id = {
        let tabs = tabs();
        let tab = match number {
            9 => tabs.list.last(),
            1..=8 => tabs.list.get(number.unsigned_abs() - 1),
            _ => None,
        };
        tab.map(|tab| tab.id)
    };
    if let Some(id) = id {
        let _ = activate(id);
    }
}

#[derive(Clone, Copy)]
pub enum Go {
    Back,
    Forward,
    Reload,
    Stop,
}

/// Back, forward, reload or stop, on the active tab.
pub fn go(action: Go) -> Result<(), Refused> {
    let browser = tabs()
        .active_tab()
        .and_then(|tab| tab.browser.clone())
        .ok_or(Refused::NoTab)?;
    match action {
        Go::Back => browser.go_back(),
        Go::Forward => browser.go_forward(),
        // Also brings back a tab whose renderer ended.
        Go::Reload => browser.reload(),
        Go::Stop => browser.stop_load(),
    }
    Ok(())
}

/// Adds a tab's view to the window, hidden, then shows it unless it opens
/// behind the active tab.
fn add(tab: Tab, background: bool, cause: Cause, window: &Window, layout: &BoxLayout) {
    let id = tab.id;
    let mut view = View::from(&tab.view);
    view.set_visible(0);
    // The page is a card inside the view; the view paints the margin around it.
    view.set_insets(Some(&crate::app::PAGE_MARGIN));
    tabs().list.push(tab);
    window.add_child_view(Some(&mut view));
    layout.set_flex_for_view(Some(&mut view), 1);
    if background {
        shell::schedule_notify();
    } else {
        let _ = activate_by(id, cause);
    }
}

/// Shows `new` and hides `old`. `new` gets the keyboard if `keyboard` says so
/// ([`takes_keyboard`]); with no tab left, the shell gets it.
fn show(
    new: Option<&BrowserView>,
    old: Option<&BrowserView>,
    window: Option<&Window>,
    shell_view: Option<&BrowserView>,
    keyboard: bool,
) {
    if let Some(new) = new {
        new.set_visible(1);
    }
    if let Some(old) = old {
        old.set_visible(0);
    }
    if let Some(window) = window {
        window.layout();
        if let Some(new) = new {
            crate::app::round_page_card(window, new);
        }
    }
    match new {
        Some(new) if keyboard => new.request_focus(),
        Some(_) => {}
        None => {
            if let Some(shell_view) = shell_view {
                shell_view.request_focus();
            }
        }
    }
}

/// Takes a tab out of the model and its view out of the window. Dropping the
/// last reference to the view makes CEF destroy the browser.
fn detach(id: u32) {
    let (tab, window, next, shell_view, was_active, ask_next) = {
        let mut tabs = tabs();
        let Some(index) = tabs.list.iter().position(|tab| tab.id == id) else {
            return;
        };
        let tab = tabs.list.remove(index);
        let was_active = tabs.active == Some(id);
        if was_active {
            // The opener if it is still open, else the next tab, else the
            // one before.
            tabs.active = tab
                .opener
                .filter(|opener| tabs.get(*opener).is_some())
                .or_else(|| tabs.list.get(index).map(|tab| tab.id))
                .or_else(|| {
                    let before = index.checked_sub(1)?;
                    tabs.list.get(before).map(|tab| tab.id)
                });
        }
        let next = if was_active {
            tabs.active_tab().map(|tab| tab.view.clone())
        } else {
            None
        };
        let ask_next = tabs.quitting && tab.browser_id.is_some() && tabs.asking == tab.browser_id;
        (
            tab,
            tabs.window.clone(),
            next,
            tabs.shell_view.clone(),
            was_active,
            ask_next,
        )
    };

    // A close the user asked for, or one the page caused (it closed itself,
    // or its browser went away).
    let cause = if tab.close_asked {
        Cause::User
    } else {
        Cause::Page
    };
    let keyboard = takes_keyboard(cause, shell_view.as_ref());
    if let Some(window) = &window {
        window.remove_child_view(Some(&mut View::from(&tab.view)));
    }
    if was_active {
        show(
            next.as_ref(),
            None,
            window.as_ref(),
            shell_view.as_ref(),
            keyboard,
        );
    } else if let Some(window) = &window {
        window.layout();
    }
    drop(tab);
    shell::schedule_notify();
    if ask_next {
        ask_next_tab(false);
    }
}

// `do_close` is answered while CEF is still inside the close; the view is
// taken out of the window afterwards, from this task.
wrap_task! {
    struct DetachTab {
        id: u32,
    }

    impl Task {
        fn execute(&self) {
            detach(self.id);
        }
    }
}

fn post_detach(id: u32) {
    let mut task = DetachTab::new(id);
    // Fails only while CEF shuts down; the window then takes the view with it.
    post_task(ThreadId::UI, Some(&mut task));
}

/// CEF's `do_close` for every browser but the shell's: a page agreed to
/// close. False lets CEF close the window that holds the browser, so it is
/// answered only for a browser with no view or in a window other than the
/// main one (DevTools, or a window of Chrome's own). A tab is taken out of the
/// window. Anything else answers true, so a close can never reach the main
/// window, which would quit the app.
pub fn closing(browser: &Browser) -> bool {
    let browser_id = browser.identifier();
    let (id, main) = {
        let tabs = tabs();
        let id = tabs.by_browser(browser_id).map(|tab| tab.id);
        (id, tabs.window.clone())
    };
    if let Some(id) = id {
        post_detach(id);
        return true;
    }
    // Read: a popup is listed before it can agree to close (`alloy_browser_host_impl.cc:203-222`).
    // The run behind it: `w = window.open(u); w.close()`, 20 times.
    let Some(view) = browser_view_get_for_browser(Some(&mut browser.clone())) else {
        return false;
    };
    let in_main = |window: &Window| {
        main.as_ref()
            .is_some_and(|main| View::from(window).is_same(Some(&mut View::from(main))) != 0)
    };
    view.window().is_none_or(|window| in_main(&window))
}

// A popup that does not become a tab: its view is dropped with this task,
// which destroys its browser. Never inside CEF's callback: a popup's view
// dropped while CEF is still creating it is used again after it is gone
// (`alloy_browser_host_impl.cc:203-222`).
wrap_task! {
    struct ReleaseView {
        view: BrowserView,
    }

    impl Task {
        fn execute(&self) {}
    }
}

fn post_release(view: BrowserView) {
    let mut task = ReleaseView::new(view);
    post_task(ThreadId::UI, Some(&mut task));
}

/// CEF's `on_before_close`. A tab still listed here lost its browser some
/// other way than through [`closing`]; its view goes too.
pub fn browser_closed(browser: &Browser) {
    let browser_id = browser.identifier();
    let (id, released) = {
        let mut tabs = tabs();
        // Popups it was allowed to open and never did stop counting against
        // the cap.
        tabs.pending_popups
            .retain(|popup| popup.opener != browser_id);
        // No reference to the browser is kept past this call (CEF's header).
        // The identifier stays, to find the tab.
        match tabs.by_browser_mut(browser_id) {
            Some(tab) => (Some(tab.id), tab.browser.take()),
            None => (None, None),
        }
    };
    drop(released);
    if let Some(id) = id {
        post_detach(id);
    }
}

/// A tab's view has its browser (`on_browser_created`).
pub fn browser_created(view: &BrowserView, browser: &Browser) {
    let browser_id = browser.identifier();
    let mut view = View::from(view);
    let mut tabs = tabs();
    // Comparing views calls into CEF, which calls nothing back.
    if let Some(tab) = tabs
        .list
        .iter_mut()
        .find(|tab| tab.browser.is_none() && tab.view.is_same(Some(&mut view)) != 0)
    {
        tab.browser = Some(browser.clone());
        tab.browser_id = Some(browser_id);
    }
}

/// Where a new tab goes for a window-open disposition: `Some(true)` behind
/// the active tab, `Some(false)` in front, `None` nowhere (cancel).
pub fn placement(disposition: WindowOpenDisposition) -> Option<bool> {
    match disposition {
        WindowOpenDisposition::NEW_FOREGROUND_TAB
        | WindowOpenDisposition::NEW_WINDOW
        | WindowOpenDisposition::NEW_POPUP
        | WindowOpenDisposition::SINGLETON_TAB => Some(false),
        WindowOpenDisposition::NEW_BACKGROUND_TAB => Some(true),
        _ => None,
    }
}

/// CEF's `on_before_popup` for a page in a tab. True lets the popup be
/// created; it then becomes a tab in [`popup_created`].
pub fn popup_allowed(
    opener: &Browser,
    popup_id: i32,
    disposition: WindowOpenDisposition,
    user_gesture: bool,
) -> bool {
    let Some(background) = placement(disposition) else {
        return false;
    };
    if !user_gesture {
        return false;
    }
    let opener = opener.identifier();
    let mut tabs = tabs();
    if tabs.quitting
        || tabs.list.len() + tabs.pending_popups.len() >= MAX_TABS
        || tabs.by_browser(opener).is_none()
    {
        return false;
    }
    tabs.pending_popups.push(PendingPopup {
        opener,
        popup_id,
        background,
    });
    true
}

/// CEF's `on_before_popup_aborted`: an allowed popup was not created.
pub fn popup_aborted(opener: &Browser, popup_id: i32) {
    let opener = opener.identifier();
    tabs()
        .pending_popups
        .retain(|popup| !(popup.opener == opener && popup.popup_id == popup_id));
}

/// CEF's `on_popup_browser_view_created`: makes the popup a tab. Always true,
/// since false makes CEF open a window of its own: while quitting, or with no
/// window to put it in, the popup is dropped instead.
pub fn popup_created(opener_view: Option<&BrowserView>, popup_view: &BrowserView) -> bool {
    let opener = opener_view
        .and_then(|view| view.browser())
        .map(|browser| browser.identifier());
    let browser = popup_view.browser();
    let placed = {
        let mut tabs = tabs();
        // CEF passes no popup identifier here. Popups of one opener are
        // created in the order they were allowed.
        let pending = opener
            .and_then(|opener| {
                tabs.pending_popups
                    .iter()
                    .position(|popup| popup.opener == opener)
            })
            .map(|index| tabs.pending_popups.remove(index));
        let opener_tab = opener
            .and_then(|opener| tabs.by_browser(opener))
            .map(|tab| tab.id);
        let background = pending.is_some_and(|popup| popup.background);
        match (tabs.quitting, tabs.window.clone(), tabs.layout.clone()) {
            (false, Some(window), Some(layout)) => tabs
                .take_id()
                .map(|id| (id, background, opener_tab, window, layout)),
            _ => None,
        }
    };
    let Some((id, background, opener_tab, window, layout)) = placed else {
        post_release(popup_view.clone());
        return true;
    };
    add(
        Tab::new(id, popup_view.clone(), browser, opener_tab),
        background,
        Cause::Page,
        &window,
        &layout,
    );
    true
}

/// A page asked for a URL to open in a new tab (CEF's `on_open_urlfrom_tab`,
/// a link clicked with Cmd held). Opened from a task, outside CEF's callback.
pub fn open_from_page(source: &Browser, url: String, background: bool) {
    let source = source.identifier();
    let opener = tabs().by_browser(source).map(|tab| tab.id);
    let mut task = OpenTab::new(url, background, opener);
    post_task(ThreadId::UI, Some(&mut task));
}

wrap_task! {
    struct OpenTab {
        url: String,
        background: bool,
        opener: Option<u32>,
    }

    impl Task {
        fn execute(&self) {
            let _ = open_tab(&self.url, self.background, self.opener, Cause::Page);
        }
    }
}

/// A leave-site prompt is about to show. True: let it show. That is for the
/// active tab, and for a close we asked for (the `close` command or Cmd+W
/// for that tab, or the quit asking it), whose tab is brought to the front.
/// False: a background page's own navigation, which goes ahead without a
/// prompt, so a page cannot pull itself to the front. A close the user asked
/// for lets one prompt show; `close` asks again each time.
pub fn unload_dialog(browser: &Browser) -> bool {
    let browser_id = browser.identifier();
    let (id, asked, active) = {
        let mut tabs = tabs();
        let asking = tabs.quitting && tabs.asking == Some(browser_id);
        let active_id = tabs.active;
        let Some(tab) = tabs.by_browser_mut(browser_id) else {
            return true;
        };
        let asked = tab.close_asked || asking;
        let active = active_id == Some(tab.id);
        if asked || active {
            tab.close_asked = false;
        }
        (tab.id, asked, active)
    };
    if asked && !active {
        let mut task = ActivateTab::new(id);
        post_task(ThreadId::UI, Some(&mut task));
    }
    asked || active
}

wrap_task! {
    struct ActivateTab {
        id: u32,
    }

    impl Task {
        fn execute(&self) {
            let _ = activate_by(self.id, Cause::Page);
        }
    }
}

/// A tab's title changed.
pub fn title_changed(browser: &Browser, title: Option<&CefString>) {
    let title = display_title(&title.map(ToString::to_string).unwrap_or_default());
    update(browser, |tab| tab.title = title);
}

/// A tab's main frame is at a new address: a commit, or a failed load. The
/// only place a tab's address and host are set.
pub fn address_changed(browser: &Browser, url: Option<&CefString>) {
    let (address, host) = shown_address(&url.map(ToString::to_string).unwrap_or_default());
    update(browser, |tab| {
        tab.address = address;
        tab.host = host;
    });
}

/// The address and the host shown for a URL a page reached. A user name and
/// password are dropped: a page can put a name the user trusts there
/// (`https://accounts.google.com:x@evil.example/`). Then the address is cut
/// for length. Text that does not parse is shown as it is, cut, with no host.
fn shown_address(url: &str) -> (String, String) {
    match url::Url::parse(url) {
        Ok(mut parsed) => {
            // Both fail only for a URL that cannot carry either.
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            let host = parsed.host_str().map(display_title).unwrap_or_default();
            (display_address(parsed.into()), host)
        }
        Err(_) => (display_address(url.to_owned()), String::new()),
    }
}

/// A tab started or stopped loading.
pub fn loading_changed(browser: &Browser, loading: bool, can_go_back: bool, can_go_forward: bool) {
    update(browser, |tab| {
        tab.loading = loading;
        tab.can_go_back = can_go_back;
        tab.can_go_forward = can_go_forward;
    });
}

/// A tab's renderer process ended. The tab stays; `reload` brings it back.
pub fn renderer_ended(browser: &Browser) {
    update(browser, |tab| tab.loading = false);
}

/// Changes the tab that shows `browser`, if any, and tells the shell.
fn update(browser: &Browser, change: impl FnOnce(&mut Tab)) {
    let browser_id = browser.identifier();
    let changed = match tabs().by_browser_mut(browser_id) {
        Some(tab) => {
            change(tab);
            true
        }
        None => false,
    };
    if changed {
        shell::schedule_notify();
    }
}

/// Names the window after the active tab. Called with each notice to the
/// shell page, so it shares its rate limit: a page can change its title
/// thousands of times a second.
pub fn show_window_title() {
    let (window, title) = {
        let tabs = tabs();
        let title = tabs.active_tab().map(|tab| tab.shown_title().to_owned());
        (tabs.window.clone(), title.unwrap_or_default())
    };
    if let Some(window) = window {
        window.set_title(Some(&CefString::from(title.as_str())));
    }
}

/// Starts a quit: Cmd+Q, the Dock menu, logout, or closing the window. From a
/// task, so it never runs inside a CEF callback.
pub fn quit() {
    let mut task = StartQuit::new();
    post_task(ThreadId::UI, Some(&mut task));
}

wrap_task! {
    struct StartQuit;

    impl Task {
        fn execute(&self) {
            // Every request goes on with the sequence, so a quit that stopped
            // starts again. Asking the tab already asked again is harmless.
            tabs().quitting = true;
            ask_next_tab(true);
        }
    }
}

/// The user answered "Stay" to a leave-site prompt (`cancelTerminate:`). The
/// tabs not yet asked stay open.
pub fn quit_cancelled() {
    let mut tabs = tabs();
    tabs.quitting = false;
    tabs.asking = None;
    // Sent for "Stay" on any leave-site prompt, the user's own close included
    // (see `cancelTerminate:` in `mac.rs`).
    for tab in &mut tabs.list {
        tab.close_asked = false;
    }
}

/// The main window's `can_close`: true once no tab is left. Otherwise starts
/// the quit, which closes the window again when the last tab is gone.
pub fn window_may_close() -> bool {
    let empty = {
        let mut tabs = tabs();
        let empty = tabs.list.is_empty();
        if empty {
            tabs.quitting = true;
        }
        empty
    };
    if !empty {
        quit();
    }
    empty
}

/// While quitting, asks the next tab to close: the tab already asked if it is
/// still open, else the active tab first, then the first in the list. A tab
/// with no browser or host to ask is taken out. With no tab left, closes the
/// window.
fn ask_next_tab(active_first: bool) {
    enum Next {
        Ask(BrowserHost),
        Detach(u32),
        CloseWindow(Window),
        Nothing,
    }
    let next = {
        let mut tabs = tabs();
        if !tabs.quitting {
            return;
        }
        // Asking it again does nothing while its close is pending, and shows
        // its prompt again after "Stay" (`alloy_browser_host_impl.cc:270`,
        // `:957-960`).
        let tab = tabs
            .asking
            .and_then(|browser_id| tabs.by_browser(browser_id))
            .or_else(|| active_first.then(|| tabs.active_tab()).flatten())
            .or_else(|| tabs.list.first());
        // Reading the host calls nothing back.
        let (next, asking) = match tab {
            Some(tab) => match tab.browser.as_ref().and_then(|browser| browser.host()) {
                Some(host) => (Next::Ask(host), tab.browser_id),
                None => (Next::Detach(tab.id), None),
            },
            None => (
                tabs.window.clone().map_or(Next::Nothing, Next::CloseWindow),
                None,
            ),
        };
        tabs.asking = asking;
        next
    };
    match next {
        // Calls `do_close` before it returns when the page has no unload
        // handlers; `detach` then asks the next tab.
        Next::Ask(host) => host.close_browser(0),
        Next::Detach(id) => {
            detach(id);
            ask_next_tab(false);
        }
        // `can_close` agrees now, and the shell closes with the window.
        Next::CloseWindow(window) => window.close(),
        Next::Nothing => {}
    }
}

/// A title as the browser shows it: at most [`MAX_TITLE_CHARS`] characters.
fn display_title(title: &str) -> String {
    match title.char_indices().nth(MAX_TITLE_CHARS - 1) {
        Some((end, _)) if title.char_indices().nth(MAX_TITLE_CHARS).is_some() => {
            let mut title = title[..end].to_owned();
            title.push(ELLIPSIS);
            title
        }
        _ => title.to_owned(),
    }
}

/// An address as the shell page shows it: at most [`MAX_ADDRESS_BYTES`].
fn display_address(mut url: String) -> String {
    if url.len() > MAX_ADDRESS_BYTES {
        let mut end = MAX_ADDRESS_BYTES - ELLIPSIS.len_utf8();
        while !url.is_char_boundary(end) {
            end -= 1;
        }
        url.truncate(end);
        url.push(ELLIPSIS);
    }
    url
}

// Every tab's view, including popups and popups of popups.
wrap_browser_view_delegate! {
    pub struct TabViewDelegate {}

    impl ViewDelegate {
        fn on_theme_changed(&self, view: Option<&mut View>) {
            // The margin around the page card is the view's own background,
            // which CEF clears on every theme change.
            if let Some(view) = view {
                view.set_background_color(crate::app::chrome_color());
            }
        }
    }

    impl BrowserViewDelegate {
        fn on_browser_created(
            &self,
            browser_view: Option<&mut BrowserView>,
            browser: Option<&mut Browser>,
        ) {
            if let (Some(view), Some(browser)) = (browser_view, browser) {
                browser_created(view, browser);
            }
        }

        fn delegate_for_popup_browser_view(
            &self,
            _browser_view: Option<&mut BrowserView>,
            _settings: Option<&BrowserSettings>,
            _client: Option<&mut Client>,
            _is_devtools: i32,
        ) -> Option<BrowserViewDelegate> {
            // Without one, CEF gives the popup's view no delegate, and its
            // own popups would open in windows of CEF's making.
            Some(TabViewDelegate::new())
        }

        fn on_popup_browser_view_created(
            &self,
            browser_view: Option<&mut BrowserView>,
            popup_browser_view: Option<&mut BrowserView>,
            is_devtools: i32,
        ) -> i32 {
            // 1: the popup is ours to place. 0: CEF opens a window for it.
            match popup_browser_view {
                Some(popup) if is_devtools == 0 => {
                    popup_created(browser_view.as_deref(), popup).into()
                }
                _ => 0,
            }
        }

        fn browser_runtime_style(&self) -> RuntimeStyle {
            RuntimeStyle::ALLOY
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_are_cut_to_the_limit() {
        assert_eq!(display_title("Example"), "Example");
        let exact = "a".repeat(MAX_TITLE_CHARS);
        assert_eq!(display_title(&exact), exact);
        let long = "é".repeat(MAX_TITLE_CHARS + 1);
        let shown = display_title(&long);
        assert_eq!(shown.chars().count(), MAX_TITLE_CHARS);
        assert!(shown.ends_with(ELLIPSIS));
    }

    #[test]
    fn addresses_are_cut_on_a_character_boundary() {
        let short = "https://example.com/".to_owned();
        assert_eq!(display_address(short.clone()), short);
        let long = format!("https://example.com/#{}", "é".repeat(MAX_ADDRESS_BYTES));
        let shown = display_address(long);
        assert!(shown.len() <= MAX_ADDRESS_BYTES);
        assert!(shown.ends_with(ELLIPSIS));
    }

    #[test]
    fn shown_addresses_carry_no_user_name_or_password() {
        for (url, address, host) in [
            (
                "https://accounts.google.com:x@evil.example/",
                "https://evil.example/",
                "evil.example",
            ),
            (
                "https://user@example.com/path?q#f",
                "https://example.com/path?q#f",
                "example.com",
            ),
            (
                "http://u:p@127.0.0.1:8080/",
                "http://127.0.0.1:8080/",
                "127.0.0.1",
            ),
            (
                "https://example.com/a@b",
                "https://example.com/a@b",
                "example.com",
            ),
            ("about:blank", "about:blank", ""),
            ("data:text/html,x@y", "data:text/html,x@y", ""),
        ] {
            assert_eq!(
                shown_address(url),
                (address.to_owned(), host.to_owned()),
                "url {url:?}"
            );
        }
    }

    #[test]
    fn shown_addresses_that_do_not_parse_are_kept_and_cut() {
        assert_eq!(
            shown_address("not a url"),
            ("not a url".to_owned(), String::new())
        );
        let long = format!("{}#{}", "x".repeat(10), "é".repeat(MAX_ADDRESS_BYTES));
        let (address, host) = shown_address(&long);
        assert!(address.len() <= MAX_ADDRESS_BYTES && address.ends_with(ELLIPSIS));
        assert_eq!(host, "");
        let long = format!("https://u:p@example.com/#{}", "a".repeat(MAX_ADDRESS_BYTES));
        let (address, host) = shown_address(&long);
        assert!(address.starts_with("https://example.com/#"));
        assert!(address.len() <= MAX_ADDRESS_BYTES && address.ends_with(ELLIPSIS));
        assert_eq!(host, "example.com");
    }

    #[test]
    fn a_title_that_is_only_the_address_counts_as_none() {
        let (address, host) = shown_address("http://u:p@127.0.0.1:9362/notitle/x");
        for title in [
            "127.0.0.1:9362/notitle/x",
            "http://127.0.0.1:9362/notitle/x",
            "",
        ] {
            assert_eq!(
                shown_title(title, &address, &host),
                "127.0.0.1",
                "title {title:?}"
            );
        }
        let (address, host) = shown_address("https://example.com/");
        for title in [
            "https://example.com/",
            "https://example.com",
            "example.com/",
            "example.com",
        ] {
            assert_eq!(
                shown_title(title, &address, &host),
                "example.com",
                "title {title:?}"
            );
        }
        // Real titles stay, also one that only starts like the address.
        for title in ["Example Domain", "example.com - home", "example", "…"] {
            assert_eq!(shown_title(title, &address, &host), title);
        }
        // Before the address has arrived, and an address with no host.
        assert_eq!(shown_title("example.com/x", "", ""), "example.com/x");
        assert_eq!(shown_title("about:blank", "about:blank", ""), "about:blank");
        // A long address, cut as a title.
        let url = format!("https://example.com/{}", "a".repeat(400));
        let (address, host) = shown_address(&url);
        let title = display_title(url.trim_start_matches("https://"));
        assert_eq!(shown_title(&title, &address, &host), "example.com");
    }

    #[test]
    fn dispositions_map_to_placements() {
        assert_eq!(
            placement(WindowOpenDisposition::NEW_FOREGROUND_TAB),
            Some(false)
        );
        assert_eq!(placement(WindowOpenDisposition::NEW_POPUP), Some(false));
        assert_eq!(
            placement(WindowOpenDisposition::NEW_BACKGROUND_TAB),
            Some(true)
        );
        for refused in [
            WindowOpenDisposition::CURRENT_TAB,
            WindowOpenDisposition::SAVE_TO_DISK,
            WindowOpenDisposition::OFF_THE_RECORD,
            WindowOpenDisposition::NEW_PICTURE_IN_PICTURE,
            WindowOpenDisposition::SWITCH_TO_TAB,
            WindowOpenDisposition::IGNORE_ACTION,
        ] {
            assert_eq!(placement(refused), None);
        }
    }
}
