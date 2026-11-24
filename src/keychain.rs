//! The keychain gate (decision D8): before Chromium starts, make sure its read of the cookie key
//! will succeed, and do not start on this profile otherwise. The design and what it
//! leaves open: `docs/ARCHITECTURE.md`, decision D8.
//!
//! Why: when Chromium cannot read the key, it deletes every site's cookies from the profile,
//! resets protected settings and says nothing (`docs/ARCHITECTURE.md`, known issue 22). It reads
//! the key inside `initialize`, and nothing in CEF stops that damage once it has begun. When the
//! item is missing, it makes a new key, and saved passwords that no longer decrypt are deleted.
//!
//! The four steps, all before `initialize`:
//!
//! 1. Which keychain Chromium will use (`choose`). Its stand-in exactly when the command line has
//!    `use-mock-keychain` (Chromium `keychain_key_provider.mm:41-46`, `os_crypt_switches.h:18`),
//!    read with CEF's command-line object, which is Chromium's own parser. Then the gate is
//!    skipped, and stderr gets one fixed line. Test window mode without the switch is refused by
//!    `main` with exit code 64, before `NSApp` exists and before any keychain call. So is the
//!    switch on the default profile: the stand-in's fixed key did not encrypt that profile's
//!    cookies, so Chromium would delete them (added after the security review of the code).
//! 2. A check that cannot prompt (`check`). The keychain's interaction setting off, Chromium's
//!    exact query (`keychain_v2.mm:107-127`), the setting restored on every path, a panic
//!    included (`NoDialogs`). The setting belongs to this process alone (Apple's
//!    `Globals.cpp:41-42`, `SecKeychain.cpp:869-885`).
//! 3. The decision, by allow-list (`decide`). The key was read: start. No such item: start only
//!    if neither this profile nor the default one holds data yet (`has_data`), because Chromium
//!    then makes a new key and nothing in those two profiles is lost; otherwise stop and say
//!    that the key cannot be reached. Every profile is encrypted with the one key, so a new key made for an empty
//!    profile would cost the default profile its logins at its next start. Anything else, a
//!    denial, a locked keychain or an error: step 4.
//! 4. Only then ask (`ask`). Our alert says what macOS will ask next and that only "Always Allow"
//!    lets Ass start. Then one read that may prompt, then the silent check and the same decision
//!    again. "Allow" passes that one read and records nothing, so the silent check after it
//!    fails, and the app does not start.
//!
//! The secret: the access check guards the data, so the query has to ask for it, as Chromium's
//! does. The result is released the moment the call returns, unread. Only the status is kept.
//!
//! Before it lets Chromium start, the gate reads the interaction setting back and requires the
//! value it found: left off, a keychain that locks before Chromium's read would make that read
//! fail without a dialog, which is the damage this gate exists to prevent.
//!
//! Nothing here is configurable. Not covered: a keychain that locks between this check and
//! Chromium's read (Chromium then prompts, and a Cancel still destroys data); an item whose data
//! is empty, which this check counts as read and Chromium treats as unavailable
//! (`keychain_key_provider.mm:54-58`); a profile in a folder the app is not told about, and the
//! account's default profile when `HOME` points elsewhere, which step 3 does not see.
//!
//! What keeps a test off the real keychain is this code alone: `choose` and the refusals in
//! `main`. If the switch were ever read as present when it is not, Chromium would read the real
//! item and macOS would prompt.

use cef::{ImplCommandLine, args::Args};
use objc2::{
    MainThreadMarker, class, msg_send,
    rc::{Retained, autoreleasepool},
    runtime::AnyObject,
};
use objc2_app_kit::NSApp;
use objc2_foundation::{NSDictionary, NSNumber, NSString};
use std::ffi::c_void;
use std::path::Path;

/// The keychain item that holds the cookie key: its service and account names. `main` gives
/// them to CEF, which gives them to Chromium (CEF `chrome_main_delegate_cef.cc:336-344`); the
/// gate reads the same item.
pub const SERVICE: &str = "Ass Safe Storage";
pub const ACCOUNT: &str = "Ass";

/// Chromium's switch for its stand-in keychain (`os_crypt_switches.h:18`).
const MOCK_SWITCH: &str = "use-mock-keychain";

/// `errSecSuccess` and `errSecItemNotFound` (`Security.framework/Headers/SecBase.h:326`, `:353`).
const ERR_SEC_SUCCESS: i32 = 0;
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

/// `NSAlertFirstButtonReturn` (`AppKit.framework/Headers/NSAlert.h:50`).
const FIRST_BUTTON: isize = 1000;

// Signatures from the macOS SDK's headers: `SecItem.h` (the constants, `SecItemCopyMatching` at
// `:1178`), `SecKeychain.h:639-651` and `CFBase.h:781`. `OSStatus` is `SInt32` and `Boolean` is
// `unsigned char` (`MacTypes.h:281`, `:311`). The two `SecKeychain` calls are deprecated by
// Apple; Chromium still uses them for this, because the `SecItem` ways to keep the keychain
// quiet do not apply to file-based keychains (`scoped_keychain_user_interaction_allowed.cc:16-19`).
#[allow(non_upper_case_globals)]
#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecClass: *const c_void;
    static kSecClassGenericPassword: *const c_void;
    static kSecAttrService: *const c_void;
    static kSecAttrAccount: *const c_void;
    static kSecMatchLimit: *const c_void;
    static kSecMatchLimitOne: *const c_void;
    static kSecReturnData: *const c_void;
    static kSecReturnAttributes: *const c_void;
    fn SecItemCopyMatching(query: *const c_void, result: *mut *const c_void) -> i32;
    fn SecKeychainGetUserInteractionAllowed(state: *mut u8) -> i32;
    fn SecKeychainSetUserInteractionAllowed(state: u8) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
}

/// Which keychain Chromium will read the key from, and so what the gate does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keychain {
    /// `--use-mock-keychain`: Chromium's stand-in, a fixed key in memory. The gate is skipped.
    Mock,
    /// The user's keychain: the gate runs.
    Real,
    /// Test window mode without `--use-mock-keychain`: `main` ends the process at once.
    Refused,
}

/// Step 1. `test_window`: whether test window mode is on; `mock_switch`: whether the command
/// line has `use-mock-keychain` (`mock_switch`).
pub fn choose(test_window: bool, mock_switch: bool) -> Keychain {
    match (test_window, mock_switch) {
        (_, true) => Keychain::Mock,
        (false, false) => Keychain::Real,
        (true, false) => Keychain::Refused,
    }
}

/// Whether the command line has `use-mock-keychain`, read with CEF's command-line object, which
/// is Chromium's `base::CommandLine`: the parser Chromium reads the switch with. False when CEF
/// cannot make the object, so the gate then runs.
pub fn mock_switch(args: &Args) -> bool {
    args.as_cmd_line()
        .is_some_and(|line| line.has_switch(Some(&cef::CefString::from(MOCK_SWITCH))) == 1)
}

/// Runs the gate for `keychain` on the profile `profile`, the folder `main` gives CEF.
/// `default_profile` is the default profile's folder, which may be the same one. True: start.
/// False: do not start; the reason is on stderr and, for the user, in our alert.
pub fn gate(keychain: Keychain, profile: &Path, default_profile: &Path) -> bool {
    match keychain {
        Keychain::Mock => {
            log_line!("Ass: keychain gate skipped: --use-mock-keychain");
            true
        }
        // `main` has ended the process before this. Never a keychain call for it.
        Keychain::Refused => false,
        Keychain::Real => autoreleasepool(|_| real(profile, default_profile)),
    }
}

/// The gate on the user's keychain: steps 2 to 4.
fn real(profile: &Path, default_profile: &Path) -> bool {
    let allowed = match interaction_allowed() {
        Ok(allowed) => allowed,
        Err(status) => {
            log_line!("Ass: keychain gate: cannot read the interaction setting (status {status})");
            return false;
        }
    };
    let has_data = any_data(profile, default_profile);
    let start = match decide(check(SERVICE, ACCOUNT), has_data) {
        Decision::Start => true,
        Decision::KeyMissing => {
            log_line!("Ass: keychain gate: no such item, and a profile holds data");
            alert(&KEY_MISSING);
            false
        }
        Decision::Ask => ask(has_data),
    };
    if start && interaction_allowed() != Ok(allowed) {
        log_line!("Ass: keychain gate: the interaction setting was not restored");
        return false;
    }
    start
}

/// Step 4: our alert, one read that may prompt, then the silent check and the decision again.
fn ask(has_data: bool) -> bool {
    if !alert(&BEFORE_PROMPT) {
        log_line!("Ass: keychain gate: Quit chosen before the prompt");
        return false;
    }
    read_with_prompt();
    match decide(check(SERVICE, ACCOUNT), has_data) {
        Decision::Start => true,
        Decision::KeyMissing => {
            log_line!("Ass: keychain gate: no such item, and a profile holds data");
            alert(&KEY_MISSING);
            false
        }
        Decision::Ask => {
            log_line!("Ass: keychain gate: no lasting access to the key");
            alert(&NOT_STARTED);
            false
        }
    }
}

/// What the gate does after a silent check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decision {
    Start,
    KeyMissing,
    Ask,
}

/// Step 3, the allow-list. `check`: the silent check's status, or `None` when it could not run
/// silently. `has_data`: whether a profile may hold data (`any_data`).
fn decide(check: Option<i32>, has_data: bool) -> Decision {
    match check {
        Some(ERR_SEC_SUCCESS) => Decision::Start,
        Some(ERR_SEC_ITEM_NOT_FOUND) if !has_data => Decision::Start,
        Some(ERR_SEC_ITEM_NOT_FOUND) => Decision::KeyMissing,
        _ => Decision::Ask,
    }
}

/// Whether a new key would destroy data: one key encrypts every profile, so true when the
/// profile about to start or the default profile may hold data. A third profile, in a folder
/// the app is not told about, is not seen.
fn any_data(profile: &Path, default_profile: &Path) -> bool {
    has_data(profile) || has_data(default_profile)
}

/// Whether the profile may hold data that a new key would destroy: true unless
/// `<profile>/Default`, where Chromium keeps `Cookies`, `Login Data` and `Web Data`, is certainly
/// absent. A path that is not absolute counts as holding data: CEF takes only absolute ones and
/// otherwise uses a folder of its own (`cef_types.h`, `root_cache_path`), so this check would
/// look at the wrong folder. So does any error other than "not found".
fn has_data(profile: &Path) -> bool {
    if !profile.is_absolute() {
        return true;
    }
    !matches!(
        std::fs::symlink_metadata(profile.join("Default")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    )
}

/// Step 2, the check that cannot prompt: Chromium's query with the keychain's dialogs off. The
/// query's status, or `None` when the dialogs could not be turned off; the query is then not run.
fn check(service: &str, account: &str) -> Option<i32> {
    let no_dialogs = match NoDialogs::new() {
        Ok(no_dialogs) => no_dialogs,
        Err(status) => {
            log_line!(
                "Ass: keychain gate: cannot turn the keychain's dialogs off (status {status})"
            );
            return None;
        }
    };
    let status = query(service, account);
    drop(no_dialogs);
    if status != ERR_SEC_SUCCESS {
        log_line!("Ass: keychain gate: silent check, status {status}");
    }
    Some(status)
}

/// The one read that may show the system's dialog: Chromium's query on our item, with the
/// dialogs as the process had them. Its status is only logged: the silent check after it
/// decides, so a passing "Allow" does not count.
fn read_with_prompt() {
    let status = query(SERVICE, ACCOUNT);
    log_line!("Ass: keychain gate: read with a prompt, status {status}");
}

/// Chromium's exact query for the key (`keychain_v2.mm:107-127`, sent by
/// `FindGenericPassword` at `:213-226`): the generic password with these names, its data and
/// its attributes, the first match only. Returns the status. The result, which holds the
/// secret, is released as soon as the call returns, unread.
fn query(service: &str, account: &str) -> i32 {
    autoreleasepool(|_| {
        let service = NSString::from_str(service);
        let account = NSString::from_str(account);
        let yes = NSNumber::numberWithBool(true);
        // SAFETY: Security's constants are CFStrings that live as long as the process, and a
        // CFString is an NSString (toll-free bridged).
        let key = |constant: *const c_void| unsafe { &*constant.cast::<NSString>() };
        // SAFETY: reading Security's constant pointers.
        let (keys, values): ([&NSString; 6], [&AnyObject; 6]) = unsafe {
            (
                [
                    key(kSecClass),
                    key(kSecAttrService),
                    key(kSecAttrAccount),
                    key(kSecMatchLimit),
                    key(kSecReturnData),
                    key(kSecReturnAttributes),
                ],
                [
                    key(kSecClassGenericPassword),
                    &service,
                    &account,
                    key(kSecMatchLimitOne),
                    &yes,
                    &yes,
                ],
            )
        };
        let query = NSDictionary::from_slices(&keys, &values);
        let mut result: *const c_void = std::ptr::null();
        // SAFETY: `query` is a dictionary (a CFDictionary, toll-free bridged); `result` gets an
        // owned reference or stays null (`SecItem.h:1178`, `CF_RETURNS_RETAINED`).
        let status = unsafe { SecItemCopyMatching(Retained::as_ptr(&query).cast(), &mut result) };
        if !result.is_null() {
            // SAFETY: the reference `SecItemCopyMatching` handed over, released once.
            unsafe { CFRelease(result) };
        }
        status
    })
}

/// The keychain's interaction setting: nonzero when it may show dialogs, or the failing status.
fn interaction_allowed() -> Result<u8, i32> {
    let mut allowed = 0;
    // SAFETY: writes one `Boolean`.
    let status = unsafe { SecKeychainGetUserInteractionAllowed(&mut allowed) };
    if status == ERR_SEC_SUCCESS {
        Ok(allowed)
    } else {
        Err(status)
    }
}

/// The keychain shows no dialog while this lives. Dropping it, a panic included, restores the
/// setting it found. Chromium's `ScopedKeychainUserInteractionAllowed`
/// (`scoped_keychain_user_interaction_allowed.cc:25-53`): when reading or changing the setting
/// fails, nothing was changed and nothing is restored.
struct NoDialogs {
    allowed: u8,
}

impl NoDialogs {
    fn new() -> Result<Self, i32> {
        let allowed = interaction_allowed()?;
        // SAFETY: sets this process's flag (`Globals.h:53`).
        let status = unsafe { SecKeychainSetUserInteractionAllowed(0) };
        if status != ERR_SEC_SUCCESS {
            return Err(status);
        }
        Ok(NoDialogs { allowed })
    }
}

impl Drop for NoDialogs {
    fn drop(&mut self) {
        // SAFETY: as in `new`. The status is not needed here: `real` reads the setting back
        // before it lets Chromium start.
        unsafe { SecKeychainSetUserInteractionAllowed(self.allowed) };
    }
}

/// One of the gate's alerts. Its wording has had no design review.
struct Alert {
    title: &'static str,
    text: &'static str,
    buttons: &'static [&'static str],
}

/// Before the read that may prompt.
const BEFORE_PROMPT: Alert = Alert {
    title: "Ass needs its key from your keychain",
    text: "macOS will now ask whether Ass may use \u{201c}Ass Safe Storage\u{201d}. That item holds \
           the key that encrypts your cookies on this Mac. Choose \u{201c}Always Allow\u{201d}. \
           With \u{201c}Allow\u{201d} or \u{201c}Deny\u{201d}, Ass will not start, and nothing \
           is changed.",
    buttons: &["Continue", "Quit"],
};

/// After that read, when the silent check still fails.
const NOT_STARTED: Alert = Alert {
    title: "Ass did not start",
    text: "macOS did not give Ass lasting access to \u{201c}Ass Safe Storage\u{201d}. Nothing was \
           changed, and your saved logins are untouched. Open Ass again and choose \
           \u{201c}Always Allow\u{201d}.",
    buttons: &["Quit"],
};

/// No such item, while a profile holds data. The keychain can also answer "no such item" for
/// one it could not read, so the words do not say that the item is gone.
const KEY_MISSING: Alert = Alert {
    title: "Ass cannot reach its key",
    text: "Ass may have saved data on this Mac, but \u{201c}Ass Safe Storage\u{201d} could not be \
           found or read in your keychain. Starting now would delete saved logins, so Ass did \
           not start. Nothing was changed. Do not reset or delete your keychain.",
    buttons: &["Quit"],
};

/// Shows `alert`, modal to the app, and returns whether its first button was chosen. Only that
/// exact answer counts: anything else is the last button, Quit.
fn alert(alert: &Alert) -> bool {
    let mtm = MainThreadMarker::new().expect("Not running on the main thread");
    // Launch has not finished, so the app is not active yet, and the alert would open behind the
    // app in front. `activate` replaces this call from macOS 14; the app runs from macOS 11.
    #[allow(deprecated)]
    NSApp(mtm).activateIgnoringOtherApps(true);
    // SAFETY: `NSAlert`'s methods as declared in `NSAlert.h` (`:71`, `:75`, `:100`, `:140`), on
    // the main thread, each argument an NSString.
    let response: isize = unsafe {
        let ns_alert: Retained<AnyObject> = msg_send![class!(NSAlert), new];
        let _: () = msg_send![&ns_alert, setMessageText: &*NSString::from_str(alert.title)];
        let _: () = msg_send![&ns_alert, setInformativeText: &*NSString::from_str(alert.text)];
        for title in alert.buttons {
            let _: Retained<AnyObject> =
                msg_send![&ns_alert, addButtonWithTitle: &*NSString::from_str(title)];
        }
        msg_send![&ns_alert, runModal]
    };
    response == FIRST_BUTTON
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn which_keychain() {
        assert_eq!(choose(false, true), Keychain::Mock);
        assert_eq!(choose(true, true), Keychain::Mock);
        assert_eq!(choose(false, false), Keychain::Real);
        assert_eq!(choose(true, false), Keychain::Refused);
    }

    /// Every status from -70000 to 40000 and the extremes, with and without profile data, and a
    /// check that could not run silently. The codes in `SecBase.h` run from -67904 to 0, so the
    /// sweep holds each of them, the ones the reading names among them (-128 cancelled, -25291
    /// not available, -25293 denied, -25294 no keychain, -25308 and -25315 interaction not
    /// allowed or required, -34018 entitlement).
    #[test]
    fn decision_table() {
        for has_data in [false, true] {
            assert_eq!(decide(Some(0), has_data), Decision::Start);
            assert_eq!(decide(None, has_data), Decision::Ask);
            let others = (-70000..=40000)
                .chain([i32::MIN, i32::MAX])
                .filter(|&status| status != ERR_SEC_SUCCESS && status != ERR_SEC_ITEM_NOT_FOUND);
            for status in others {
                assert_eq!(
                    decide(Some(status), has_data),
                    Decision::Ask,
                    "status {status}"
                );
            }
        }
        assert_eq!(decide(Some(-25300), false), Decision::Start);
        assert_eq!(decide(Some(-25300), true), Decision::KeyMissing);
    }

    /// A new, empty folder for one test, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("ass-keychain-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("cannot create the test folder");
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn profile_data() {
        assert!(has_data(Path::new("")), "empty path");
        assert!(has_data(Path::new("relative/profile")), "relative path");

        let scratch = Scratch::new("data");
        let profile = scratch.0.join("profile");
        assert!(!has_data(&profile), "no profile folder");
        std::fs::create_dir(&profile).expect("cannot create the profile");
        assert!(!has_data(&profile), "empty profile folder");
        std::fs::write(profile.join("Local State"), "{}").expect("cannot write a file");
        assert!(!has_data(&profile), "a profile with no Default folder");

        std::fs::create_dir(profile.join("Default")).expect("cannot create Default");
        assert!(has_data(&profile), "Default folder");
        std::fs::remove_dir(profile.join("Default")).expect("cannot remove Default");
        std::fs::write(profile.join("Default"), "").expect("cannot write Default");
        assert!(has_data(&profile), "Default as a file");
        std::fs::remove_file(profile.join("Default")).expect("cannot remove Default");
        std::os::unix::fs::symlink(scratch.0.join("nowhere"), profile.join("Default"))
            .expect("cannot create a link");
        assert!(has_data(&profile), "Default as a dangling link");
        std::fs::remove_file(profile.join("Default")).expect("cannot remove the link");

        // A folder that cannot be searched: the check cannot know, so it counts as data.
        use std::os::unix::fs::PermissionsExt;
        let locked = std::fs::Permissions::from_mode(0o000);
        std::fs::set_permissions(&profile, locked).expect("cannot lock the profile");
        let result = has_data(&profile);
        let open = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&profile, open).expect("cannot unlock the profile");
        assert!(result, "a profile folder that cannot be searched");
    }

    /// A refused start makes no keychain call and does not start, whatever `main` did before.
    #[test]
    fn refused_never_starts() {
        let nowhere = Path::new("/nonexistent/ass-test");
        assert!(!gate(Keychain::Refused, nowhere, nowhere));
    }

    #[test]
    fn data_in_either_profile() {
        let scratch = Scratch::new("either");
        let (fresh, default) = (scratch.0.join("fresh"), scratch.0.join("default"));
        assert!(!any_data(&fresh, &default), "neither exists");
        assert!(!any_data(&fresh, &fresh), "the default profile itself, new");
        std::fs::create_dir_all(default.join("Default")).expect("cannot create Default");
        assert!(
            any_data(&fresh, &default),
            "a new profile beside a default one with data"
        );
        assert!(any_data(&default, &fresh), "a profile with data");
        assert!(
            any_data(&default, &default),
            "the default profile itself, with data"
        );
    }

    /// The tests' one contact with the real keychain: the silent check of an item that cannot
    /// exist (a random service name) finds nothing, can raise nothing, and leaves the setting as
    /// it was. Then, without the keychain, the setting is restored when a panic leaves the guard.
    /// One test, because the setting is shared by the whole test process.
    #[test]
    fn silent_check_of_a_missing_item() {
        use std::hash::{BuildHasher, RandomState};
        let random = RandomState::new().hash_one(std::process::id());
        let service = format!("dev.ass.test.no-such-item.{random:016x}");

        let before = interaction_allowed().expect("cannot read the setting");
        assert_ne!(before, 0, "the keychain may show dialogs by default");
        assert_eq!(check(&service, "test"), Some(ERR_SEC_ITEM_NOT_FOUND));
        assert_eq!(interaction_allowed(), Ok(before));

        let panicked = std::panic::catch_unwind(|| {
            let _no_dialogs = NoDialogs::new().expect("cannot turn the dialogs off");
            assert_eq!(interaction_allowed(), Ok(0));
            panic!("a panic while the dialogs are off");
        });
        assert!(panicked.is_err());
        assert_eq!(interaction_allowed(), Ok(before));
    }
}
