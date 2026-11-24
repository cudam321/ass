#!/usr/bin/env python3
# Run: python3 -I scripts/appprobe_test.py [probe binary]   (no app launch; needs swiftc and cc)
"""Offline tests of scripts/appprobe.swift. The watched binary is a C program that
sleeps and never touches AppKit; --self-test makes every sample a violation.
  identity  A process started under any hard-link name of the binary (2 and 200 links)
            is adopted and killed. Before each start, 50 threads look up another
            name: the kernel then reports the process under that name, also to the
            probe (a probe that compares paths fails here). A byte-identical copy
            (another inode) and the binary under another parent are left alone.
  allow     --allow turns off one stop condition (checked by --replay on a made-up
            log); a rule about which process may be signalled cannot be named.
  desktops  --replay finds the desktop slides in a made-up log (off, back, still sliding;
            a move in y is none) and which of them brought the window back. The probe
            runs with the activation policy Prohibited.
  visibility  --replay counts the samples in which the main window could not be seen
            outside a switch, at layer 8 (on every desktop: switches with a pause, a
            window stranded off the display, covered, off screen and back, a second
            window, the close) and at layer 0 (away with its desktop, back, away at
            the end), and the slides of each kind.
  guard     A refused kill (--forbid) and a child whose executable cannot be read
            each give one guard-failed record, and the probe exits 4. A script
            that starts hundreds of short-lived children gives none.
  orphan    When the script that started the probe dies (SIGTERM, SIGINT, SIGKILL),
            the probe exits by itself and touches nothing.
Exit code 0 when every case passes."""
import json, os, shutil, signal, subprocess, sys, tempfile, threading, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SOURCE = os.path.join(ROOT, "scripts", "appprobe.swift")
PROBE = os.path.realpath(sys.argv[1]) if len(sys.argv) > 1 else os.path.join(ROOT, "target", "appprobe")
DUMMY_C = b"#include <stdlib.h>\n#include <unistd.h>\nint main(int c, char **v) { sleep(c > 1 ? atoi(v[1]) : 1); return 0; }\n"
RESULTS = []


def check(name, ok, detail):
    RESULTS.append(ok)
    print("%s  %s: %s" % ("PASS" if ok else "FAIL", name, detail), flush=True)


def alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False


def records(log):
    return [json.loads(line) for line in open(log) if line.startswith("{")]


def start_probe(log, *args):
    probe = subprocess.Popen([PROBE] + list(args), stdout=open(log, "w"), stderr=subprocess.STDOUT)
    while '"k":"ready"' not in open(log).read():
        if probe.poll() is not None:
            sys.exit("the probe did not start: %s" % open(log).read())
        time.sleep(0.02)
    return probe


def stop_probe(probe):
    probe.send_signal(signal.SIGTERM)
    return probe.wait(timeout=10)


def killed(path):
    """Starts the binary under this name; True if the probe SIGKILLs it within 1.5 s."""
    process = subprocess.Popen([path, "1"])
    try:
        return process.wait(timeout=1.5) == -signal.SIGKILL
    except subprocess.TimeoutExpired:
        process.wait()
        return False


def look_up(path, threads=50):
    """Looks the name up from many threads, so it is the name the kernel reports."""
    workers = [threading.Thread(target=os.stat, args=(path,)) for _ in range(threads)]
    for worker in workers:
        worker.start()
    for worker in workers:
        worker.join()


def under_launchd(path, folder, name):
    """Starts the binary from a shell that exits at once, so launchd becomes its parent."""
    pidfile = os.path.join(folder, name)
    subprocess.run(["/bin/sh", "-c", '"$1" 3 > /dev/null 2>&1 & echo $! > "$2"', "sh", path, pidfile])
    return int(open(pidfile).read())


def identity(folder, dummy):
    two = os.path.join(folder, "two")
    os.mkdir(two)
    a, b, copy = (os.path.join(two, n) for n in ("a", "b", "copy"))
    shutil.copy2(dummy, a)
    os.link(a, b)
    shutil.copy2(a, copy)
    log = os.path.join(folder, "identity.jsonl")
    probe = start_probe(log, "--binary", a, "--kill", "--self-test")
    check("identity, 2 links", killed(a), "started as a: adopted and killed")
    look_up(b)
    check("identity, 2 links", all(look_up(b) or killed(b) for _ in range(3)),
          "started as b 3 times, b looked up by 50 threads before each: adopted and killed")
    stop = threading.Event()

    def look_up_b():
        while not stop.is_set():
            os.stat(b)
    looker = threading.Thread(target=look_up_b)
    looker.start()
    check("identity, 2 links", all(killed(a) for _ in range(5)), "started as a 5 times while b is looked up: killed")
    stop.set()
    looker.join()
    process = subprocess.Popen([copy, "1"])
    check("identity, copy", process.wait() == 0 and not [r for r in records(log) if r.get("pid") == process.pid],
          "a byte-identical copy (another inode) is not adopted")
    other = under_launchd(a, folder, "other")
    time.sleep(0.5)
    check("identity, parent", alive(other) and not [r for r in records(log) if r.get("pid") == other],
          "the binary under launchd is not adopted")
    check("identity, exit", stop_probe(probe) == 0, "probe exit code 0, no guard failure")

    many = os.path.join(folder, "many")
    os.mkdir(many)
    names = [os.path.join(many, "l%03d" % i) for i in range(200)]
    shutil.copy2(dummy, names[0])
    for name in names[1:]:
        os.link(names[0], name)
    probe = start_probe(os.path.join(folder, "many.jsonl"), "--binary", names[0], "--kill", "--self-test")
    first_miss = next((name for name in names if not (look_up(name if name != names[0] else names[1]) or killed(name))),
                      None)
    stop_probe(probe)
    check("identity, 200 links", first_miss is None and os.stat(names[0]).st_nlink == 200,
          "started under each of the 200 names: all killed" if first_miss is None
          else "started as %s: not killed" % os.path.basename(first_miss))
    while alive(other):
        time.sleep(0.1)


def allow(folder):
    log = os.path.join(folder, "allow.jsonl")
    with open(log, "w") as f:
        f.write(json.dumps({"k": "watch", "pid": 7}) + "\n")
        f.write(json.dumps({"k": "s", "front": 1, "menu": 1, "apps": [
            {"pid": 7, "pidList": [7], "active": False, "windows": [{"on": True, "a": 1.0}]}]}) + "\n")
    def violations(*allowed):
        out = subprocess.run([PROBE, "--replay", log] + [x for a in allowed for x in ("--allow", a)],
                             capture_output=True, text=True).stdout.strip().splitlines()
        return json.loads(out[-1])["violations"] if out else None
    counts = (violations(), violations("window-visible"), violations("frontmost"))
    check("allow, one condition", counts == (1, 0, 1),
          "a visible window: %s violation(s); with --allow window-visible: %s; with --allow frontmost: %s" % counts)
    def refuses(name):
        try:
            return subprocess.run([PROBE, "--binary", "/bin/sh", "--allow", name], capture_output=True,
                                  timeout=5).returncode != 0
        except subprocess.TimeoutExpired:   # it accepted the name and ran
            return False
    refused = [name for name in ("forbid", "parent", "identity", "binary", "self-test") if refuses(name)]
    check("allow, process rules", len(refused) == 5, "--allow refused for %s" % ", ".join(refused))


def desktops(folder, dummy):
    log = os.path.join(folder, "slides.jsonl")
    def window(x, y=33, on=True, n=5):
        return {"n": n, "on": on, "a": 0.0, "x": x, "y": y, "w": 1280, "h": 800, "d": on and x > -1280}
    frames = ([window(0), window(0), window(162), window(971), window(1575), window(0, on=False)]   # slides off
              + [window(1505), window(898), window(359), window(1), window(1), window(1)]        # slides in, stops
              + [window(1, y=40), window(1, y=60)]                                                # moved, not slid
              + [window(500, n=6), window(700, n=6)])                                             # open at the end
    with open(log, "w") as f:
        f.write(json.dumps({"k": "watch", "pid": 7}) + "\n")
        for i, w in enumerate(frames):
            f.write(json.dumps({"k": "s", "t": 100 + i, "front": 1, "menu": 1, "apps": [
                {"pid": 7, "pidList": [7], "active": False, "windows": [w]}]}) + "\n")
    out = [json.loads(line) for line in subprocess.run([PROBE, "--replay", log], capture_output=True,
                                                       text=True).stdout.splitlines() if line.startswith("{")]
    slides = [(r["from"], r["to"], r["open"], r["return"]) for r in out if r["k"] == "slide"]
    summary = out[-1] if out else {}
    check("desktops, slides", slides == [(0, 1575, False, False), (1505, 1, False, True), (500, 700, True, True)]
          and summary.get("slides") == 3 and summary.get("returns") == 2 and summary.get("violations") == 0,
          "slid off, slid back (a return), still sliding on the display at the end (a return): %s; "
          "a move in y: not counted" % slides)

    visibility(folder)
    probe = start_probe(os.path.join(folder, "policy.jsonl"), "--binary", dummy)
    ready = next(r for r in records(os.path.join(folder, "policy.jsonl")) if r["k"] == "ready")
    code = stop_probe(probe)
    check("desktops, probe policy", ready.get("probePolicy") == 2 and code == 0,
          "the probe's own activation policy: %s (2 is Prohibited), exit %s" % (ready.get("probePolicy"), code))


def replay_log(folder, name, frames, step=0.05):
    """Writes one sample per entry (a list of windows) and returns --replay's records."""
    log = os.path.join(folder, name)
    with open(log, "w") as f:
        f.write(json.dumps({"k": "watch", "pid": 7}) + "\n")
        for i, windows in enumerate(frames):
            f.write(json.dumps({"k": "s", "t": 100 + i * step, "front": 1, "menu": 1, "apps": [
                {"pid": 7, "pidList": [7], "active": False, "windows": windows}]}) + "\n")
    return [json.loads(line) for line in subprocess.run([PROBE, "--replay", log], capture_output=True,
                                                        text=True).stdout.splitlines() if line.startswith("{")]


def visibility(folder):
    def window(x, layer, on=True, c=False, n=5, w=1280, h=800):
        return {"n": n, "on": on, "a": 0.0, "l": layer, "x": x, "y": 33, "w": w, "h": h, "c": c,
                "d": on and -w < x < 1512}
    # Layer 8, on every desktop: a switch with a one-sample pause off the display; the
    # window left 20 samples (1 s) and 10 more on no display; covered 3 samples, and once
    # while moving; off screen 4 samples; a second window that goes away; off screen at
    # the end for 2 samples, as when it closes.
    still = lambda x, k, **kw: [[window(x, 8, **kw)] for _ in range(k)]
    frames = (still(0, 3)
              + [[window(6305, 8)], [window(6306, 8)], [window(6306, 8)], [window(6307, 8)], [window(0, 8)]]
              + still(0, 2) + [[window(1600, 8)]] + still(1600, 30) + [[window(0, 8)]] + still(0, 2)
              + still(0, 3, c=True) + [[window(9, 8, c=True)]] + still(9, 1)
              + still(9, 4, on=False) + still(9, 2)
              + [[window(9, 8), window(0, 8, n=6, w=400, h=300)], [window(9, 8), window(0, 8, n=6, w=400, h=300,
                                                                                         on=False)]]
              + still(9, 2) + still(9, 2, on=False))
    out = replay_log(folder, "all-desktops.jsonl", frames)
    summary = out[-1] if out else {}
    slides = [(r["from"], r["to"], r["layer"], r["return"]) for r in out if r["k"] == "slide"]
    # 20 samples of 0.05 s make the 1 s; the 30 still samples at 1600 all count.
    want = {"off-screen": 4, "off-display": 30, "covered": 3}
    first = 100 + 0.05 * (3 + 5 + 2 + 1)
    check("visibility, layer 8", summary.get("hidden") == want and summary.get("switchSlides") == len(slides) == 5
          and summary.get("slides") == 0 and summary.get("returns") == 0
          and abs(summary.get("firstHiddenAt", 0) - first) < 1e-6 and all(not s[3] for s in slides),
          "a switch with a pause (2 slides) and no count; off the display 1.5 s: %s samples; covered %s; "
          "off screen and back %s; a second window and the close not counted: %s; switch slides %s, desktop slides "
          "%s, returns %s" % (summary.get("hidden", {}).get("off-display"), summary.get("hidden", {}).get("covered"),
                              summary.get("hidden", {}).get("off-screen"), summary.get("hidden") == want,
                              summary.get("switchSlides"), summary.get("slides"), summary.get("returns")))
    # Layer 0: the window leaves with its desktop for 30 samples, comes back sliding in
    # from 4694 (on no display for one sample, not counted), and is away again for the
    # last 40 samples (2 s), which count at the end.
    still0 = lambda x, k, **kw: [[window(x, 0, **kw)] for _ in range(k)]
    frames = (still0(0, 3) + [[window(-1574, 0)]] + still0(0, 30, on=False)
              + [[window(4694, 0)], [window(2000, 0)], [window(0, 0)]]
              + still0(0, 3) + [[window(-1574, 0)]] + still0(0, 40, on=False))
    out = replay_log(folder, "one-desktop.jsonl", frames)
    summary = out[-1] if out else {}
    slides = [(r["from"], r["to"], r["return"]) for r in out if r["k"] == "slide"]
    check("visibility, layer 0", summary.get("hidden") == {"off-screen": 70, "off-display": 0, "covered": 0}
          and slides == [(0, -1574, False), (4694, 0, True), (0, -1574, False)] and summary.get("switchSlides") == 0,
          "away 30 samples and back, away 40 at the end: %s; slides %s (returns as before)"
          % (summary.get("hidden"), slides))


def guard(folder, dummy):
    process = subprocess.Popen([dummy, "2"])
    log = os.path.join(folder, "forbid.jsonl")
    probe = start_probe(log, "--binary", dummy, "--kill", "--self-test", "--forbid", str(process.pid))
    time.sleep(1)
    code = stop_probe(probe)
    found = records(log)
    failed = [r for r in found if r["k"] == "guard-failed"]
    check("guard, forbid", process.wait() == 0 and len(failed) == 1 and code == 4,
          "%d violations, %d guard-failed (%s), not killed, probe exit %s"
          % (sum(r["k"] == "violation" for r in found), len(failed), failed[0]["reason"] if failed else "-", code))

    # A child running a deleted binary cannot be identified. macOS sometimes ends
    # such a process with SIGKILL 40 to 75 ms after its binary is deleted (4 of
    # 60 trials, 2026-11-24); this case then failed 1 run in 6 or 8, when that
    # came before the probe's first look. So the probe starts first, the probe
    # killing it is what fails, and the case is tried again (up to 3 times) only
    # when the system ended the child before the probe reported it.
    gone = os.path.join(folder, "deleted")
    for attempt in range(1, 4):
        log = os.path.join(folder, "unknown-%d.jsonl" % attempt)
        probe = start_probe(log, "--binary", dummy, "--kill", "--self-test")
        shutil.copy2(dummy, gone)
        process = subprocess.Popen([gone, "10"])
        time.sleep(0.2)
        os.remove(gone)
        time.sleep(0.5)
        code = stop_probe(probe)
        alive = process.poll() is None
        if alive:
            process.terminate()
        status = process.wait()
        found = records(log)
        failed = [r for r in found if r["k"] == "guard-failed" and r["pid"] == process.pid]
        by_probe = [r for r in found if r["k"] == "killed" and r["pid"] == process.pid]
        if failed or alive or by_probe:
            break
    check("guard, unidentified", len(failed) == 1 and code == 4 and not by_probe,
          "a child running a deleted binary: not adopted, %d guard-failed (%s), probe exit %s, killed by the probe %s;"
          " attempt %d, the child %s" % (len(failed), failed[0]["reason"] if failed else "-", code, bool(by_probe),
                                        attempt, "alive until the end" if alive else "ended with status %s" % status))


def churn(folder, dummy):
    log = os.path.join(folder, "churn.jsonl")
    script = ('"$1" --binary "$2" --kill > "$3" 2>&1 & P=$!\n'
              'until grep -q \'"k":"ready"\' "$3"; do sleep 0.02; done\n'
              'i=0; while [ $i -lt 300 ]; do /usr/bin/true & /usr/bin/true; i=$((i + 1)); done\n'
              'sleep 0.5; kill -TERM $P; wait $P; echo $?\n')
    out = subprocess.run(["/bin/sh", "-c", script, "sh", PROBE, dummy, log], capture_output=True, text=True)
    failed = [r for r in records(log) if r["k"] == "guard-failed"]
    check("guard, churn", out.stdout.strip() == "0" and not failed,
          "600 short-lived children of the probe's parent: %d guard-failed, probe exit %s"
          % (len(failed), out.stdout.strip()))


def orphan(folder, dummy, name):
    log, pidfile = os.path.join(folder, name + ".jsonl"), os.path.join(folder, name + ".pid")
    shell = subprocess.Popen(["/bin/sh", "-c", '"$1" --binary "$2" --kill --self-test > "$3" 2>&1 & echo $! > "$4"; wait',
                              "sh", PROBE, dummy, log, pidfile],
                             preexec_fn=lambda: signal.signal(signal.SIGINT, signal.SIG_DFL))
    while not (os.path.exists(pidfile) and os.path.exists(log) and '"k":"ready"' in open(log).read()):
        time.sleep(0.02)
    probe = int(open(pidfile).read())
    before = under_launchd(dummy, folder, name + ".before")
    time.sleep(0.3)
    os.kill(shell.pid, getattr(signal, name))
    shell.wait()
    end = time.time() + 1
    while alive(probe) and time.time() < end:
        time.sleep(0.005)
    probe_left = alive(probe)
    after = under_launchd(dummy, folder, name + ".after")
    time.sleep(0.6)
    touched = [r for r in records(log) if r["k"] in ("watch", "killed") and r.get("pid") in (before, after)]
    check("orphan, %s" % name, not probe_left and alive(before) and alive(after) and not touched
          and any(r["k"] == "parent-gone" for r in records(log)),
          "script killed: probe %s; the binary under launchd %s" % (
              "still running" if probe_left else "exited by itself", "touched" if touched else "untouched"))
    if probe_left:
        os.kill(probe, signal.SIGTERM)
    for pid in (before, after):
        while alive(pid):
            time.sleep(0.1)


if __name__ == "__main__":
    if len(sys.argv) == 1 and (not os.path.exists(PROBE) or os.path.getmtime(PROBE) < os.path.getmtime(SOURCE)):
        subprocess.run(["swiftc", "-O", "-o", PROBE, SOURCE], check=True)
    with tempfile.TemporaryDirectory() as folder:
        dummy = os.path.join(folder, "dummy")
        subprocess.run(["cc", "-x", "c", "-o", dummy, "-"], input=DUMMY_C, check=True)
        identity(folder, dummy)
        allow(folder)
        desktops(folder, dummy)
        guard(folder, dummy)
        churn(folder, dummy)
        for signal_name in ("SIGTERM", "SIGINT", "SIGKILL"):
            orphan(folder, dummy, signal_name)
    print("%d of %d passed" % (sum(RESULTS), len(RESULTS)))
    sys.exit(0 if all(RESULTS) else 1)
