//! Ass: a Chromium-based browser. This is the browser (main) process.
//! Sub-processes run `ass_helper` inside the sandbox.

/// Writes one line to stderr, as `eprintln!` does, and ignores a failed write. `eprintln!`
/// panics when stderr cannot be written, and a panic inside a CEF callback ends the app. Every
/// line the app writes goes through this.
macro_rules! log_line {
    ($($arg:tt)*) => {
        $crate::write_line(&mut ::std::io::stderr(), ::std::format_args!($($arg)*))
    };
}

mod address;
mod app;
mod flush;
mod handler;
mod keychain;
mod leftover;
mod mac;
mod profile;
mod shell;
mod tabs;
mod test_window;

use cef::*;
use std::{io::Write, path::Path, process::ExitCode};

/// Exit code when the start is refused before anything is opened: a profile folder that cannot
/// be used (`profile::refusal`: not an absolute path, not a folder, not readable, named in
/// `ASS_PROFILE_DIR` and missing, or the default one with no home directory to make it in),
/// `--use-mock-keychain` on the default profile, and test
/// window mode asked for and refused (`test_window.rs`, and here when `--use-mock-keychain`
/// is missing). Each of those ends the process before `NSApp` exists. Also, after the keychain
/// gate, when the profile folder is not there and the default one could not be made
/// (`profile::ready`). 64 is the usage error of `sysexits.h`; CEF's own result codes stop
/// at 43.
const EXIT_USAGE: u8 = 64;

/// Exit code when the keychain gate stops the start (`keychain.rs`): Quit at our alert, no
/// lasting access to the cookie key, the key not reachable while this profile or the default
/// one holds data, or the keychain's interaction setting not readable or not restored. 77 is
/// the permission error of `sysexits.h`.
const EXIT_KEYCHAIN_REFUSED: u8 = 77;

/// The writer behind [`log_line!`]: one line to `out`, a failed write ignored.
fn write_line(out: &mut impl Write, line: std::fmt::Arguments) {
    let _ = writeln!(out, "{line}");
}

/// Exit codes: 0 after a normal quit, and when another instance already owns
/// the profile and takes over this launch; CEF's own result code when it fails
/// to initialize (1 when that code does not fit in a byte); 64
/// (`EXIT_USAGE`) when the start is refused before anything is opened; 77
/// (`EXIT_KEYCHAIN_REFUSED`) when the keychain gate stops the start.
fn main() -> ExitCode {
    // Load the CEF framework from the app bundle.
    let loader = library_loader::LibraryLoader::new(&std::env::current_exe().unwrap(), false);
    assert!(loader.load(), "cannot load the CEF framework");

    // Initialize the CEF API version.
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);

    let args = args::Args::new();

    // A profile path CEF would not take as given, or could not make into a folder: it would
    // carry on in a folder we never chose (`profile.rs`).
    if let Some(reason) = profile::refusal() {
        if profile::is_given() {
            log_line!("Ass: ASS_PROFILE_DIR refused: {reason}");
        } else {
            log_line!("Ass: the default profile folder refused: {reason}");
        }
        return ExitCode::from(EXIT_USAGE);
    }

    // The stand-in keychain on the default profile: its fixed key did not encrypt that
    // profile's cookies, so Chromium would delete them, and the gate is skipped with it
    // (`keychain.rs`). Refused here, before `NSApp` exists.
    let mock_switch = keychain::mock_switch(&args);
    if mock_switch && profile::is_default() {
        log_line!("Ass: --use-mock-keychain refused: the profile is the default profile folder");
        return ExitCode::from(EXIT_USAGE);
    }

    // Test-only, off unless set up by a test: see `test_window.rs`. A refused
    // request ends the process here, before `NSApp` or any window exists.
    if !test_window::init() {
        return ExitCode::from(EXIT_USAGE);
    }
    // Which keychain Chromium will read the cookie key from (`keychain.rs`). Test window mode
    // must use the stand-in: without it, refused here like the requests above, before any
    // keychain call.
    let chosen = keychain::choose(test_window::enabled(), mock_switch);
    if chosen == keychain::Keychain::Refused {
        log_line!("Ass: ASS_TEST_WINDOW refused: --use-mock-keychain is not on the command line");
        return ExitCode::from(EXIT_USAGE);
    }
    mac::setup_application();

    let main_args = args.as_main_args();

    // Sub-processes use the helper executable, so this is always the browser process.
    let ret = execute_process(Some(main_args), None::<&mut App>, std::ptr::null_mut());
    assert_eq!(ret, -1, "cannot execute browser process");

    let profile = profile::dir();
    let profile = profile.to_str().expect("profile path is not UTF-8");
    // Chromium reads the cookie key inside `initialize`, and a failed read destroys saved
    // logins. The gate makes sure that read will succeed, or the app does not start.
    if !keychain::gate(chosen, Path::new(profile), &profile::default_dir()) {
        return ExitCode::from(EXIT_KEYCHAIN_REFUSED);
    }
    // The profile folder is there before Chromium is asked for it (`profile.rs`). The first
    // thing a start writes: an empty default profile folder, if there was none. After the
    // gate, so that a start the gate stops has changed nothing.
    if !profile::ready() {
        log_line!("Ass: the profile folder is not there and could not be made");
        return ExitCode::from(EXIT_USAGE);
    }
    // Helpers an earlier run of this profile left running could still write into it once this
    // run has begun (`leftover.rs`). Before `initialize`, so before this run opens anything there.
    leftover::end_helpers(Path::new(profile));
    let settings = Settings {
        // The sandbox stays on. Never turn this off to work around a problem.
        no_sandbox: 0,
        root_cache_path: CefString::from(profile),
        cache_path: CefString::from(profile),
        // Own Keychain item, so the cookie encryption key is not shared with other Chromium apps.
        keychain_service_name: CefString::from(keychain::SERVICE),
        keychain_account_name: CefString::from(keychain::ACCOUNT),
        ..Default::default()
    };

    let mut app = app::AssApp::new();
    let initialized = initialize(
        Some(main_args),
        Some(&settings),
        Some(&mut app),
        std::ptr::null_mut(),
    );
    if initialized != 1 {
        // Not always an error: when another instance already owns this profile,
        // CEF hands the launch to it and this process exits normally.
        let code = get_exit_code();
        let handed_off =
            code == sys::cef_resultcode_t::CEF_RESULT_CODE_NORMAL_EXIT_PROCESS_NOTIFIED as i32;
        return if handed_off {
            ExitCode::SUCCESS
        } else {
            log_line!("Ass: CEF failed to initialize (exit code {code})");
            ExitCode::from(u8::try_from(code).unwrap_or(1))
        };
    }

    let _delegate = mac::setup_app_delegate();

    run_message_loop();

    shutdown();
    drop(loader);
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::write_line;
    use std::io::{self, Write};

    /// A stderr that cannot be written, as when the reading end of its pipe is gone.
    struct Broken;

    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    #[test]
    fn a_failed_write_is_ignored() {
        write_line(&mut Broken, format_args!("Ass: a line {}", 1));
    }

    #[test]
    fn writes_one_line() {
        let mut out = Vec::new();
        write_line(&mut out, format_args!("Ass: {} and {}", "one", 2));
        assert_eq!(out, b"Ass: one and 2\n");
    }
}
