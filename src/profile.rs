//! Which folder is the profile, and what is asked about it before anything is opened: whether
//! it may be used at all, and whether it is the default profile folder
//! (`docs/ARCHITECTURE.md`, "Data locations").
//!
//! One rule holds on every start: the profile folder exists, and is a folder, before Chromium
//! is started. Given a profile path it cannot make into a folder (a file, a path below a
//! file, a place it may not write), CEF does not stop. `PathService::Override` fails, CEF does
//! not look at the result (`libcef/common/resource_util.cc:126-130`), and the profile is then
//! the folder Chrome picks for itself, `~/Library/Application Support/Chromium`
//! (`chrome_paths_mac.mm`): another browser's profile, opened with our key. Read at the pinned
//! sources, never run. So nothing here leaves the folder for Chromium to make.

use std::ffi::{CStr, OsStr, c_char, c_int, c_long};
use std::fs::Metadata;
use std::io::ErrorKind;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

/// The default profile folder, under a home directory.
const UNDER_HOME: &str = "Library/Application Support/Ass";

/// Where the profile (cookies, cache, history) lives.
/// `ASS_PROFILE_DIR` overrides it, for throwaway test profiles; empty counts as not set.
pub fn dir() -> PathBuf {
    given().unwrap_or_else(default_dir)
}

/// `ASS_PROFILE_DIR`, when it is set and not empty.
fn given() -> Option<PathBuf> {
    std::env::var_os("ASS_PROFILE_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
}

/// Whether `ASS_PROFILE_DIR` names the profile folder.
pub fn is_given() -> bool {
    given().is_some()
}

/// The profile folder used when `ASS_PROFILE_DIR` is not set: under `HOME`.
pub fn default_dir() -> PathBuf {
    home().join(UNDER_HOME)
}

/// The home directory the default profile folder lies in: `HOME`.
fn home() -> PathBuf {
    std::env::home_dir().expect("no home directory")
}

/// Why the profile folder may not be used, or None. Asked first, before anything is opened,
/// and writes nothing.
///
/// Not an absolute path: CEF takes only an absolute profile path, and given any other it uses
/// a folder of its own, or `--user-data-dir`. Not a folder, or not readable: see the module
/// doc. A folder named in `ASS_PROFILE_DIR` must already exist: a name that is mistyped is not
/// made into a profile, and a path with `..` through a folder that is missing cannot name one
/// folder to the kernel and, once its parents have been made by the path's text, another.
/// The default folder may be missing: [`ready`] makes it, once the keychain gate has passed.
/// The home directory it is made in may not be missing, for the same reason: with every part
/// of `HOME` there, the kernel and the text agree, and what is left to make (`Library`,
/// `Application Support`, `Ass`) has no `..` in it.
pub fn refusal() -> Option<&'static str> {
    refusal_of(given().as_deref(), &home())
}

/// [`refusal`] with its inputs given: `ASS_PROFILE_DIR` and the home directory.
fn refusal_of(given: Option<&Path>, home: &Path) -> Option<&'static str> {
    let default = home.join(UNDER_HOME);
    let dir = given.unwrap_or(&default);
    if !dir.is_absolute() {
        return Some("not an absolute path");
    }
    match std::fs::metadata(dir) {
        Ok(found) if found.is_dir() => None,
        Ok(_) => Some("not a folder"),
        Err(error) if error.kind() == ErrorKind::NotFound => match given {
            Some(_) => Some("the folder does not exist"),
            None if is_folder(home) => None,
            None => Some("the home directory is not there"),
        },
        Err(_) => Some("the folder cannot be read"),
    }
}

/// Whether a folder is at `path`.
fn is_folder(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|found| found.is_dir())
}

/// Makes the default profile folder if this start uses it and it is not there yet, then says
/// whether the profile folder is there, as a folder whose real path can be found: the two
/// things CEF's override of the profile path needs (`base/path_service.cc`,
/// `OverrideAndCreateIfNeeded`). Asked last, after the keychain gate and right before Chromium
/// is started; false ends the start. A folder named in `ASS_PROFILE_DIR` is never made here. The folder is made for its owner only, as Chromium
/// makes it (`base/files/file_util_posix.cc`, `CreateDirectoryAndGetError`: `S_IRWXU`).
pub fn ready() -> bool {
    ready_of(given().as_deref(), &default_dir())
}

/// [`ready`] with its inputs given: `ASS_PROFILE_DIR` and the default folder.
fn ready_of(given: Option<&Path>, default: &Path) -> bool {
    if given.is_none() {
        // A failure shows in the look below.
        let _ = std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(default);
    }
    let dir = given.unwrap_or(default);
    is_folder(dir) && std::fs::canonicalize(dir).is_ok()
}

/// Whether this start's profile is the default profile folder, or cannot be told apart from
/// it. Every test start asks this, and is refused on true: `main` for `--use-mock-keychain`,
/// `test_window.rs` for test window mode.
///
/// Folders are compared by device and inode only. [`refusal`] has passed by then, so a folder
/// named in `ASS_PROFILE_DIR` exists. Compared as text, a folder that does not exist yet would
/// pass under any other spelling of the default one (a link in a parent,
/// `/System/Volumes/Data/…`, other letter case, a leading `//`).
///
/// The default folder is looked for in two places: under `HOME`, and under the account's
/// home directory as the system's user database has it, because `HOME` can point anywhere.
/// A place where it is not found is not it; any other error there, or a user database that
/// cannot be read, counts as the default folder, so that the start is refused. Not seen as the
/// default: a folder inside the default one, or one that holds it.
pub fn is_default() -> bool {
    is_default_of(
        given().as_deref(),
        &default_dir(),
        account_home().as_deref(),
    )
}

/// [`is_default`] with its inputs given: `ASS_PROFILE_DIR`, the default folder under `HOME`,
/// and the account's home directory (None: not readable).
fn is_default_of(given: Option<&Path>, under_home_env: &Path, account_home: Option<&Path>) -> bool {
    let Some(given) = given else {
        return true;
    };
    let Ok(folder) = std::fs::metadata(given) else {
        return true;
    };
    let Some(account_home) = account_home else {
        return true;
    };
    is_that_folder(&folder, under_home_env)
        || is_that_folder(&folder, &account_home.join(UNDER_HOME))
}

/// Whether `folder` is the folder at `place`, by device and inode. Nothing found there: not
/// it. Any other error: counted as it.
fn is_that_folder(folder: &Metadata, place: &Path) -> bool {
    match std::fs::metadata(place) {
        Ok(there) => there.dev() == folder.dev() && there.ino() == folder.ino(),
        Err(error) => error.kind() != ErrorKind::NotFound,
    }
}

// From the macOS SDK's headers: `pwd.h:84-95` (`struct passwd`) and `:102` (`getpwuid_r`),
// `sys/errno.h:126` (`ERANGE`). In libSystem.
unsafe extern "C" {
    fn getuid() -> u32;
    fn getpwuid_r(
        uid: u32,
        entry: *mut Passwd,
        buffer: *mut c_char,
        size: usize,
        result: *mut *mut Passwd,
    ) -> c_int;
}

const ERANGE: c_int = 34;

/// `struct passwd` (`pwd.h:84-95`); `__darwin_time_t` is a `long`.
#[repr(C)]
struct Passwd {
    name: *mut c_char,
    passwd: *mut c_char,
    uid: u32,
    gid: u32,
    change: c_long,
    class: *mut c_char,
    gecos: *mut c_char,
    dir: *mut c_char,
    shell: *mut c_char,
    expire: c_long,
}

/// The real user's home directory, from the system's user database (`getpwuid_r`), whatever
/// `HOME` says. None when it cannot be read or is not an absolute path.
fn account_home() -> Option<PathBuf> {
    let mut entry = std::mem::MaybeUninit::<Passwd>::zeroed();
    let mut result: *mut Passwd = std::ptr::null_mut();
    let mut buffer = vec![0 as c_char; 4096];
    loop {
        // SAFETY: the entry and the buffer outlive the call, and the size is the buffer's.
        let status = unsafe {
            getpwuid_r(
                getuid(),
                entry.as_mut_ptr(),
                buffer.as_mut_ptr(),
                buffer.len(),
                &mut result,
            )
        };
        if status == ERANGE && buffer.len() < 1 << 20 {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if status != 0 || result.is_null() {
            return None;
        }
        break;
    }
    // SAFETY: `result` points to `entry`, filled by the call; its strings point into `buffer`,
    // which is still alive, and end in a zero byte.
    let home = unsafe {
        let dir = (*result).dir;
        if dir.is_null() {
            return None;
        }
        PathBuf::from(OsStr::from_bytes(CStr::from_ptr(dir).to_bytes()))
    };
    home.is_absolute().then_some(home)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A new, empty folder for one test, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("ass-profile-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("cannot create the test folder");
            Scratch(dir)
        }

        fn folder(&self, name: &str) -> PathBuf {
            let dir = self.0.join(name);
            std::fs::create_dir_all(&dir).expect("cannot create a folder");
            dir
        }

        fn file(&self, name: &str) -> PathBuf {
            let file = self.0.join(name);
            std::fs::write(&file, b"").expect("cannot create a file");
            file
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const MISSING: Option<&str> = Some("the folder does not exist");
    const NOT_A_FOLDER: Option<&str> = Some("not a folder");

    #[test]
    fn refuses_a_relative_path() {
        let home = Path::new("/nowhere/home");
        let refused = Some("not an absolute path");
        assert_eq!(refusal_of(Some(Path::new("profile")), home), refused);
        assert_eq!(refusal_of(Some(Path::new("./profile")), home), refused);
        assert_eq!(refusal_of(None, Path::new("relative/home")), refused);
    }

    #[test]
    fn a_folder_that_exists_is_not_refused() {
        let scratch = Scratch::new("exists");
        let profile = scratch.folder("profile");
        let home = scratch.folder("home");
        assert_eq!(refusal_of(Some(&profile), &home), None);
        // Through a link to it, too.
        let link = scratch.0.join("link");
        std::os::unix::fs::symlink(&profile, &link).expect("cannot create a link");
        assert_eq!(refusal_of(Some(&link), &home), None);
        // A named folder does not need the home directory.
        assert_eq!(refusal_of(Some(&profile), &scratch.0.join("no-home")), None);
        // The default folder itself, not there yet and there.
        assert_eq!(refusal_of(None, &home), None);
        scratch.folder(&format!("home/{UNDER_HOME}"));
        assert_eq!(refusal_of(None, &home), None);
    }

    /// The default folder is made by us, in a home directory that is there. A `HOME` with `..`
    /// through a folder that is missing names nothing to the kernel, and making its parents
    /// by the path's text would end in another home's profile.
    #[test]
    fn a_missing_home_is_refused() {
        let scratch = Scratch::new("no-home");
        let refused = Some("the home directory is not there");
        scratch.folder(&format!("home/{UNDER_HOME}/Default"));
        let spelled = scratch.0.join("absent").join("..").join("home");
        assert_eq!(refusal_of(None, &spelled), refused);
        assert!(!scratch.0.join("absent").exists());
        assert_eq!(refusal_of(None, &scratch.0.join("absent")), refused);
        // A file where the home directory should be: refused too, as a path below a file.
        assert_eq!(
            refusal_of(None, &scratch.file("file")),
            Some("the folder cannot be read")
        );
    }

    #[test]
    fn a_named_folder_that_is_missing_is_refused() {
        let scratch = Scratch::new("missing");
        let home = scratch.folder("home");
        let missing = scratch.0.join("profile");
        assert_eq!(refusal_of(Some(&missing), &home), MISSING);
        // A dangling link: nothing there.
        let dangling = scratch.0.join("dangling");
        std::os::unix::fs::symlink(&missing, &dangling).expect("cannot create a link");
        assert_eq!(refusal_of(Some(&dangling), &home), MISSING);
        assert!(!missing.exists(), "the refusal made the folder");
    }

    /// `..` through a folder that is missing: the kernel finds nothing there, while Chromium,
    /// which makes the parents by the path's text, would end in the folder beside it.
    #[test]
    fn dot_dot_through_a_missing_folder_is_refused() {
        let scratch = Scratch::new("dotdot");
        let home = scratch.folder("home");
        scratch.folder("with-data/Default");
        let spelled = scratch.0.join("absent").join("..").join("with-data");
        assert_eq!(refusal_of(Some(&spelled), &home), MISSING);
        assert!(!scratch.0.join("absent").exists());
    }

    /// What CEF could not make into a folder, and would leave for a folder of Chrome's own.
    #[test]
    fn what_is_not_a_folder_is_refused() {
        let scratch = Scratch::new("file");
        let home = scratch.folder("home");
        let file = scratch.file("file");
        assert_eq!(refusal_of(Some(&file), &home), NOT_A_FOLDER);
        let link = scratch.0.join("link");
        std::os::unix::fs::symlink(&file, &link).expect("cannot create a link");
        assert_eq!(refusal_of(Some(&link), &home), NOT_A_FOLDER);
        assert_eq!(
            refusal_of(Some(Path::new("/dev/null")), &home),
            NOT_A_FOLDER
        );
        // A path below a file: the kernel says "not a folder", which is not "not found".
        assert_eq!(
            refusal_of(Some(&file.join("profile")), &home),
            Some("the folder cannot be read")
        );
        // The default path, taken by a file.
        scratch.folder("home/Library/Application Support");
        scratch.file(&format!("home/{UNDER_HOME}"));
        assert_eq!(refusal_of(None, &home), NOT_A_FOLDER);
    }

    #[test]
    fn ready_makes_only_the_default_folder() {
        let scratch = Scratch::new("ready");
        let default = scratch.0.join("home").join(UNDER_HOME);
        assert!(ready_of(None, &default));
        assert!(default.is_dir());
        let mode = std::fs::metadata(&default).expect("no folder").mode();
        assert_eq!(mode & 0o777, 0o700, "made for its owner only");
        // A folder named in `ASS_PROFILE_DIR` is never made.
        let named = scratch.0.join("named");
        assert!(!ready_of(Some(&named), &default));
        assert!(!named.exists());
        let named = scratch.folder("named");
        assert!(ready_of(Some(&named), &default));
        // A default folder that cannot be made, or is not a folder: not ready.
        let file = scratch.file("file");
        assert!(!ready_of(None, &file.join(UNDER_HOME)));
        assert!(!ready_of(None, &file));
        assert!(!ready_of(Some(&file), &default));
        // A dangling link at the default path: not ready, and what it points to is not made.
        let target = scratch.0.join("target");
        let dangling = scratch.0.join("dangling");
        std::os::unix::fs::symlink(&target, &dangling).expect("cannot create a link");
        assert!(!ready_of(None, &dangling));
        assert!(!target.exists());
    }

    #[test]
    fn not_set_is_the_default() {
        let scratch = Scratch::new("not-set");
        let account = scratch.folder("account");
        let env_default = scratch.0.join("elsewhere").join(UNDER_HOME);
        assert!(is_default_of(None, &env_default, Some(&account)));
    }

    /// Another spelling of the default folder while it does not exist yet: `refusal` stops it,
    /// because the folder is missing, and `is_default` cannot tell it apart either. As text it
    /// would have passed as another folder.
    #[test]
    fn another_spelling_of_a_missing_default_is_refused() {
        let scratch = Scratch::new("spellings");
        let account = scratch.folder("account");
        let env_default = scratch.0.join("elsewhere").join(UNDER_HOME);
        let link = scratch.0.join("link");
        std::os::unix::fs::symlink(&account, &link).expect("cannot create a link");
        let through_link = link.join(UNDER_HOME);
        let double_slash = PathBuf::from(format!("/{}", account.join(UNDER_HOME).display()));
        let other_case = PathBuf::from(
            account
                .join(UNDER_HOME)
                .to_str()
                .expect("not UTF-8")
                .to_uppercase(),
        );
        for spelling in [&through_link, &double_slash, &other_case] {
            assert_eq!(
                refusal_of(Some(spelling), &account),
                MISSING,
                "{}",
                spelling.display()
            );
            assert!(
                is_default_of(Some(spelling), &env_default, Some(&account)),
                "{}",
                spelling.display()
            );
        }
    }

    #[test]
    fn an_existing_folder_with_both_defaults_missing_is_another_folder() {
        let scratch = Scratch::new("usable");
        let account = scratch.folder("account");
        let env_default = scratch.0.join("elsewhere").join(UNDER_HOME);
        let profile = scratch.folder("profile");
        assert!(!is_default_of(Some(&profile), &env_default, Some(&account)));
        // The parent of a default folder, and a folder inside one, are other folders.
        let default = scratch.folder(&format!("account/{UNDER_HOME}"));
        let inside = scratch.folder(&format!("account/{UNDER_HOME}/Default"));
        for other in [&account, &inside, &profile] {
            assert!(!is_default_of(Some(other), &env_default, Some(&account)));
        }
        assert!(is_default_of(Some(&default), &env_default, Some(&account)));
    }

    /// The default folder that exists, under either home, by any spelling that reaches it.
    #[test]
    fn the_existing_default_by_any_spelling() {
        let scratch = Scratch::new("default");
        let account = scratch.folder("account");
        let real_default = scratch.folder(&format!("account/{UNDER_HOME}"));
        let elsewhere = scratch.folder("elsewhere");
        let env_default = elsewhere.join(UNDER_HOME);
        let link = scratch.0.join("link");
        std::os::unix::fs::symlink(&account, &link).expect("cannot create a link");
        let spellings = [
            real_default.clone(),
            link.join(UNDER_HOME),
            PathBuf::from(format!("{}/", real_default.display())),
            PathBuf::from(format!("/{}", real_default.display())),
            real_default.join("."),
            std::fs::canonicalize(&real_default).expect("cannot resolve"),
        ];
        // `HOME` pointed elsewhere: the account's own default folder is still the default.
        for spelling in &spellings {
            assert!(
                is_default_of(Some(spelling), &env_default, Some(&account)),
                "{}",
                spelling.display()
            );
        }
        // The default folder under `HOME`, with the account's own missing.
        let env_real = scratch.folder(&format!("elsewhere/{UNDER_HOME}"));
        let fresh = scratch.folder("fresh");
        assert!(is_default_of(Some(&env_real), &env_real, Some(&fresh)));
    }

    #[test]
    fn an_unreadable_user_database_counts_as_the_default() {
        let scratch = Scratch::new("no-account");
        let profile = scratch.folder("profile");
        let env_default = scratch.0.join("elsewhere").join(UNDER_HOME);
        assert!(is_default_of(Some(&profile), &env_default, None));
    }

    /// A folder that cannot be searched: the profile behind it cannot be read, and a default
    /// place behind it counts as the default.
    #[test]
    fn what_cannot_be_read() {
        let scratch = Scratch::new("locked");
        let account = scratch.folder("account");
        let profile = scratch.folder("profile");
        let env_default = scratch.0.join("elsewhere").join(UNDER_HOME);
        let locked = scratch.folder("locked");
        let behind = scratch.folder("locked/inside");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000))
            .expect("cannot lock the folder");
        let given_behind = refusal_of(Some(&behind), &account);
        let default_behind = refusal_of(None, &behind);
        let same_as_behind = is_default_of(Some(&profile), &behind, Some(&account));
        let ready_behind = ready_of(Some(&behind), &env_default);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700))
            .expect("cannot unlock the folder");
        assert_eq!(given_behind, Some("the folder cannot be read"));
        assert_eq!(default_behind, Some("the folder cannot be read"));
        assert!(same_as_behind);
        assert!(!ready_behind);
    }

    #[test]
    fn reads_the_accounts_home() {
        let home = account_home().expect("the user database gave no home directory");
        assert!(home.is_absolute());
    }
}
