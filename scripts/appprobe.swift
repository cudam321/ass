// Window and focus probe for app tests in test window mode (src/test_window.rs).
// Needs no Accessibility or Screen Recording permission.
//
//   swiftc -O -o appprobe scripts/appprobe.swift
//   appprobe --binary <executable> [--kill] [--forbid <pid>]... [--allow <condition>]... [--self-test]
//   appprobe --replay <probe log>
//
// Watches every process that runs <executable> and has the same parent as the
// probe, so start the probe first and launch the app from the same script. A
// process runs <executable> when its executable file has the same device and
// inode, whatever hard-link name it was started by: the kernel reports a
// process's executable under whichever of those names it looked up last.
// Each sample (every 20 ms for the first 5 s after a process appears, then
// every 50 ms) records the frontmost app, the menu bar owner, each watched
// app's `active` and activation policy, and every window owned by a watched
// process or its children: on-screen flag, alpha, layer, bounds. NSWorkspace
// notifications are logged as they arrive. Apps that are not watched are
// logged as "other", never by name.
//
// In test window mode the app's window is on a display, ordered in, with alpha
// 0. A violation (`violations` below) is a watched app that is frontmost, is
// active, owns the menu bar, or has an on-screen window with alpha above 0; a
// window whose alpha the system does not report counts as alpha 1. An
// activation notification for a watched app is a violation too, even between
// samples. With --kill, the first violation sends SIGKILL to that watched
// process. Only watched processes are ever signalled, and never a --forbid
// PID. --self-test, for testing the probe itself, counts every sample of a
// watched process as a violation.
//
// --allow <condition> turns one condition of the stop rule off for a run:
// frontmost, menu-bar-owner, active, window-visible or activated. An app that
// comes forward meets four of them, so letting it come forward takes
// --allow frontmost, active, activated and menu-bar-owner. Each is still
// counted (allowed-<condition> in the summary), never stops the app, and never
// changes which process may be signalled: the binary's identity, the parent
// rule and --forbid cannot be turned off.
//
// When the probe cannot do its job for a process, it writes one "guard-failed"
// record for it, with the reason, and exits with code 4 at the end: a kill it
// refuses (a --forbid PID, a PID that no longer runs the binary) or that fails,
// and a child of its parent whose executable it cannot identify (never adopted).
// The calling script must then stop the app itself.
//
// --replay applies the same rule to the samples in a probe log and prints one
// line per violation, for testing the rule without launching anything.
//
// Desktops (Spaces), and whether the window could be seen. A window at the
// normal level (layer 0) belongs to one desktop. Test window mode puts the app's
// window above it, at layer 8, and on every desktop (src/test_window.rs). The
// window list gives the layer but not the collection behaviour, so the probe
// goes by the layer: a window above layer 0 is taken to be on every desktop.
// What each record and summary count means, for each kind:
//
//   desktop-changes  The system's notification that the active desktop changed,
//       counted once the app has a window on screen. The same for both kinds. The
//       probe is an application with the policy Prohibited, which it needs to
//       receive it.
//   slide  Nothing in test window mode moves the app's window, so a change of x
//       alone between two samples in which the window is on screen is the system
//       moving it. One "slide" record per slide, when it stops (or when the log or
//       the app ends: "open"), says where the window ended, and its layer.
//       Layer 0: the window going away or coming back with its desktop.
//       desktop-slides counts these slides, and desktop-returns those that ended
//       on screen and on a display ("return": back with its desktop).
//       Above layer 0: the window is never left behind, but while a desktop
//       switch runs the system moves it off the display and back, also for a
//       swipe that is given up and sends no notification. switch-slides counts
//       these; "return" is always false. A pause of one sample within a switch
//       splits it into two slides.
//   i, of, c  In each window sample: its place among the on-screen windows above
//       the desktop's icons and up to its own layer, front to back ("i", 0 is the
//       front one), how many there are ("of"), and whether a window of another
//       process in front of it, with alpha above 0, holds its whole frame ("c").
//       Windows above its own layer are left out. At layer 0 those are the menu
//       bar, the Dock and the like. At layer 8 they also include a window at layer
//       23 of another app, which held the whole window in up to 281 of 959
//       samples while macOS reported the window visible (card 68, trials 3 to 5).
//   hidden-off-screen, hidden-off-display, hidden-covered  Samples in which the
//       main window (the first window of the app of at least 200 by 200 to be on
//       screen) could not be seen outside a desktop switch. A sample in which it
//       is moving is part of a switch and is never counted. off-screen: not
//       ordered in on the active desktop; counted when it is back on screen, or
//       at the end when its last samples off screen span 1 s or more, so a window
//       ordered out to be closed at a quit is not. off-display: on screen, on no
//       display and not moving; counted once it has stood like that for 1 s, so a
//       pause within a switch is not. covered: "c", not moving. firstHiddenAt in
//       the summary is the first such sample. The rule is the same for both
//       kinds. At layer 0 it counts the time the window spent on another desktop;
//       above layer 0 it should stay 0. It is the probe's reading of the window list:
//       macOS's own occlusion state of another process's window cannot be read.
//       In card 68's trials 3 to 5 (a bare program at layer 8 with this
//       behaviour, 14 desktop changes) every span in which macOS reported the
//       program's window occluded began at most one sample before a slide and
//       ended within it, and these counts stay 0 on their logs.
//   pointer-over-window  Samples in which the pointer stood over an on-screen
//       window of the app of at least 200 by 200 (NSEvent.mouseLocation, which
//       needs no permission). Only the count is kept; no position is logged.
//
// --replay prints the slides and these counts too, from the samples as logged
// (it does not recompute "c").
//
// SIGTERM prints a summary line and exits: code 0, or 4 after a guard failure.
// Output: one JSON object per line.
//
// The probe acts only for the script that started it. If that script dies
// (Ctrl+C, a signal, a timeout), the probe is re-parented, usually to launchd,
// and could otherwise adopt and kill a later launch of the same binary from
// Finder, the Dock or `open`. So whenever its parent is not the one it started
// with, or is launchd, it writes one "parent-gone" line and exits with code 3,
// signalling nothing.

import AppKit
import CoreGraphics
import Darwin

/// What the rule needs to know about one window.
struct WindowFacts {
    let onscreen: Bool
    let alpha: Double
}

/// The stop rule. `pids` is the watched process and its children.
func violations(front: pid_t, menu: pid_t, pids: Set<pid_t>, active: Bool,
                windows: [WindowFacts]) -> [String] {
    var found: [String] = []
    if pids.contains(front) { found.append("frontmost") }
    if pids.contains(menu) { found.append("menu-bar-owner") }
    if active { found.append("active") }
    if windows.contains(where: { $0.onscreen && $0.alpha > 0 }) { found.append("window-visible") }
    return found
}

@Sendable func now() -> Double { Date().timeIntervalSince1970 }

func emit(_ object: [String: Any]) {
    var record = object
    if record["t"] == nil { record["t"] = now() }
    guard let data = try? JSONSerialization.data(withJSONObject: record, options: [.sortedKeys]) else { return }
    FileHandle.standardOutput.write(data + Data([0x0A]))
}

/// The conditions that stop the app: all but those turned off with --allow.
func stopping(_ conditions: [String]) -> [String] { conditions.filter { !allowed.contains($0) } }

/// One sample of a window, as the "s" records give it.
struct Frame { let x: Int, y: Int, w: Int, h: Int, on: Bool, display: Bool, layer: Int }
/// A slide in progress: when it began, and from and to which x.
struct Slide { let start: Double, from: Int; var to: Int }
var lastFrames: [Int: Frame] = [:]
var openSlides: [Int: Slide] = [:]
/// The main window (see the header, hidden-*), and while it is off screen, or on no
/// display and not moving, the times of its samples since.
var mainWindow: Int? = nil
var offScreenSince: [Double] = []
var offDisplaySince: [Double] = []
/// How long the main window must stay off screen at the end, or on no display, before
/// it counts; and whether it has stood on no display that long.
let strandedAfter = 1.0
var stranded = false

/// A slide that stopped, or was still going when the log or the app ended ("open"; then
/// where it stood last). At layer 0, a slide that ends on screen and on a display brought
/// the window back with its desktop: "return". Above layer 0 the window is on every
/// desktop and never comes back with one: "return" is false.
func slideRecord(_ number: Int, _ slide: Slide, end: Double, at frame: Frame, open: Bool) -> [String: Any] {
    ["k": "slide", "number": number, "start": slide.start, "t": end, "from": slide.from, "to": slide.to,
     "layer": frame.layer, "onscreen": frame.on, "onDisplay": frame.display, "open": open,
     "return": frame.layer == 0 && frame.on && frame.display]
}

/// What one sample adds: a "slide" record for each slide that stopped, and each sample
/// now known to show a window that could not be seen outside a switch, with why and when.
struct Tracked {
    var slides: [[String: Any]] = []
    var hidden: [(reason: String, t: Double)] = []
}

/// Feeds one sample's windows to the slide detector and the visibility rule (see the
/// header).
func track(_ t: Double, _ windows: [[String: Any]]) -> Tracked {
    var out = Tracked()
    for window in windows {
        guard let number = window["n"] as? Int, let x = window["x"] as? Int, let y = window["y"] as? Int,
              let w = window["w"] as? Int, let h = window["h"] as? Int, let on = window["on"] as? Bool else { continue }
        let frame = Frame(x: x, y: y, w: w, h: h, on: on, display: window["d"] as? Bool ?? false,
                          layer: window["l"] as? Int ?? 0)
        let last = lastFrames[number]
        let moving = last.map { $0.on && on && $0.y == y && $0.w == w && $0.h == h && $0.x != x } ?? false
        if moving, let last {
            if openSlides[number] == nil { openSlides[number] = Slide(start: t, from: last.x, to: x) }
            openSlides[number]?.to = x
        } else if let slide = openSlides.removeValue(forKey: number) {
            out.slides.append(slideRecord(number, slide, end: t, at: frame, open: false))
        }
        lastFrames[number] = frame

        if mainWindow == nil, on, w >= 200, h >= 200 { mainWindow = number }
        guard number == mainWindow else { continue }
        if !on || moving || frame.display {
            offDisplaySince = []
            stranded = false
        }
        if !on {
            offScreenSince.append(t)
            continue
        }
        out.hidden += offScreenSince.map { ("off-screen", $0) }
        offScreenSince = []
        if moving { continue }
        if frame.display {
            if window["c"] as? Bool == true { out.hidden.append(("covered", t)) }
        } else if stranded {
            out.hidden.append(("off-display", t))
        } else {
            offDisplaySince.append(t)
            if t - offDisplaySince[0] >= strandedAfter {
                out.hidden += offDisplaySince.map { ("off-display", $0) }
                offDisplaySince = []
                stranded = true
            }
        }
    }
    return out
}

/// When the log or the app ends: the main window's last samples off screen, if they
/// span at least strandedAfter. A window closed at a quit leaves the list at once or
/// within a sample or two.
func endHidden() -> [(reason: String, t: Double)] {
    defer { offScreenSince = [] }
    guard let first = offScreenSince.first, let last = offScreenSince.last, last - first >= strandedAfter else { return [] }
    return offScreenSince.map { ("off-screen", $0) }
}

/// The slides still in progress when the log or the app ends.
func endOpenSlides(_ t: Double) -> [[String: Any]] {
    defer { openSlides = [:] }
    return openSlides.sorted { $0.key < $1.key }.compactMap { number, slide in
        lastFrames[number].map { slideRecord(number, slide, end: t, at: $0, open: true) }
    }
}

/// The summary's counts, live and in --replay.
var counts: [String: Int] = [:]
var firstHiddenAt: Double? = nil

@Sendable func bump(_ key: String) { counts[key, default: 0] += 1 }

func countSlide(_ slide: [String: Any]) {
    if slide["layer"] as? Int ?? 0 > 0 {
        bump("switch-slides")
    } else {
        bump("desktop-slides")
        if slide["return"] as? Bool == true { bump("desktop-returns") }
    }
    emit(slide)
}

/// Counts what one sample added (see the header) and writes its slide records.
func count(_ tracked: Tracked) {
    for slide in tracked.slides { countSlide(slide) }
    for (reason, t) in tracked.hidden {
        bump("hidden-" + reason)
        firstHiddenAt = min(firstHiddenAt ?? t, t)
    }
}

/// Replays the rule, with any --allow, over a probe log's samples and activation
/// notifications.
func replay(_ path: String) -> Never {
    guard let text = try? String(contentsOfFile: path, encoding: .utf8) else { fatalError("--replay: cannot read") }
    var watchedPids: Set<Int> = []
    var total = 0
    var lastT = 0.0
    for line in text.split(separator: "\n") {
        guard let data = line.data(using: .utf8),
              let record = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let kind = record["k"] as? String else { continue }
        let t = record["t"] as? Double ?? 0
        lastT = max(lastT, t)
        if kind == "watch", let pid = record["pid"] as? Int { watchedPids.insert(pid) }
        if kind == "ws", (record["name"] as? String) == NSWorkspace.didActivateApplicationNotification.rawValue,
           let pid = record["pid"] as? Int, watchedPids.contains(pid) || record["app"] as? String == "ours",
           !stopping(["activated"]).isEmpty {
            total += 1
            emit(["k": "replay", "t": t, "pid": pid, "what": ["activated"]])
        }
        guard kind == "s", let apps = record["apps"] as? [[String: Any]] else { continue }
        let front = pid_t(record["front"] as? Int ?? -1)
        let menu = pid_t(record["menu"] as? Int ?? -1)
        for app in apps {
            let pid = app["pid"] as? Int ?? -1
            let pids = Set(((app["pidList"] as? [Int]) ?? [pid]).map { pid_t($0) })
            count(track(t, (app["windows"] as? [[String: Any]]) ?? []))
            let windows = ((app["windows"] as? [[String: Any]]) ?? []).map {
                WindowFacts(onscreen: $0["on"] as? Bool ?? true, alpha: $0["a"] as? Double ?? 1)
            }
            let found = stopping(violations(front: front, menu: menu, pids: pids,
                                            active: app["active"] as? Bool ?? false, windows: windows))
            if !found.isEmpty {
                total += 1
                emit(["k": "replay", "t": t, "pid": pid, "what": found])
            }
        }
    }
    count(Tracked(slides: endOpenSlides(lastT), hidden: endHidden()))
    emit(["k": "replay-summary", "violations": total, "slides": counts["desktop-slides"] ?? 0,
          "returns": counts["desktop-returns"] ?? 0, "switchSlides": counts["switch-slides"] ?? 0,
          "hidden": ["off-screen": counts["hidden-off-screen"] ?? 0, "off-display": counts["hidden-off-display"] ?? 0,
                     "covered": counts["hidden-covered"] ?? 0],
          "firstHiddenAt": firstHiddenAt ?? -1])
    exit(0)
}

var expectedBinary = ""
var forbidden: Set<pid_t> = []
var killOnViolation = false
var selfTest = false
var allowed: Set<String> = []
let allowable: Set<String> = ["frontmost", "menu-bar-owner", "active", "window-visible", "activated"]
let fastInterval = 0.02, slowInterval = 0.05, fastSeconds = 5.0

var replayPath: String? = nil
var argv = Array(CommandLine.arguments.dropFirst())
while !argv.isEmpty {
    let flag = argv.removeFirst()
    switch flag {
    case "--binary": expectedBinary = argv.removeFirst()
    case "--forbid": forbidden.insert(pid_t(argv.removeFirst())!)
    case "--kill": killOnViolation = true
    case "--self-test": selfTest = true
    case "--allow":
        let condition = argv.removeFirst()
        guard allowable.contains(condition) else { fatalError("--allow: one of \(allowable.sorted())") }
        allowed.insert(condition)
    case "--replay": replayPath = argv.removeFirst()
    default: fatalError("unknown argument \(flag)")
    }
}
if let replayPath { replay(replayPath) }
guard let resolved = realpath(expectedBinary, nil) else { fatalError("--binary: no such file") }
expectedBinary = String(cString: resolved)
free(resolved)
/// The binary's device and inode, read once.
var expectedFile = stat()
guard stat(expectedBinary, &expectedFile) == 0 else { fatalError("--binary: cannot stat") }

/// The process that started the probe, recorded once.
let startParent = getppid()

/// Exits at once, signalling nothing, if the script that started the probe is
/// gone. Called before every adoption, every sample, in every handler and right
/// before every kill.
@Sendable func requireParent() {
    let parent = getppid()
    guard parent != startParent || parent == 1 else { return }
    emit(["k": "parent-gone", "startedBy": Int(startParent), "parent": Int(parent)])
    exit(3)
}
requireParent()

func activeDisplays() -> [CGDirectDisplayID] {
    var count: UInt32 = 0
    guard CGGetActiveDisplayList(0, nil, &count) == .success, count > 0 else { return [] }
    var ids = [CGDirectDisplayID](repeating: 0, count: Int(count))
    guard CGGetActiveDisplayList(count, &ids, &count) == .success else { return [] }
    return Array(ids.prefix(Int(count)))
}

func displayInfo() -> [[String: Any]] {
    activeDisplays().map { id in
        let bounds = CGDisplayBounds(id)
        let screen = NSScreen.screens.first {
            ($0.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value == id
        }
        return ["id": Int(id), "main": CGDisplayIsMain(id) != 0, "builtin": CGDisplayIsBuiltin(id) != 0,
                "bounds": [bounds.minX, bounds.minY, bounds.width, bounds.height],
                "modeRefreshHz": CGDisplayCopyDisplayMode(id)?.refreshRate ?? -1,
                "maxFPS": screen?.maximumFramesPerSecond ?? -1]
    }
}

@Sendable func children(of pid: pid_t) -> [pid_t] {
    var buffer = [pid_t](repeating: 0, count: 1024)
    _ = proc_listchildpids(pid, &buffer, Int32(buffer.count * MemoryLayout<pid_t>.size))
    return buffer.filter { $0 > 0 }
}

enum Runs { case binary, other, gone, unknown(String) }

/// Whether a process runs the binary. proc_pidpath (libproc) names the
/// process's executable, by whichever hard-link name the kernel looked up last;
/// stat on that name gives its device and inode, which decide. `gone` when the
/// process has ended (a zombie included), `unknown` when it exists but its
/// executable cannot be read.
@Sendable func runs(_ pid: pid_t) -> Runs {
    var buffer = [CChar](repeating: 0, count: 4 * Int(MAXPATHLEN))
    guard proc_pidpath(pid, &buffer, UInt32(buffer.count)) > 0 else {
        return errno == ESRCH ? .gone : .unknown("proc_pidpath: " + String(cString: strerror(errno)))
    }
    var file = stat()
    guard stat(buffer, &file) == 0 else {
        return .unknown("stat " + String(cString: buffer) + ": " + String(cString: strerror(errno)))
    }
    return file.st_dev == expectedFile.st_dev && file.st_ino == expectedFile.st_ino ? .binary : .other
}

/// One record per process the probe could not do its job for; any makes the
/// exit code 4.
var guardFailures: [pid_t: String] = [:]
@Sendable func guardFailed(_ pid: pid_t, _ reason: String) {
    guard guardFailures[pid] == nil else { return }
    guardFailures[pid] = reason
    emit(["k": "guard-failed", "pid": Int(pid), "reason": reason])
}

func isAlive(_ pid: pid_t) -> Bool { kill(pid, 0) == 0 || errno == EPERM }

/// One process under watch.
final class Watched {
    let pid: pid_t
    let since: Double
    var policy = -2
    var active: Bool? = nil
    var policies: Set<Int> = []
    var killed = false
    var samples = 0
    var pids: Set<pid_t>
    init(_ pid: pid_t) {
        self.pid = pid
        since = now()
        pids = [pid]
    }
}

var watched: [pid_t: Watched] = [:]
var lastAdoption = 0.0
var lastSample = 0.0
var maxGap = 0.0
var lastFront: pid_t = -2
var lastMenu: pid_t = -2
var windowStates: [Int: String] = [:]
var maxWindows = 0
var firstWindowAt: Double? = nil
var firstOnscreenWindowAt: Double? = nil

/// "ours" for a watched process or one of its children, else "other".
func label(_ pid: pid_t) -> String {
    watched.values.contains { $0.pid == pid || $0.pids.contains(pid) } ? "ours" : "other"
}

/// Watches one process if it runs the binary and shares our parent. Called by
/// each sample and, so that an activation or registration before the next
/// sample is still acted on, by the workspace and registration handlers.
func adoptOne(_ pid: pid_t, via: String) {
    requireParent()
    guard watched[pid] == nil, guardFailures[pid] == nil, pid != getpid(),
          children(of: startParent).contains(pid) else { return }
    switch runs(pid) {
    case .binary: break
    case .other, .gone: return
    case .unknown(let why): return guardFailed(pid, "not adopted: cannot tell whether it runs the binary (\(why))")
    }
    watched[pid] = Watched(pid)
    lastAdoption = now()
    emit(["k": "watch", "pid": Int(pid), "forbidden": forbidden.contains(pid), "via": via])
}

/// Watches every new process that runs the binary and shares our parent.
func adopt() {
    for pid in children(of: startParent) { adoptOne(pid, via: "sample") }
}

@Sendable func killWatched(_ process: Watched, _ reason: String) {
    guard killOnViolation, !process.killed, guardFailures[process.pid] == nil else { return }
    requireParent()
    let refused = "kill refused for violation \(reason): "
    if forbidden.contains(process.pid) { return guardFailed(process.pid, refused + "the PID is in --forbid") }
    switch runs(process.pid) {
    case .binary: break
    case .gone: return
    case .other: return guardFailed(process.pid, refused + "the PID no longer runs the binary")
    case .unknown(let why): return guardFailed(process.pid, refused + why)
    }
    process.killed = true
    let result = kill(process.pid, SIGKILL)
    let error = errno
    emit(["k": "killed", "pid": Int(process.pid), "result": Int(result), "reason": reason])
    if result != 0 && error != ESRCH {
        guardFailed(process.pid, "kill failed for violation \(reason): " + String(cString: strerror(error)))
    }
}

func report(_ process: Watched, _ conditions: [String]) {
    for condition in conditions where allowed.contains(condition) { bump("allowed-" + condition) }
    let found = stopping(conditions)
    guard !found.isEmpty else { return }
    for violation in found { bump("violation-" + violation) }
    emit(["k": "violation", "pid": Int(process.pid), "what": found])
    killWatched(process, found.joined(separator: ","))
}

func sample() {
    requireParent()
    adopt()
    let workspace = NSWorkspace.shared
    let front = workspace.frontmostApplication?.processIdentifier ?? -1
    let menu = workspace.menuBarOwningApplication?.processIdentifier ?? -1
    if front != lastFront {
        emit(["k": "front", "from": Int(lastFront), "to": Int(front), "name": label(front)])
        lastFront = front
    }
    if menu != lastMenu {
        emit(["k": "menu", "from": Int(lastMenu), "to": Int(menu), "name": label(menu)])
        lastMenu = menu
    }
    let living = watched.values.filter { isAlive($0.pid) }
    guard !living.isEmpty else { return }
    bump("samples")
    let t = now()
    if lastSample > 0 { maxGap = max(maxGap, t - lastSample) }
    lastSample = t

    let displays = activeDisplays().map { CGDisplayBounds($0) }
    let list = (CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as? [[String: Any]]) ?? []
    // The on-screen windows above the desktop's icons, front to back (the order
    // this option documents): owner, number, layer, alpha, bounds. Each window of
    // ours is placed among those up to its own layer (see the header).
    let icons = Int(CGWindowLevelForKey(.desktopIconWindow))
    let stack: [(owner: Int, number: Int, layer: Int, alpha: Double, bounds: CGRect)] =
        ((CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]]) ?? []).compactMap {
            guard let layer = $0[kCGWindowLayer as String] as? Int, layer > icons,
                  let owner = $0[kCGWindowOwnerPID as String] as? Int,
                  let boundsInfo = $0[kCGWindowBounds as String],
                  let bounds = CGRect(dictionaryRepresentation: boundsInfo as! CFDictionary) else { return nil }
            return (owner, $0[kCGWindowNumber as String] as? Int ?? -1, layer,
                    $0[kCGWindowAlpha as String] as? Double ?? 1, bounds)
        }
    // The pointer, in the window list's coordinates: origin at the top left of the
    // main display, where NSEvent's is at its bottom left.
    let mouse = NSEvent.mouseLocation
    let pointer = CGPoint(x: mouse.x, y: CGDisplayBounds(CGMainDisplayID()).height - mouse.y)
    var pointerOver = false
    var record: [String: Any] = ["k": "s", "front": Int(front), "menu": Int(menu)]
    var apps: [[String: Any]] = []
    var windowCount = 0
    for process in living {
        let pids = Set([process.pid] + children(of: process.pid))
        process.pids.formUnion(pids)
        var active = false
        var app: [String: Any] = ["pid": Int(process.pid), "pids": pids.count,
                                  "pidList": pids.sorted().map { Int($0) }]
        if let running = NSRunningApplication(processIdentifier: process.pid) {
            let policy = running.activationPolicy.rawValue
            active = running.isActive
            app["policy"] = policy
            app["active"] = active
            process.policies.insert(policy)
            if policy != process.policy {
                emit(["k": "policy", "pid": Int(process.pid), "from": process.policy, "to": policy])
                process.policy = policy
            }
            if process.active != active {
                emit(["k": "active", "pid": Int(process.pid), "to": active])
                process.active = active
            }
            if policy == NSApplication.ActivationPolicy.regular.rawValue { bump("policy-regular") }
        } else {
            bump("not-registered")
        }
        var windows: [[String: Any]] = []
        var facts: [WindowFacts] = []
        for info in list {
            guard let owner = info[kCGWindowOwnerPID as String] as? Int, pids.contains(pid_t(owner)),
                  let boundsInfo = info[kCGWindowBounds as String],
                  let bounds = CGRect(dictionaryRepresentation: boundsInfo as! CFDictionary) else { continue }
            let number = info[kCGWindowNumber as String] as? Int ?? -1
            let onscreen = info[kCGWindowIsOnscreen as String] as? Bool ?? false
            // Not reported counts as opaque, so a missing value can only stop the app.
            let alpha = info[kCGWindowAlpha as String] as? Double ?? 1
            let layer = info[kCGWindowLayer as String] as? Int ?? 0
            let onDisplay = bounds.width > 0 && bounds.height > 0 && displays.contains { $0.intersects(bounds) }
            facts.append(WindowFacts(onscreen: onscreen, alpha: alpha))
            let upToOwn = stack.filter { $0.layer <= layer }
            let index = upToOwn.firstIndex { $0.number == number } ?? -1
            let covered = index > 0 && upToOwn[..<index].contains {
                !pids.contains(pid_t($0.owner)) && $0.alpha > 0 && $0.bounds.contains(bounds)
            }
            if onscreen && bounds.width >= 200 && bounds.height >= 200 && bounds.contains(pointer) { pointerOver = true }
            let state = "pid=\(owner) layer=\(layer) onscreen=\(onscreen) alpha=\(alpha) "
                + "bounds=\(Int(bounds.minX)),\(Int(bounds.minY)),\(Int(bounds.width))x\(Int(bounds.height)) "
                + "onDisplay=\(onDisplay) index=\(index)/\(upToOwn.count) covered=\(covered)"
            if windowStates[number] != state {
                emit(["k": "window", "number": number, "state": state, "onscreen": onscreen, "alpha": alpha,
                      "layer": layer, "onDisplay": onDisplay, "index": index, "of": upToOwn.count, "covered": covered,
                      "bounds": [bounds.minX, bounds.minY, bounds.width, bounds.height]])
                windowStates[number] = state
            }
            if firstWindowAt == nil { firstWindowAt = now() }
            if onscreen && firstOnscreenWindowAt == nil { firstOnscreenWindowAt = now() }
            windows.append(["n": number, "on": onscreen, "a": alpha, "l": layer, "d": onDisplay,
                            "x": Int(bounds.minX), "y": Int(bounds.minY),
                            "w": Int(bounds.width), "h": Int(bounds.height), "i": index, "of": upToOwn.count,
                            "c": covered])
        }
        count(track(t, windows))
        // Samples that show the mode doing its job: a window ordered in, every
        // ordered-in window at alpha 0. Without them an alpha check proves nothing.
        let onscreenFacts = facts.filter { $0.onscreen }
        if !onscreenFacts.isEmpty && onscreenFacts.allSatisfy({ $0.alpha == 0 }) { bump("onscreen-alpha0") }
        windowCount += windows.count
        app["windows"] = windows
        var found = violations(front: front, menu: menu, pids: pids, active: active, windows: facts)
        process.samples += 1
        if selfTest { found.append("self-test") }
        if !found.isEmpty {
            app["violations"] = found
            report(process, found)
        }
        apps.append(app)
    }
    maxWindows = max(maxWindows, windowCount)
    if pointerOver { bump("pointer-over-window") }
    record["apps"] = apps
    emit(record)
}

func schedule() {
    let fast = watched.isEmpty || now() - lastAdoption < fastSeconds
    let timer = Timer(timeInterval: fast ? fastInterval : slowInterval, repeats: false) { _ in
        sample()
        schedule()
    }
    RunLoop.main.add(timer, forMode: .common)
}

func finish() {
    sample()
    count(Tracked(slides: endOpenSlides(now()), hidden: endHidden()))
    emit(["k": "summary", "counts": counts, "maxWindows": maxWindows, "maxSampleGap": maxGap,
          "firstWindowAt": firstWindowAt ?? -1, "firstOnscreenWindowAt": firstOnscreenWindowAt ?? -1,
          "firstHiddenAt": firstHiddenAt ?? -1, "watched": watched.values.sorted { $0.since < $1.since }.map {
              ["pid": Int($0.pid), "since": $0.since, "policies": $0.policies.sorted(), "killed": $0.killed]
          }, "guardFailed": guardFailures.map { ["pid": Int($0.key), "reason": $0.value] }])
    exit(guardFailures.isEmpty ? 0 : 4)
}

// Without an application object the probe got no notification of a desktop
// change: none in 10 logs, among them three desktop slides seen in the samples
// of one run (2026-11-24). Prohibited: no Dock icon, no menu bar, never active.
let application = NSApplication.shared
application.setActivationPolicy(.prohibited)

let center = NSWorkspace.shared.notificationCenter
for name in [NSWorkspace.didLaunchApplicationNotification, NSWorkspace.didActivateApplicationNotification,
             NSWorkspace.didDeactivateApplicationNotification, NSWorkspace.didHideApplicationNotification,
             NSWorkspace.didUnhideApplicationNotification, NSWorkspace.didTerminateApplicationNotification,
             NSWorkspace.activeSpaceDidChangeNotification] {
    center.addObserver(forName: name, object: nil, queue: .main) { note in
        requireParent()
        let app = note.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication
        let pid = app?.processIdentifier ?? -1
        // A process activated before the next sample adopted it still counts.
        if pid > 0 { adoptOne(pid, via: "workspace") }
        emit(["k": "ws", "name": note.name.rawValue, "pid": Int(pid), "app": label(pid),
              "policy": app?.activationPolicy.rawValue ?? -1])
        // Counted once the app has a window on screen: only then can a change move it.
        if note.name == NSWorkspace.activeSpaceDidChangeNotification, firstOnscreenWindowAt != nil {
            bump("desktop-changes")
        }
        // An activation between two samples still counts.
        if note.name == NSWorkspace.didActivateApplicationNotification,
           let process = watched.values.first(where: { $0.pid == pid || $0.pids.contains(pid) }) {
            report(process, ["activated"])
        }
    }
}

// The activation policy each app running the binary had when it registered
// with the system, which samples can miss.
let registrations = NSWorkspace.shared.observe(\.runningApplications, options: [.new]) { _, change in
    requireParent()
    for app in change.newValue ?? [] {
        adoptOne(app.processIdentifier, via: "registered")
        guard watched[app.processIdentifier] != nil else { continue }
        emit(["k": "registered", "pid": Int(app.processIdentifier), "policy": app.activationPolicy.rawValue])
    }
}

signal(SIGTERM, SIG_IGN)
let terminate = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .main)
terminate.setEventHandler { finish() }
terminate.resume()

emit(["k": "ready", "displays": displayInfo(), "binary": expectedBinary, "forbidden": forbidden.map { Int($0) },
      "allowed": allowed.sorted(), "startedBy": Int(startParent),
      "probePolicy": NSRunningApplication.current.activationPolicy.rawValue])
schedule()
withExtendedLifetime(registrations) { application.run() }
