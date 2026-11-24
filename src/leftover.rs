//! At start, before Chromium initializes: ends the helper processes that an earlier run of this
//! profile left running (known issue 2, `docs/ARCHITECTURE.md`).
//!
//! When Chromium's teardown watchdog ends the browser process (exit code 2), the network
//! service helper can stay alive with `launchd` as its parent and the profile's cookie file
//! open, and could still write into the profile after this start has begun. Crashes of the
//! browser process leave helpers the same way.
//!
//! This is housekeeping, not a barrier. The match trusts what each process says about itself
//! (the name of its executable and its arguments), so a process of this user can only get
//! itself ended.
//!
//! # What is ended
//!
//! With SIGKILL ([`is_leftover`]): a process of this user, real and effective, whose parent is
//! `launchd` (PID 1), whose executable's file name starts with `Ass Helper`, whose arguments
//! carry `--type=`, and whose last `--user-data-dir=` is, byte for byte, one of the two texts
//! that name this profile ([`Names`]). Nothing on disk is looked at for a path that another
//! process names: a folder on a volume that has stopped answering would hold up the start.
//!
//! The two texts are the profile path as handed to CEF, without trailing slashes, and its real
//! path. The real path is resolved once, and only for a process that matched on everything
//! else. What a helper carries, read at the pin:
//!
//! - CEF hands every helper `chrome::DIR_USER_DATA` as `--user-data-dir`
//!   (`libcef/browser/chrome/chrome_content_browser_client_cef.cc:231-236`).
//! - That is the root cache path (`libcef/common/resource_util.cc:72-81`, `:126-130`, called
//!   from `libcef/common/chrome/chrome_main_delegate_cef.cc:458`), set with
//!   `PathService::Override`, which creates the folder when it is missing and then resolves it
//!   with `realpath` (`base/path_service.cc:269-299`).
//! - Before that, CEF strips trailing slashes from the root cache path and resolves it when the
//!   folder exists, and keeps the text when it does not exist yet
//!   (`libcef/browser/context.cc:102-133`). The override then resolves that text too.
//!
//! So a helper carries the real path, as long as the folder can be created: CEF does not look
//! at what `Override` returns (`resource_util.cc:130`). The given text matches when it already
//! is the real path, and then nothing needs resolving. Seen: a profile passed as `/var/…`
//! reaches its helpers as `/private/var/…` (`scripts/quit-stall-test.sh`). Not matched: a
//! helper of this profile whose real path has changed since it started, because the folder was
//! moved or a link in the path now points elsewhere.
//!
//! One helper has `launchd` as its parent while its browser process lives: Crashpad's handler.
//! It carries `--database=` with the profile and no `--user-data-dir`, and it is off in our
//! bundle (no `crash_reporter.cfg`). So the match is never widened to another switch that names
//! the profile.
//!
//! Why a running instance's helpers are never touched: Chromium starts every helper itself, so
//! while that instance's browser process lives, it is their parent, not `launchd`. A second
//! launch runs this before `initialize` hands it to the first instance, and finds none of them.
//!
//! What it misses: a start inside the 10 s of a dying instance's watchdog, when the profile
//! lock is already released and that instance's helpers are still children of its living
//! browser process; and the helper stays until the next start of this profile.
//!
//! What it ends that is not this folder's: when the profile folder was moved away and a new
//! one made at the same path between runs, an orphan of the old folder still names that path,
//! and is ended.
//!
//! # SIGKILL, and what it costs
//!
//! No signal makes a helper commit. Every Chromium child process sets SIGTERM and SIGHUP back
//! to their default actions at start (`content/app/content_main.cc:99-111`, called at `:272`)
//! and ignores SIGINT (`content/app/content_main_runner_impl.cc:992-1007`). A leftover network
//! helper whose browser process is gone is already ending itself, and would commit its cookie
//! store if left alone; SIGKILL can cut a commit it has under way. That is the cost. It is
//! accepted because no bound on that ending is known (4 s to over a minute in our runs), and
//! a helper running on beside this start shares the profile's disk cache and can write its
//! older network state over this start's. The cookie file itself survives a kill in the middle
//! of a commit: SQLite rolls the commit back from its journal at the next open. Our code never
//! deletes `Cookies-journal`.
//!
//! # The calls
//!
//! `proc_listpids` for every process, `proc_pidinfo` (`PROC_PIDTBSDINFO`) for its users and
//! parent, then, only for an orphan of this user, `proc_pidpath` for its executable and
//! `sysctl` (`KERN_PROCARGS2`) for its arguments; `kill` to end it. When the list cannot be
//! read nothing is ended; a process whose details cannot be read is left alone. The start goes
//! on either way. A process ID could in principle be taken by a new process between the check
//! and the signal; IDs count up to 99999 before they wrap, so that new process would have to
//! appear in that microsecond with the same ID.

use std::cell::OnceCell;
use std::ffi::{OsStr, OsString, c_int, c_void};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

// Signatures from the macOS SDK's headers: `libproc.h:92-102`, `sys/proc_info.h:51` and
// `:59-82` (`PROC_ALL_PIDS`, `struct proc_bsdinfo`), `:723-749` (`PROC_PIDTBSDINFO`,
// `PROC_PIDPATHINFO_MAXSIZE`), `sys/sysctl.h:188-265` (`CTL_KERN`, `KERN_ARGMAX`,
// `KERN_PROCARGS2`). All in libSystem.
unsafe extern "C" {
    fn proc_listpids(kind: u32, typeinfo: u32, buffer: *mut c_void, buffersize: c_int) -> c_int;
    fn proc_pidinfo(pid: c_int, flavor: c_int, arg: u64, buffer: *mut c_void, size: c_int)
    -> c_int;
    fn proc_pidpath(pid: c_int, buffer: *mut c_void, buffersize: u32) -> c_int;
    fn sysctl(
        name: *mut c_int,
        namelen: u32,
        oldp: *mut c_void,
        oldlenp: *mut usize,
        newp: *mut c_void,
        newlen: usize,
    ) -> c_int;
    fn getuid() -> u32;
    fn kill(pid: c_int, signal: c_int) -> c_int;
}

const PROC_ALL_PIDS: u32 = 1;
const PROC_PIDTBSDINFO: c_int = 3;
const PROC_PIDPATHINFO_MAXSIZE: usize = 4 * 1024;
const CTL_KERN: c_int = 1;
const KERN_ARGMAX: c_int = 8;
const KERN_PROCARGS2: c_int = 49;
const SIGKILL: c_int = 9;
const LAUNCHD: i32 = 1;

/// `struct proc_bsdinfo` (`sys/proc_info.h:59-82`).
#[repr(C)]
struct ProcBsdInfo {
    flags: u32,
    status: u32,
    xstatus: u32,
    pid: u32,
    ppid: u32,
    uid: u32,
    gid: u32,
    ruid: u32,
    rgid: u32,
    svuid: u32,
    svgid: u32,
    rfu_1: u32,
    comm: [u8; 16],
    name: [u8; 32],
    nfiles: u32,
    pgid: u32,
    pjobc: u32,
    e_tdev: u32,
    e_tpgid: u32,
    nice: i32,
    start_tvsec: u64,
    start_tvusec: u64,
}
const _: () = assert!(std::mem::size_of::<ProcBsdInfo>() == 136);

/// What [`is_leftover`] looks at in a process.
pub struct Process {
    /// Effective user.
    pub uid: u32,
    /// Real user.
    pub ruid: u32,
    pub ppid: i32,
    /// The file name of its executable.
    pub exe_name: OsString,
    /// Its arguments, the program's own path first.
    pub args: Vec<OsString>,
}

/// The two texts that name this profile in a helper's `--user-data-dir=` (module doc): the
/// path as handed to CEF, and its real path, resolved on first use and once.
pub struct Names {
    given: Vec<u8>,
    path: PathBuf,
    real: OnceCell<Option<Vec<u8>>>,
}

impl Names {
    /// `profile`: the folder CEF is given as its root cache path.
    pub fn new(profile: &Path) -> Self {
        let mut given = profile.as_os_str().as_bytes();
        // As CEF does (`libcef/browser/context.cc:103-107`); a lone `/` stays.
        while given.len() > 1 && given.ends_with(b"/") {
            given = &given[..given.len() - 1];
        }
        Self {
            given: given.to_vec(),
            path: profile.to_path_buf(),
            real: OnceCell::new(),
        }
    }

    /// Whether `value` is one of the two texts, byte for byte. Never an empty value.
    fn name(&self, value: &[u8]) -> bool {
        !value.is_empty() && (value == self.given || self.real() == Some(value))
    }

    /// The profile's real path, resolved on the first call; None when it cannot be resolved.
    /// Only this profile's own path is ever resolved.
    fn real(&self) -> Option<&[u8]> {
        self.real
            .get_or_init(|| {
                std::fs::canonicalize(&self.path)
                    .ok()
                    .map(|real| real.into_os_string().into_vec())
            })
            .as_deref()
    }
}

/// Ends this profile's leftover helpers. `profile`: the folder CEF is given as its root cache
/// path. One line on stderr when at least one was ended.
pub fn end_helpers(profile: &Path) {
    let me = unsafe { getuid() };
    let Some(pids) = all_pids() else {
        return;
    };
    let names = Names::new(profile);
    let mut buffer = Vec::new();
    let mut ended = 0;
    for pid in pids {
        let Some(info) = bsd_info(pid) else {
            continue;
        };
        // The cheap part of the test first; the arguments are read only for an orphan of ours.
        if !orphan_of(info.uid, info.ruid, info.ppid as i32, me) {
            continue;
        }
        let (Some(exe_name), Some(args)) = (exe_name(pid), arguments(pid, &mut buffer)) else {
            continue;
        };
        let process = Process {
            uid: info.uid,
            ruid: info.ruid,
            ppid: info.ppid as i32,
            exe_name,
            args,
        };
        if is_leftover(&process, me, &names) && unsafe { kill(pid, SIGKILL) } == 0 {
            ended += 1;
        }
    }
    if ended > 0 {
        log_line!(
            "Ass: ended {ended} helper process(es) left running by an earlier run of this profile"
        );
    }
}

/// The decision. `me`: this user; `names`: the texts that name this profile.
pub fn is_leftover(process: &Process, me: u32, names: &Names) -> bool {
    orphan_of(process.uid, process.ruid, process.ppid, me) && helper_of(process, names)
}

/// A process of user `me`, real and effective, whose parent is `launchd`.
fn orphan_of(uid: u32, ruid: u32, ppid: i32, me: u32) -> bool {
    uid == me && ruid == me && ppid == LAUNCHD
}

/// One of our helper executables, started by Chromium (`--type=`) for this profile. When a
/// switch is given twice Chromium keeps the last value, so the last `--user-data-dir=` counts.
/// The profile's real path is resolved only once the other three have matched.
fn helper_of(process: &Process, names: &Names) -> bool {
    let switch = |name: &[u8]| -> Option<&OsStr> {
        process
            .args
            .iter()
            .skip(1)
            .rev()
            .find_map(|arg| arg.as_bytes().strip_prefix(name))
            .map(OsStr::from_bytes)
    };
    process.exe_name.as_bytes().starts_with(b"Ass Helper")
        && switch(b"--type=").is_some()
        && switch(b"--user-data-dir=").is_some_and(|dir| names.name(dir.as_bytes()))
}

/// Every process ID, or None when the list cannot be read.
fn all_pids() -> Option<Vec<c_int>> {
    // Called with no buffer it returns the bytes needed now; room is added for processes
    // started in between.
    let needed = unsafe { proc_listpids(PROC_ALL_PIDS, 0, std::ptr::null_mut(), 0) };
    let count = usize::try_from(needed).ok()? / size_of::<c_int>() + 256;
    let mut pids = vec![0 as c_int; count];
    let bytes = c_int::try_from(count * size_of::<c_int>()).ok()?;
    let filled = unsafe { proc_listpids(PROC_ALL_PIDS, 0, pids.as_mut_ptr().cast(), bytes) };
    pids.truncate(usize::try_from(filled).ok()? / size_of::<c_int>());
    pids.retain(|&pid| pid > LAUNCHD);
    Some(pids)
}

fn bsd_info(pid: c_int) -> Option<ProcBsdInfo> {
    let mut info = std::mem::MaybeUninit::<ProcBsdInfo>::zeroed();
    let size = size_of::<ProcBsdInfo>() as c_int;
    let filled = unsafe { proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, info.as_mut_ptr().cast(), size) };
    // All zeros is a valid value of the struct, and the call filled it whole.
    (filled == size).then(|| unsafe { info.assume_init() })
}

/// The file name of the process's executable.
fn exe_name(pid: c_int) -> Option<OsString> {
    let mut path = vec![0u8; PROC_PIDPATHINFO_MAXSIZE];
    let len = unsafe { proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
    path.truncate(usize::try_from(len).ok().filter(|&len| len > 0)?);
    Path::new(OsStr::from_bytes(&path))
        .file_name()
        .map(OsStr::to_os_string)
}

/// The process's arguments, read with `KERN_PROCARGS2`: the argument count, the executable's
/// path, padding, then the arguments, each ending in a zero byte, then the environment.
/// `buffer` is reused between calls.
fn arguments(pid: c_int, buffer: &mut Vec<u8>) -> Option<Vec<OsString>> {
    if buffer.is_empty() {
        let mut argmax: c_int = 0;
        let mut size = size_of::<c_int>();
        let mut name = [CTL_KERN, KERN_ARGMAX];
        let read = unsafe {
            sysctl(
                name.as_mut_ptr(),
                2,
                (&raw mut argmax).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        buffer.resize(usize::try_from(argmax).ok().filter(|_| read == 0)?, 0);
    }
    let mut size = buffer.len();
    let mut name = [CTL_KERN, KERN_PROCARGS2, pid];
    let read = unsafe {
        sysctl(
            name.as_mut_ptr(),
            3,
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if read != 0 {
        return None;
    }
    parse_procargs(buffer.get(..size)?)
}

/// Splits what `KERN_PROCARGS2` returns into the arguments. None when it is cut short.
fn parse_procargs(data: &[u8]) -> Option<Vec<OsString>> {
    let argc = usize::try_from(c_int::from_ne_bytes(data.get(..4)?.try_into().ok()?)).ok()?;
    let rest = &data[4..];
    // The executable's path, then the zero bytes that pad it.
    let path_end = rest.iter().position(|&b| b == 0)?;
    let mut rest = &rest[path_end..];
    while let [0, tail @ ..] = rest {
        rest = tail;
    }
    // Each argument takes at least its zero byte, so no more than that many fit.
    let mut args = Vec::with_capacity(argc.min(rest.len()));
    for _ in 0..argc {
        let end = rest.iter().position(|&b| b == 0)?;
        args.push(OsStr::from_bytes(&rest[..end]).to_os_string());
        rest = &rest[end + 1..];
    }
    Some(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const ME: u32 = 501;
    const PROFILE: &str = "/Users/someone/Library/Application Support/Ass";

    impl Names {
        /// Both texts given, so that these tests touch no folder.
        fn of(given: &str, real: Option<&str>) -> Self {
            let names = Names::new(Path::new(given));
            let _ = names.real.set(real.map(|real| real.as_bytes().to_vec()));
            names
        }
    }

    fn helper(ppid: i32, exe: &str, args: &[&str]) -> Process {
        Process {
            uid: ME,
            ruid: ME,
            ppid,
            exe_name: exe.into(),
            args: std::iter::once("/Applications/Ass.app/Contents/Frameworks/x")
                .chain(args.iter().copied())
                .map(OsString::from)
                .collect(),
        }
    }

    fn network_helper(ppid: i32, profile: &str) -> Process {
        let dir = format!("--user-data-dir={profile}");
        helper(
            ppid,
            "Ass Helper",
            &[
                "--type=utility",
                "--utility-sub-type=network.mojom.NetworkService",
                "--lang=en-US",
                &dir,
                "--shared-files",
            ],
        )
    }

    fn leftover(process: &Process) -> bool {
        is_leftover(process, ME, &Names::of(PROFILE, None))
    }

    #[test]
    fn ends_this_profiles_orphaned_helpers() {
        assert!(leftover(&network_helper(LAUNCHD, PROFILE)));
        let renderer = helper(
            LAUNCHD,
            "Ass Helper (Renderer)",
            &["--type=renderer", &format!("--user-data-dir={PROFILE}")],
        );
        assert!(leftover(&renderer));
    }

    #[test]
    fn leaves_the_helpers_of_a_running_instance() {
        // Their parent is that instance's browser process.
        assert!(!leftover(&network_helper(4242, PROFILE)));
    }

    #[test]
    fn leaves_another_profiles_helpers() {
        assert!(!leftover(&network_helper(LAUNCHD, "/tmp/other-profile")));
    }

    #[test]
    fn leaves_a_folder_whose_name_only_starts_the_same() {
        assert!(!leftover(&network_helper(LAUNCHD, &format!("{PROFILE} 2"))));
        assert!(!leftover(&network_helper(
            LAUNCHD,
            &format!("{PROFILE}/Default")
        )));
        assert!(!leftover(&network_helper(
            LAUNCHD,
            "/Users/someone/Library/Application Support/As"
        )));
    }

    #[test]
    fn leaves_a_helper_of_another_app() {
        let mut other = network_helper(LAUNCHD, PROFILE);
        other.exe_name = "Google Chrome Helper".into();
        assert!(!leftover(&other));
        other.exe_name = "Ass".into();
        assert!(!leftover(&other));
    }

    #[test]
    fn leaves_chromiums_clone_cleanup_helper() {
        // Started with the helper executable, outlives the browser on purpose, and names no
        // profile (`code_sign_clone_manager.mm:348-377`).
        let cleanup = helper(
            LAUNCHD,
            "Ass Helper",
            &[
                "--type=code-sign-clone-cleanup",
                "--unique-temp-dir-suffix=abc123",
            ],
        );
        assert!(!leftover(&cleanup));
    }

    #[test]
    fn leaves_a_process_of_another_user() {
        let mut other = network_helper(LAUNCHD, PROFILE);
        other.uid = 502;
        other.ruid = 502;
        assert!(!leftover(&other));
        other.uid = ME;
        assert!(!leftover(&other), "real user differs");
        other.uid = 502;
        other.ruid = ME;
        assert!(!leftover(&other), "effective user differs");
    }

    #[test]
    fn needs_both_switches() {
        let no_type = helper(
            LAUNCHD,
            "Ass Helper",
            &[&format!("--user-data-dir={PROFILE}")],
        );
        assert!(!leftover(&no_type));
        let empty_dir = helper(
            LAUNCHD,
            "Ass Helper",
            &["--type=utility", "--user-data-dir="],
        );
        assert!(!is_leftover(&empty_dir, ME, &Names::of("", Some(""))));
        // A switch is not looked for in the program's own path.
        let mut in_path = helper(LAUNCHD, "Ass Helper", &["--type=utility"]);
        in_path.args[0] = format!("--user-data-dir={PROFILE}").into();
        assert!(!leftover(&in_path));
    }

    #[test]
    fn the_last_user_data_dir_counts() {
        let first_ours = helper(
            LAUNCHD,
            "Ass Helper",
            &[
                "--type=utility",
                &format!("--user-data-dir={PROFILE}"),
                "--user-data-dir=/tmp/other-profile",
            ],
        );
        assert!(!leftover(&first_ours));
    }

    /// A folder of its own under the temporary folder, removed when dropped.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("ass-leftover-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The match by text (module doc): the given path or the real one, byte for byte.
    #[test]
    fn matches_the_given_or_the_real_path_by_text() {
        let names = Names::of("/var/t/p", Some("/private/var/t/p"));
        let with_dir = |dir: &str| is_leftover(&network_helper(LAUNCHD, dir), ME, &names);
        assert!(with_dir("/var/t/p"), "the given path");
        assert!(with_dir("/private/var/t/p"), "the real path");
        assert!(!with_dir("/var/t/q"), "neither");
        assert!(!with_dir("/var/t/p2"), "the given path as a prefix");
        assert!(!with_dir("/private/var/t/p2"), "the real path as a prefix");
        assert!(!with_dir("/var/t"), "a parent");
        assert!(!with_dir("/var/t/p/"), "not the same text");
        assert!(!with_dir(""), "an empty value");
        // The given path's trailing slashes are dropped, as CEF drops them.
        let slashes = Names::of("/var/t/p//", None);
        assert!(slashes.name(b"/var/t/p"));
        assert!(!slashes.name(b"/var/t/p//"));
        // A profile whose real path cannot be resolved: only the given text.
        let unresolved = Names::of("/var/t/p", None);
        assert!(unresolved.name(b"/var/t/p"));
        assert!(!unresolved.name(b"/private/var/t/p"));
        assert!(!unresolved.name(b""));
    }

    #[test]
    fn the_last_of_two_user_data_dirs_wins() {
        let names = Names::of(PROFILE, Some("/private/real"));
        let two = |first: &str, last: &str| {
            let process = helper(
                LAUNCHD,
                "Ass Helper",
                &[
                    "--type=utility",
                    &format!("--user-data-dir={first}"),
                    &format!("--user-data-dir={last}"),
                ],
            );
            is_leftover(&process, ME, &names)
        };
        assert!(two("/tmp/other", PROFILE));
        assert!(two("/tmp/other", "/private/real"));
        assert!(!two(PROFILE, "/tmp/other"));
        assert!(!two(PROFILE, ""));
    }

    /// The real path is resolved only for a process that matched on user, parent, name and
    /// `--type=`; never for any other.
    #[test]
    fn resolves_only_after_everything_else_matched() {
        let unresolved = || Names::new(Path::new("/var/t/p"));
        let not_ours = [
            network_helper(4242, "/x"),
            {
                let mut other = network_helper(LAUNCHD, "/x");
                other.exe_name = "Google Chrome Helper".into();
                other
            },
            helper(LAUNCHD, "Ass Helper", &["--user-data-dir=/x"]),
            {
                let mut other = network_helper(LAUNCHD, "/x");
                other.uid = 502;
                other
            },
        ];
        for process in &not_ours {
            let names = unresolved();
            assert!(!is_leftover(process, ME, &names));
            assert!(names.real.get().is_none(), "resolved for a non-candidate");
        }
        // The given text matches: nothing to resolve.
        let names = unresolved();
        assert!(is_leftover(
            &network_helper(LAUNCHD, "/var/t/p"),
            ME,
            &names
        ));
        assert!(names.real.get().is_none());
        // Any other text from a candidate: resolved, once.
        let names = unresolved();
        assert!(!is_leftover(&network_helper(LAUNCHD, "/x"), ME, &names));
        assert!(names.real.get().is_some());
    }

    /// The real path, on a real folder reached through a link (and, on macOS, through `/var`).
    #[test]
    fn resolves_this_profiles_real_path() {
        let root = TempDir::new("real");
        let profile = root.0.join("profile");
        std::fs::create_dir(&profile).unwrap();
        let link = root.0.join("link");
        std::os::unix::fs::symlink(&profile, &link).unwrap();
        let real = std::fs::canonicalize(&profile).unwrap();
        let names = Names::new(&link);
        assert!(names.name(link.as_os_str().as_bytes()));
        assert!(names.name(real.as_os_str().as_bytes()));
        assert!(!names.name(root.0.join("profile-2").as_os_str().as_bytes()));
        // A profile that does not exist: only the given text.
        let missing = Names::new(&root.0.join("missing"));
        assert!(!missing.name(real.as_os_str().as_bytes()));
        assert!(missing.name(root.0.join("missing").as_os_str().as_bytes()));
    }

    #[test]
    fn parses_procargs() {
        let mut data = 3i32.to_ne_bytes().to_vec();
        data.extend_from_slice(b"/path/Ass Helper\0\0\0\0/path/Ass Helper\0--type=utility\0--user-data-dir=/a b\0HOME=/x\0");
        let args = parse_procargs(&data).unwrap();
        assert_eq!(
            args,
            ["/path/Ass Helper", "--type=utility", "--user-data-dir=/a b"]
        );
        // Cut short: the last argument has no end.
        assert!(parse_procargs(&data[..data.len() - 30]).is_none());
        assert!(parse_procargs(&[1, 0]).is_none());
    }

    /// A count far beyond what the data can hold reserves no more than the data's length, and
    /// gives None.
    #[test]
    fn parses_procargs_with_a_huge_count() {
        let mut data = i32::MAX.to_ne_bytes().to_vec();
        data.extend_from_slice(b"/path/Ass Helper\0\0--type=utility\0");
        assert!(parse_procargs(&data).is_none());
        let mut data = (-1i32).to_ne_bytes().to_vec();
        data.extend_from_slice(b"/path/Ass Helper\0\0--type=utility\0");
        assert!(parse_procargs(&data).is_none());
    }

    #[test]
    fn reads_this_process() {
        // The calls themselves, on the test's own process: its parent is not launchd, so it is
        // not a leftover, but its details must read.
        let pid = std::process::id() as c_int;
        assert!(all_pids().unwrap().contains(&pid));
        let info = bsd_info(pid).unwrap();
        assert_eq!(info.pid as c_int, pid);
        assert_eq!(info.uid, unsafe { getuid() });
        assert!(exe_name(pid).is_some());
        let args = arguments(pid, &mut Vec::new()).unwrap();
        assert!(!args.is_empty());
    }
}
