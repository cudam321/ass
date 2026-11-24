//! The shell: the browser's own UI, an HTML page in its own browser view.
//!
//! - The page and its assets are compiled into the binary and served from
//!   `ass://shell/`. Nothing is read from disk or from the network.
//! - Page to Rust: the page POSTs a JSON object to `ass://shell/api/<command>`.
//!   [`parse_command`] is the complete list of commands. Every command but
//!   `ping` is answered from the UI thread, where the tabs live
//!   ([`run_command`]); the request waits for it and is always answered or
//!   cancelled ([`PendingReply`]).
//! - Rust to page: [`run_in_shell`] runs one of three constant scripts, each
//!   firing an event with no data: the state is stale (at most once every
//!   [`NOTIFY_INTERVAL_MS`]), focus the address input (Cmd+L), or start a new
//!   tab (Cmd+T). The page then fetches the state as JSON. Text that a web page
//!   controls (titles, addresses) is cut to a fixed length (`tabs.rs`), and
//!   only ever travels as JSON data, never as script.
//!
//! What the browser process guarantees, from the browser and frame CEF
//! reports for each request (CEF resolves both in the browser process, so a
//! page cannot forge them):
//!
//! - `ass://shell/` answers only the shell browser's main frame. Requests from
//!   any other browser, from a sub-frame, or from no browser at all get no
//!   asset and no reply.
//! - Commands are answered only while CEF reports that frame on the shell page
//!   (its URL without the fragment is `SHELL_URL`). The page and its two
//!   assets are served whatever the frame shows: they are fixed bytes with no
//!   data in them.
//! - The shell browser can only navigate to `SHELL_URL`, and cannot open
//!   popups. If its main frame ends up on another URL anyway (`about:blank`
//!   commits without asking), or its renderer process ends, the shell page is
//!   loaded again, at most [`MAX_RELOADS`] times in [`RELOAD_WINDOW`].
//! - Every other browser is refused all navigation to `ass:` URLs.
//!
//! What it cannot tell apart:
//!
//! - CEF reports the frame, not the document that sent the request. The
//!   frame's URL changes once the browser process has handled a commit, a
//!   moment after the new document started running. So a new shell page can
//!   be refused at first (it is told when to try again, see
//!   [`address_changed`]), and a document that has just replaced the shell
//!   page can be answered at first.
//! - A child frame of the shell page with no URL of its own (an `iframe` with
//!   no `src`) has the shell page's origin, and CEF reports its requests as
//!   the main frame's.
//!
//! Neither is reachable from web content: it takes the shell's own code, or
//! the debugging port, to put another document in the shell view. What keeps
//! other code out of the shell origin is the page's Content-Security-Policy
//! (only its own script files, no inline script, nothing loaded from anywhere
//! else) and Trusted Types (no sink that turns a string into markup or
//! script), not these checks.

use ass::scheme::{SCHEME, SHELL_HOST, SHELL_URL};
use cef::*;
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use crate::address;
use crate::tabs;

/// Applied to every response. Scripts, styles and connections are limited to
/// the page's own origin; everything else is off. Trusted Types with no policy
/// makes the engine reject `innerHTML` and the other string-to-code sinks.
const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
     connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'; \
     require-trusted-types-for 'script'; trusted-types 'none'";

/// The largest API request body that is accepted.
const MAX_BODY: usize = 16 * 1024;

/// The whole of the Rust-to-page channel: three constants. Nothing is ever
/// interpolated into script that runs in the shell page.
const STATE_CHANGED_SCRIPT: &str = "dispatchEvent(new Event('ass-state'))";
const FOCUS_ADDRESS_SCRIPT: &str = "dispatchEvent(new Event('ass-focus-address'))";
const NEW_TAB_SCRIPT: &str = "dispatchEvent(new Event('ass-new-tab'))";

/// The shell page is told about changes at most this often, in milliseconds.
/// A page can change its title thousands of times a second; the shell only
/// needs the latest one.
const NOTIFY_INTERVAL_MS: i64 = 50;

/// The shell page is loaded again at most this many times in
/// [`RELOAD_WINDOW`], so a shell page that keeps failing cannot start a loop.
const MAX_RELOADS: usize = 3;
const RELOAD_WINDOW: Duration = Duration::from_secs(60);

struct State {
    /// Identifier of the shell browser. Set once, never cleared: CEF does not
    /// reuse identifiers, so a later browser can never pass for the shell.
    shell_id: Option<i32>,
    shell: Option<Browser>,
    /// A notice to the shell page is already scheduled.
    notify_pending: bool,
    /// When the shell page was last loaded again, within [`RELOAD_WINDOW`].
    reloads: Vec<Instant>,
}

/// Read on the IO thread (requests) and written on the UI thread. Never held
/// across a call into CEF, and never together with the `Handler` or tabs lock.
static STATE: Mutex<State> = Mutex::new(State {
    shell_id: None,
    shell: None,
    notify_pending: false,
    reloads: Vec::new(),
});

fn state() -> MutexGuard<'static, State> {
    STATE.lock().expect("Failed to lock shell state")
}

/// Serves `ass://shell/` to the shell browser. Call once CEF is initialized.
pub fn register_scheme_handler() {
    let mut factory = ShellSchemeHandlerFactory::new();
    let registered = register_scheme_handler_factory(
        Some(&CefString::from(SCHEME)),
        Some(&CefString::from(SHELL_HOST)),
        Some(&mut factory),
    );
    assert_eq!(registered, 1, "cannot serve {SHELL_URL}");
}

/// Records which browser is the shell. Called from the shell view's own
/// creation callback; the first call wins.
pub fn set_shell_browser(browser: &Browser) {
    let (id, browser) = (browser.identifier(), browser.clone());
    let mut state = state();
    if state.shell_id.is_none() {
        state.shell_id = Some(id);
        state.shell = Some(browser);
    }
}

/// Releases the reference to a browser that is about to be destroyed.
pub fn browser_closed(browser: &Browser) {
    let id = browser.identifier();
    let released = {
        let mut state = state();
        (state.shell_id == Some(id)).then(|| state.shell.take())
    };
    // The reference goes back to CEF here, after the lock is released.
    drop(released);
}

pub fn is_shell(browser: &Browser) -> bool {
    let id = browser.identifier();
    state().shell_id == Some(id)
}

/// True when `url`, without its fragment, is the shell page.
fn is_shell_page(url: &str) -> bool {
    url.split_once('#').map_or(url, |(page, _)| page) == SHELL_URL
}

/// The navigation policy for every browser. True means cancel.
pub fn blocks_navigation(browser: &Browser, url: &str) -> bool {
    if is_shell(browser) {
        // The shell view shows the shell page and nothing else.
        url != SHELL_URL
    } else {
        // Web content never navigates to the browser's own pages.
        is_internal_url(url)
    }
}

/// The popup policy every browser shares. True means cancel. The tabs add
/// their own rules (`tabs::popup_allowed`).
pub fn blocks_popup(browser: &Browser, target_url: &str) -> bool {
    // The shell view never opens a popup, and no popup opens on the
    // browser's own pages.
    is_shell(browser) || is_internal_url(target_url)
}

/// True for any URL with the `ass` scheme, however it is capitalized.
fn is_internal_url(url: &str) -> bool {
    url.split_once(':')
        .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case(SCHEME))
}

/// The shell's main frame is at a new address.
pub fn address_changed(url: Option<&CefString>) {
    let url = url.map(ToString::to_string).unwrap_or_default();
    if is_shell_page(&url) {
        // Commands are answered from now on (see `serves`), which can be
        // later than the page's first request. The notice tells the page to
        // try again.
        schedule_notify();
    } else {
        reload_shell("its main frame left the shell page");
    }
}

/// The shell's renderer process ended unexpectedly.
pub fn render_process_ended() {
    reload_shell("its renderer process ended");
}

/// Loads the shell page again, unless that already happened [`MAX_RELOADS`]
/// times in the last [`RELOAD_WINDOW`].
fn reload_shell(reason: &'static str) {
    let allowed = {
        let mut state = state();
        let now = Instant::now();
        state
            .reloads
            .retain(|at| now.duration_since(*at) < RELOAD_WINDOW);
        let allowed = state.reloads.len() < MAX_RELOADS;
        if allowed {
            state.reloads.push(now);
        }
        allowed
    };
    // Fixed text only: nothing from a page is logged.
    if allowed {
        log_line!("Ass: the shell view lost its page ({reason}); loading {SHELL_URL} again");
        // Not from inside the callback that reported it.
        let mut task = ReloadShell::new();
        post_task(ThreadId::UI, Some(&mut task));
    } else {
        log_line!(
            "Ass: the shell view lost its page ({reason}); not loading it again, \
             it was reloaded {MAX_RELOADS} times in {}s",
            RELOAD_WINDOW.as_secs()
        );
    }
}

wrap_task! {
    struct ReloadShell;

    impl Task {
        fn execute(&self) {
            let shell = state().shell.clone();
            if let Some(frame) = shell.and_then(|shell| shell.main_frame()) {
                frame.load_url(Some(&CefString::from(SHELL_URL)));
            }
        }
    }
}

/// Tells the shell page about a change within [`NOTIFY_INTERVAL_MS`]. Changes
/// that arrive in the meantime are covered by the same notice: the page
/// always fetches the latest state.
pub fn schedule_notify() {
    let already_scheduled = std::mem::replace(&mut state().notify_pending, true);
    if !already_scheduled {
        let mut task = NotifyShell::new();
        if post_delayed_task(ThreadId::UI, Some(&mut task), NOTIFY_INTERVAL_MS) == 0 {
            // CEF is shutting down. Leave the flag as it would be if the task
            // had run.
            state().notify_pending = false;
        }
    }
}

wrap_task! {
    struct NotifyShell;

    impl Task {
        fn execute(&self) {
            state().notify_pending = false;
            // The window title shows the same state, at the same rate.
            tabs::show_window_title();
            run_in_shell(STATE_CHANGED_SCRIPT);
        }
    }
}

/// Cmd+L: the keyboard goes to the shell view, and the page focuses its
/// address input.
pub fn focus_address() {
    focus_shell();
    run_in_shell(FOCUS_ADDRESS_SCRIPT);
}

/// Cmd+T: the keyboard goes to the shell view, and the page takes the address
/// of a new tab.
pub fn new_tab() {
    focus_shell();
    run_in_shell(NEW_TAB_SCRIPT);
}

fn focus_shell() {
    let shell = state().shell.clone();
    if let Some(mut shell) = shell
        && let Some(view) = browser_view_get_for_browser(Some(&mut shell))
    {
        view.request_focus();
    }
}

/// Runs one of the constant scripts above in the shell page.
fn run_in_shell(script: &'static str) {
    // Before the shell page has loaded there is nobody to tell. The page
    // fetches the state when it starts.
    if let Some(frame) = shell_frame() {
        frame.execute_java_script(Some(&CefString::from(script)), None, 0);
    }
}

/// The shell's main frame while it shows the shell page, as far as the
/// browser process has seen.
fn shell_frame() -> Option<Frame> {
    let shell = state().shell.clone();
    let frame = shell.and_then(|shell| shell.main_frame())?;
    is_shell_page(&CefString::from(&frame.url()).to_string()).then_some(frame)
}

/// A complete response, built before the request is answered.
#[derive(Clone)]
struct Reply {
    status: i32,
    mime_type: &'static str,
    body: Cow<'static, [u8]>,
}

impl Reply {
    fn asset(mime_type: &'static str, body: &'static [u8]) -> Self {
        Self {
            status: 200,
            mime_type,
            body: Cow::Borrowed(body),
        }
    }

    fn json(status: i32, value: &impl Serialize) -> Self {
        Self {
            status,
            mime_type: "application/json",
            body: Cow::Owned(serde_json::to_vec(value).expect("reply is not JSON")),
        }
    }

    fn error(status: i32, message: &'static str) -> Self {
        #[derive(Serialize)]
        struct Error {
            error: &'static str,
        }
        Self::json(status, &Error { error: message })
    }

    fn done() -> Self {
        #[derive(Serialize)]
        struct Done {
            ok: bool,
        }
        Self::json(200, &Done { ok: true })
    }
}

/// A command that runs on the UI thread.
#[derive(Clone)]
enum Command {
    State,
    Open(String),
    Navigate(String),
    Activate(u32),
    Close(u32),
    Go(tabs::Go),
}

/// How a request is answered: now, on the IO thread, or later, from the UI
/// thread.
enum Answer {
    Now(Reply),
    Later(Command),
}

/// The command a URL names, if it is a command URL. The gate (`serves`) and
/// the router (`respond`) both use this, so they agree on what a command is.
fn command_name(url: &str) -> Option<&str> {
    url.strip_prefix(SHELL_URL)?.strip_prefix("api/")
}

/// Routes a request that has already been checked to come from the shell page.
/// Only exact URLs match: no query strings, no path tricks.
fn respond(request: &Request) -> Answer {
    let url = CefString::from(&request.url()).to_string();
    let method = CefString::from(&request.method()).to_string();
    let Some(path) = url.strip_prefix(SHELL_URL) else {
        return Answer::Now(Reply::error(404, "not found"));
    };

    if let Some(command) = command_name(&url) {
        return if method == "POST" {
            read_body(request)
                .and_then(|body| parse_command(command, &body))
                .unwrap_or_else(Answer::Now)
        } else {
            Answer::Now(Reply::error(405, "commands are sent with POST"))
        };
    }

    let asset = match path {
        "" => Reply::asset("text/html", include_bytes!("shell_ui/index.html")),
        "shell.css" => Reply::asset("text/css", include_bytes!("shell_ui/shell.css")),
        "shell.js" => Reply::asset("text/javascript", include_bytes!("shell_ui/shell.js")),
        _ => return Answer::Now(Reply::error(404, "not found")),
    };
    Answer::Now(if method == "GET" {
        asset
    } else {
        Reply::error(405, "pages are read with GET")
    })
}

/// Reads an API request body: JSON, in memory, at most [`MAX_BODY`] bytes.
fn read_body(request: &Request) -> Result<Vec<u8>, Reply> {
    let content_type = request.header_by_name(Some(&CefString::from("Content-Type")));
    if CefString::from(&content_type).to_string() != "application/json" {
        return Err(Reply::error(415, "the body must be application/json"));
    }

    let malformed = || Reply::error(400, "malformed body");
    let post_data = request.post_data().ok_or_else(malformed)?;
    if post_data.has_excluded_elements() != 0 {
        return Err(malformed());
    }
    // `elements` fills as many slots as it is given.
    let mut elements = vec![None; post_data.element_count()];
    post_data.elements(Some(&mut elements));

    let mut body = Vec::new();
    for element in elements {
        let element = element.ok_or_else(malformed)?;
        if element.get_type() != PostdataelementType::BYTES {
            return Err(malformed());
        }
        let size = element.bytes_count();
        if body.len() + size > MAX_BODY {
            return Err(Reply::error(413, "body too large"));
        }
        let start = body.len();
        body.resize(start + size, 0);
        let read = element.bytes(size, body[start..].as_mut_ptr());
        body.truncate(start + read.min(size));
    }
    Ok(body)
}

/// The arguments of a command that takes none.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArguments {}

/// What the user typed: an address, or words to search for.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    input: String,
}

/// A tab, by the identifier in the state.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TabId {
    id: u32,
}

/// Parses the arguments of a command. The body must be exactly one JSON
/// object with exactly the expected fields: unknown, missing and duplicate
/// fields are all errors.
fn arguments<'a, T: Deserialize<'a>>(body: &'a [u8]) -> Result<T, Reply> {
    let malformed = || Reply::error(400, "malformed body");
    // serde would also accept an array of the fields in order.
    if body.first() != Some(&b'{') {
        return Err(malformed());
    }
    serde_json::from_slice(body).map_err(|_| malformed())
}

/// Every command the shell page can send. This match is the whole interface:
/// a command that is not listed here does not exist. Runs on the IO thread;
/// what changes or reads the tabs runs later in [`run_command`].
fn parse_command(command: &str, body: &[u8]) -> Result<Answer, Reply> {
    let later = |command| Ok(Answer::Later(command));
    match command {
        // Lets the page check that the channel works.
        "ping" => {
            let NoArguments {} = arguments(body)?;
            Ok(Answer::Now(Reply::done()))
        }
        // The state the page shows (`tabs::State`).
        "state" => {
            let NoArguments {} = arguments(body)?;
            later(Command::State)
        }
        // A new tab in front, loading the input.
        "open" => {
            let Input { input } = arguments(body)?;
            later(Command::Open(input))
        }
        // The input in the active tab; with no tab, the same as `open`.
        "navigate" => {
            let Input { input } = arguments(body)?;
            later(Command::Navigate(input))
        }
        "activate" => {
            let TabId { id } = arguments(body)?;
            later(Command::Activate(id))
        }
        // The page's leave-site prompt can cancel it.
        "close" => {
            let TabId { id } = arguments(body)?;
            later(Command::Close(id))
        }
        // On the active tab. With no tab: refused.
        "back" | "forward" | "reload" | "stop" => {
            let NoArguments {} = arguments(body)?;
            later(Command::Go(match command {
                "back" => tabs::Go::Back,
                "forward" => tabs::Go::Forward,
                "reload" => tabs::Go::Reload,
                _ => tabs::Go::Stop,
            }))
        }
        _ => Err(Reply::error(404, "unknown command")),
    }
}

/// Runs a command on the UI thread and builds its reply.
fn run_command(command: &Command) -> Reply {
    let result = match command {
        Command::State => return Reply::json(200, &tabs::state()),
        Command::Open(input) => return load_input(input, tabs::open),
        Command::Navigate(input) => return load_input(input, tabs::navigate),
        Command::Activate(id) => tabs::activate(*id),
        Command::Close(id) => tabs::close(*id),
        Command::Go(action) => tabs::go(*action),
    };
    result.map_or_else(refused, |()| Reply::done())
}

/// Resolves what the user typed (`address::resolve`) and loads it with
/// `load`. The reply says whether it went to an address or to a search.
fn load_input(input: &str, load: impl FnOnce(&str) -> Result<(), tabs::Refused>) -> Reply {
    #[derive(Serialize)]
    struct Went {
        ok: bool,
        went: &'static str,
    }
    let resolved = match address::resolve(input) {
        Ok(resolved) => resolved,
        // Fixed text: nothing typed is repeated or logged.
        Err(reason) => return Reply::error(400, reason.message()),
    };
    match load(resolved.url()) {
        Ok(()) => Reply::json(
            200,
            &Went {
                ok: true,
                went: resolved.went(),
            },
        ),
        Err(reason) => refused(reason),
    }
}

/// Every refusal is a 4xx with a JSON body `{"error": "<text>"}`: the page
/// shows the text.
fn refused(reason: tabs::Refused) -> Reply {
    match reason {
        tabs::Refused::NoSuchTab => Reply::error(404, "that tab is closed"),
        tabs::Refused::NoTab => Reply::error(409, "no tab is open"),
        tabs::Refused::TooManyTabs => Reply::error(409, "too many tabs are open"),
        tabs::Refused::Quitting => Reply::error(409, "the browser is quitting"),
        tabs::Refused::Failed => Reply::error(409, "the tab could not be opened"),
    }
}

/// The gate: true when a request for an `ass://shell/` URL may be answered.
///
/// CEF resolves `browser` and `frame` in the browser process from the frame
/// the request was made in, so a page cannot forge them. See the module
/// documentation for what this cannot tell apart.
fn serves(browser: &Browser, frame: &Frame, request: &Request) -> bool {
    if !is_shell(browser) || frame.is_main() == 0 {
        return false;
    }
    let url = CefString::from(&request.url()).to_string();
    if command_name(&url).is_none() {
        // The page and its assets: fixed bytes compiled into the binary, with
        // no data in them. The frame's URL is not checked for these: CEF
        // updates it only after the new document has requested them.
        return true;
    }
    // Commands answer only while the shell's main frame shows the shell page,
    // as far as the browser process has seen (see the module documentation).
    is_shell_page(&CefString::from(&frame.url()).to_string())
}

/// The reply slot shared by a resource handler and the task that fills it.
type ReplySlot = Arc<Mutex<Option<Reply>>>;

fn lock(slot: &ReplySlot) -> MutexGuard<'_, Option<Reply>> {
    slot.lock().expect("Failed to lock a reply")
}

/// The answer to a command that waits for the UI thread. It is always given,
/// as a reply: the command's own, or a refusal when the task is dropped
/// without running (CEF shutting down). A request is never left waiting.
struct PendingReply {
    reply: ReplySlot,
    callback: Callback,
    answered: AtomicBool,
    /// Set by the resource handler's `cancel`: the request is gone.
    cancelled: Arc<AtomicBool>,
}

impl PendingReply {
    fn answer(&self, reply: Reply) {
        *lock(&self.reply) = Some(reply);
        self.answered.store(true, Ordering::Release);
        self.callback.cont();
    }
}

impl Drop for PendingReply {
    fn drop(&mut self) {
        if !*self.answered.get_mut() {
            self.answer(refused(tabs::Refused::Quitting));
        }
    }
}

wrap_task! {
    struct RunCommand {
        command: Command,
        pending: Arc<PendingReply>,
    }

    impl Task {
        fn execute(&self) {
            // The gate ran on the IO thread when the request came in. Before
            // anything changes, the shell must still show its page, and the
            // request must still be there to answer.
            let reply = if shell_frame().is_none() {
                Reply::error(403, "the shell page is not loaded")
            } else if self.pending.cancelled.load(Ordering::Acquire) {
                Reply::error(409, "the request was cancelled")
            } else {
                run_command(&self.command)
            };
            self.pending.answer(reply);
        }
    }
}

wrap_scheme_handler_factory! {
    struct ShellSchemeHandlerFactory;

    impl SchemeHandlerFactory {
        // Called on the IO thread for every `ass://shell/` request.
        fn create(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _scheme_name: Option<&CefString>,
            request: Option<&mut Request>,
        ) -> Option<ResourceHandler> {
            let from_shell_page = match (&browser, &frame, &request) {
                (Some(browser), Some(frame), Some(request)) => serves(browser, frame, request),
                _ => false,
            };
            let (reply, command) = match request {
                Some(request) if from_shell_page => match respond(request) {
                    Answer::Now(reply) => (Some(reply), None),
                    Answer::Later(command) => (None, Some(command)),
                },
                _ => {
                    // Fixed text and a number only: nothing from the page is logged.
                    let id = browser.map_or(0, |browser| browser.identifier());
                    log_line!("Ass: refused an {SCHEME}: request from browser {id}");
                    (None, None)
                }
            };
            Some(ShellResourceHandler::new(
                command,
                Arc::new(Mutex::new(reply)),
                Arc::new(AtomicUsize::new(0)),
                Arc::new(AtomicBool::new(false)),
            ))
        }
    }
}

wrap_resource_handler! {
    struct ShellResourceHandler {
        // A command still to run on the UI thread, which fills `reply`.
        command: Option<Command>,
        // With no command, `None` fails the request without a response.
        reply: ReplySlot,
        // How much of the body has been read.
        offset: Arc<AtomicUsize>,
        // Set when CEF ends the request: after the reply, or before it when
        // the page goes away or aborts the request.
        cancelled: Arc<AtomicBool>,
    }

    impl ResourceHandler {
        fn cancel(&self) {
            self.cancelled.store(true, Ordering::Release);
        }

        fn open(
            &self,
            _request: Option<&mut Request>,
            handle_request: Option<&mut i32>,
            callback: Option<&mut Callback>,
        ) -> i32 {
            let Some(handle_request) = handle_request else {
                return 0;
            };
            if let (Some(command), Some(callback)) = (&self.command, callback) {
                // Answered later: 0 here and 1 returned, then `cont` on the
                // callback once the reply is in (`PendingReply`).
                *handle_request = 0;
                let pending = Arc::new(PendingReply {
                    reply: self.reply.clone(),
                    callback: callback.clone(),
                    answered: AtomicBool::new(false),
                    cancelled: self.cancelled.clone(),
                });
                let mut task = RunCommand::new(command.clone(), pending);
                // A task that cannot be posted is dropped, which answers with
                // a refusal.
                post_task(ThreadId::UI, Some(&mut task));
                return 1;
            }
            // The answer is already known. 1 continues, 0 cancels the request.
            *handle_request = 1;
            lock(&self.reply).is_some().into()
        }

        fn response_headers(
            &self,
            response: Option<&mut Response>,
            response_length: Option<&mut i64>,
            _redirect_url: Option<&mut CefString>,
        ) {
            let reply = lock(&self.reply);
            let (Some(response), Some(reply)) = (response, reply.as_ref()) else {
                return;
            };
            response.set_status(reply.status);
            response.set_mime_type(Some(&CefString::from(reply.mime_type)));
            response.set_charset(Some(&CefString::from("utf-8")));
            // CEF derives Content-Type from the two values above only for
            // status 200, so it is also set as a header.
            let content_type = format!("{}; charset=utf-8", reply.mime_type);
            for (name, value) in [
                ("Content-Type", content_type.as_str()),
                ("Content-Security-Policy", CONTENT_SECURITY_POLICY),
                ("X-Content-Type-Options", "nosniff"),
                ("Cache-Control", "no-store"),
            ] {
                response.set_header_by_name(
                    Some(&CefString::from(name)),
                    Some(&CefString::from(value)),
                    1,
                );
            }
            if let Some(response_length) = response_length {
                *response_length = reply.body.len() as i64;
            }
        }

        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        fn read(
            &self,
            data_out: *mut u8,
            bytes_to_read: i32,
            bytes_read: Option<&mut i32>,
            _callback: Option<&mut ResourceReadCallback>,
        ) -> i32 {
            let reply = lock(&self.reply);
            let (Some(bytes_read), Some(reply)) = (bytes_read, reply.as_ref()) else {
                return 0;
            };
            let offset = self.offset.load(Ordering::Relaxed);
            let rest = reply.body.get(offset..).unwrap_or_default();
            let count = rest.len().min(usize::try_from(bytes_to_read).unwrap_or(0));
            *bytes_read = count as i32;
            if count == 0 || data_out.is_null() {
                // 0 bytes and 0 returned: the response is complete.
                *bytes_read = 0;
                return 0;
            }
            // SAFETY: CEF provides `bytes_to_read` writable bytes at `data_out`,
            // and `count` is at most that and at most what is left of the body.
            unsafe { std::ptr::copy_nonoverlapping(rest.as_ptr(), data_out, count) };
            self.offset.store(offset + count, Ordering::Relaxed);
            1
        }
    }
}
