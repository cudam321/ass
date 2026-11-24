//! The last step of a quit: once no browser is left, the cookie store is flushed, and only then
//! does the message loop end (known issue 2, `docs/ARCHITECTURE.md`).
//!
//! Why: inside `shutdown()` Chromium waits at most 10 s for the network service helper to exit,
//! and that exit is when the helper commits the cookies changed since its last commit (every
//! 30 s, or after 512 changes). Chromium's teardown watchdog ends the app at 10 s, so on a very
//! busy Mac a login made just before the quit could be lost. Asking for the commit while the
//! message loop still runs takes it out of that race.
//!
//! What the answer means, read at the pin: CEF's `FlushStore` reaches the network service's
//! cookie store, which commits every pending change in one transaction and then answers
//! (`cookie_manager_impl.cc:223-233`, `:365-378`; `cookie_monster.cc:513-522`;
//! `sqlite_persistent_store_backend_base.cc:226-233`). The answer comes after the commit has
//! run, whether or not it succeeded, and at once when the store has not finished loading. A
//! stopped helper never answers, and one that dies drops the answer: the bound covers both.
//! Seen once in a run (known issue 2): a cookie set just before the quit was in the cookie
//! file at the moment of the answer.
//!
//! The state this adds: no browser left and the message loop still running, for at most
//! [`BOUND_MS`]. Every way out ends in [`Wait::finish`], which acts once.
//!
//! What can still open a browser in it. Nothing of ours: the tabs refuse to open while
//! quitting, and the window and the shell are gone. But a DevTools client on
//! `--remote-debugging-port` can create one (`Target.createTarget` opens a window of Chrome's
//! own). The app then runs until that window closes: `quit_message_loop` only drops CEF's own
//! hold on the loop, and CEF then waits for every Chrome browser window to close
//! (`libcef/browser/main_runner.cc:172-179`, `:376-385`). Whether that switch is honoured at
//! all in a release build is the switch policy's question (known issue 4).
//!
//! Such a browser gets our client: CEF hands every browser Chrome creates itself the
//! application's default client (`libcef/browser/chrome/chrome_browser_delegate.cc:522-528`),
//! and ours is `Handler`'s (`app.rs`). So `Handler` lists it, and its close empties the list
//! again and calls [`flush_then_quit`] a second time. Read, not run. The second call starts a
//! wait of its own, with its own bound, flush and line; a wait still running goes on
//! unchanged, because each acts once. Its `quit_message_loop` does nothing more: CEF allows the
//! call more than once (`main_runner.cc:376-385`). The loop can end before the second wait's
//! answer or bound, and its line is then not printed. If that answer or bound runs inside
//! `shutdown()` instead, its `quit_message_loop` finds the context shutting down and returns
//! (`libcef/browser/context.cc:301-306`, `context.h:138-140`). It also fails a DCHECK there,
//! which is compiled out of the framework we bundle: that file's DCHECK messages are not in
//! the binary, and its `LOG(ERROR)` messages are.

use cef::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

/// How long the quit waits for the flush's answer. Never timed under load yet: the line
/// printed at each quit gives the time, and the bound is set from that.
const BOUND_MS: i64 = 2000;

/// Flushes the cookie store, then quits the message loop: when the store answers, or after
/// [`BOUND_MS`], whichever comes first. At once when the flush cannot be asked for. Call on the
/// UI thread, once no browser is left.
pub fn flush_then_quit() {
    debug_assert_ne!(currently_on(ThreadId::UI), 0);
    let wait = Arc::new(Wait {
        started: Instant::now(),
        finished: AtomicBool::new(false),
    });

    // The bound first, so that no flush is ever waited for without one.
    let mut bound = FlushBound::new(wait.clone());
    if post_delayed_task(ThreadId::UI, Some(&mut bound), BOUND_MS) == 0 {
        wait.finish(Outcome::NotStarted);
        return;
    }
    // Our tabs and the shell use the global request context (no other is ever created), so
    // this is their cookie store. A manager that is still initializing keeps the request and
    // runs it once ready (`cookie_manager_impl.cc:223-231`). 0 only when the browser context
    // is gone (`:369-372`).
    let asked = cookie_manager_get_global_manager(None).is_some_and(|manager| {
        let mut answered = FlushAnswered::new(wait.clone());
        manager.flush_store(Some(&mut answered)) != 0
    });
    if !asked {
        wait.finish(Outcome::NotStarted);
    }
}

enum Outcome {
    Answered,
    NoAnswer,
    NotStarted,
}

struct Wait {
    started: Instant,
    finished: AtomicBool,
}

impl Wait {
    /// Ends the wait: one line on stderr, then the message loop quits. Only the first call
    /// acts. The bound after an answer does nothing; an answer after the bound is only
    /// reported, with a second line (it could come inside `shutdown()`, if CEF runs it among
    /// the tasks left on the UI thread). Each line carries the moment as seconds since the
    /// epoch, so a test can set it beside the cookie file's last change.
    fn finish(&self, outcome: Outcome) {
        let ms = self.started.elapsed().as_millis();
        let at = clock();
        if self.finished.swap(true, Ordering::SeqCst) {
            if let Outcome::Answered = outcome {
                log_line!(
                    "Ass: quit: the cookie flush answered in {ms} ms, at {at}, after the bound; ignored"
                );
            }
            return;
        }
        // "Answered", never "saved": the answer means the commit ran, not that it succeeded.
        match outcome {
            Outcome::Answered => {
                log_line!("Ass: quit: the cookie flush answered in {ms} ms, at {at}")
            }
            Outcome::NoAnswer => {
                log_line!(
                    "Ass: quit: the cookie flush gave no answer within {BOUND_MS} ms, at {at}"
                )
            }
            Outcome::NotStarted => {
                log_line!("Ass: quit: the cookie flush could not be started, at {at}")
            }
        }
        quit_message_loop();
    }
}

/// Now, as seconds since the epoch with milliseconds.
fn clock() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}", now.as_secs(), now.subsec_millis())
}

wrap_completion_callback! {
    struct FlushAnswered {
        wait: Arc<Wait>,
    }

    impl CompletionCallback {
        // CEF posts this to the UI thread (`cookie_manager_impl.cc:38-45`).
        fn on_complete(&self) {
            self.wait.finish(Outcome::Answered);
        }
    }
}

wrap_task! {
    struct FlushBound {
        wait: Arc<Wait>,
    }

    impl Task {
        fn execute(&self) {
            self.wait.finish(Outcome::NoAnswer);
        }
    }
}
