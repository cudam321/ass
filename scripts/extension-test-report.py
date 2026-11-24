#!/usr/bin/env python3
"""Result report for the extension test run (tests/extension/checks.json).

  extension-test-report.py report <run_dir> <checks.json>
  extension-test-report.py plan <run_dir> <checks.json>
  extension-test-report.py lint <checks.json> <path> [<path>...]
  extension-test-report.py selftest <checks.json>

Run as python3 -I. Standard library only. Prints inputs to a decision, never a verdict.
"""
import json
import os
import re
import shutil
import sys
import tempfile

VIA_ORDER = ["http", "cdp", "script"]
RESULTS = ["works", "fails", "crashes", "absent", "not-run", "no-record"]
EXT_CONTEXTS = ("worker", "content", "popup", "options")
DETAIL_MAX = 110
BASIC_GROUPS = [
    ("content_scripts", "content scripts"),
    ("worker", "service worker"),
    ("messaging", "messaging"),
    ("storage", "storage"),
    ("dnr", "declarativeNetRequest"),
    ("extension_pages", "extension pages in a view of ours"),
    ("load", "extension load"),
]


# ---------- reading ----------

def load_jsonl(path, counter):
    """Returns the parsed dict lines of a file; counter[0] counts the lines skipped."""
    out = []
    try:
        f = open(path, encoding="utf-8", errors="replace")
    except OSError:
        return out
    with f:
        for line in f:
            if not line.strip():
                continue
            try:
                obj = json.loads(line)
            except ValueError:
                counter[0] += 1
                continue
            if isinstance(obj, dict):
                out.append(obj)
            else:
                counter[0] += 1
    return out


def num(v, default=None):
    if isinstance(v, bool):
        return default
    if isinstance(v, (int, float)):
        return v
    try:
        return int(v)
    except (TypeError, ValueError):
        return default


def flat(d, limit=None):
    if d is None:
        s = ""
    elif isinstance(d, str):
        s = d
    else:
        s = json.dumps(d, separators=(",", ":"), sort_keys=True)
    s = " ".join(s.split())
    if limit and len(s) > limit:
        s = s[: limit - 3] + "..."
    return s


def dash(v):
    """A value for the report: '-' when it is missing."""
    return "-" if v is None else v


def is_final(r):
    return r.get("final") is not False


def via_text(via):
    return "+".join(v for v in VIA_ORDER if v in via) or "-"


# ---------- analysis ----------

def analyze(run_dir, checks_path):
    with open(checks_path, encoding="utf-8") as f:
        spec = json.load(f)
    checks = spec["checks"]
    bad = [0]
    raw_counts = {}
    merged = {}
    for name, via in (("records.jsonl", "http"), ("cdp-records.jsonl", "cdp"), ("script.jsonl", "script")):
        recs = load_jsonl(os.path.join(run_dir, name), bad)
        raw_counts[via] = len(recs)
        for r in recs:
            r_launch = num(r.get("launch"))
            key = (r.get("id"), r_launch, r.get("by"), r.get("t"), r.get("result"), r.get("phase"))
            if key in merged:
                merged[key]["via"].add(via)
                continue
            r = dict(r)
            r["via"] = {via}
            r["launch"] = r_launch
            r["t"] = num(r.get("t"), 0)
            merged[key] = r
    records = list(merged.values())
    requests = load_jsonl(os.path.join(run_dir, "requests.jsonl"), bad)
    load_jsonl(os.path.join(run_dir, "logins.jsonl"), bad)
    launches = load_jsonl(os.path.join(run_dir, "launches.jsonl"), bad)
    launches.sort(key=lambda x: num(x.get("launch"), 0))

    begins = [r for r in records if r.get("phase") == "begin"]
    results = {}
    for r in records:
        if r.get("phase") != "begin" and r.get("id") is not None:
            results.setdefault(r["id"], []).append(r)

    def order(r):
        return (r["launch"] if r["launch"] is not None else -1, r["t"])

    chosen = {}
    disagreements = []
    differs = []
    for c in checks:
        cid = c["id"]
        recs = results.get(cid, [])
        finals = [r for r in recs if is_final(r)]
        pool = finals if finals else recs
        if not pool:
            started = sorted(set(b["launch"] for b in begins if b.get("id") == cid and b["launch"] is not None))
            detail = ("begin marker sent in launch %s, no result: the call hung or its worker ended"
                      % ", ".join(str(n) for n in started)) if started else ""
            chosen[cid] = {"result": "no-record", "detail": detail, "via": set(), "launch": None, "t": 0,
                           "received": None, "final": True, "none": True}
            continue
        best = max(pool, key=order)
        if c["group"] == "lifecycle":
            # Recorded once per launch: show the worst launch, not the latest,
            # so a crash in launch 1 is not hidden by a clean relaunch. not-run
            # ranks lowest: lifecycle.quit is not-run in a launch that ended early.
            worst = {"crashes": 3, "fails": 2, "works": 1}
            best = max(pool, key=lambda r: (worst.get(r.get("result"), 0), order(r)))
        chosen[cid] = {"result": best.get("result"), "detail": best.get("detail"), "via": set(best["via"]),
                       "launch": best["launch"], "t": best["t"], "received": num(best.get("received")),
                       "final": is_final(best), "none": False}
        if finals:
            same = [r for r in finals if r["launch"] == best["launch"]]
            if len(set(r.get("result") for r in same)) > 1:
                disagreements.append(cid)
            by_launch = {}
            for r in finals:
                if r["launch"] != best["launch"] and (r["launch"] is None or best["launch"] is None or r["launch"] < best["launch"]):
                    prev = by_launch.get(r["launch"])
                    if prev is None or order(r) > order(prev):
                        by_launch[r["launch"]] = r
            if any(r.get("result") != best.get("result") for r in by_launch.values()):
                differs.append(cid)

    # crashes, and launches the window probe ended because something showed on
    # screen (not a crash: the check in flight fails, and the next launch skips
    # it like a crashed one)
    notes = []
    crash_events = []
    screen_events = []
    check_ids = set(chosen)
    for ln in launches:
        n = num(ln.get("launch"))
        if not ln.get("ended_before_quit"):
            continue
        exit_ = dash(ln.get("exit"))
        sig = dash(ln.get("signal"))
        shown = (ln.get("probe_saw") or "something") if ln.get("probe_killed") else None
        done = set(r["id"] for r in records if r["launch"] == n and r.get("phase") != "begin" and r.get("id") is not None)
        open_ = [b for b in begins if b["launch"] == n and b.get("id") not in done]
        if not open_:
            in_launch = [r for r in records if r["launch"] == n and r.get("id") is not None]
            last = max(in_launch, key=lambda r: r["t"])["id"] if in_launch else "none"
            if shown:
                notes.append("launch %s: the window probe ended the app (%s) with no worker call in flight; last record: %s"
                             % (n, shown, last))
                screen_events.append((n, shown, None))
            else:
                notes.append("launch %s: browser process ended with no worker call in flight; last record: %s" % (n, last))
                crash_events.append((n, exit_, sig, None))
            continue
        b = max(open_, key=lambda r: r["t"])
        cid = b["id"]
        if shown:
            screen_events.append((n, shown, cid))
        else:
            crash_events.append((n, exit_, sig, cid))
        if cid not in check_ids:
            continue
        cur = chosen[cid]
        later_final = (not cur["none"]) and cur["final"] and cur["launch"] is not None and cur["launch"] > n
        if later_final:
            cur["detail"] = (flat(cur["detail"]) + " " if cur["detail"] else "") + (
                "showed on screen in launch %s: %s" % (n, shown) if shown else "crashed the browser in launch %s" % n)
        elif shown:
            chosen[cid] = {"result": "fails",
                           "detail": "showed on screen during this call, launch %s: %s" % (n, shown),
                           "via": set(b["via"]), "launch": n, "t": b["t"], "received": None, "final": True, "none": False,
                           "pinned": True}
        else:
            chosen[cid] = {"result": "crashes",
                           "detail": "browser process ended (exit %s, signal %s) during this call, launch %s" % (exit_, sig, n),
                           "via": set(b["via"]), "launch": n, "t": b["t"], "received": None, "final": True, "none": False,
                           "pinned": True}

    # server cross-check and cookie fact
    by_launch_requests = {}
    for q in requests:
        by_launch_requests.setdefault(num(q.get("launch")), []).append(q)
    for c in checks:
        cur = chosen[c["id"]]
        if cur["none"] or cur.get("pinned"):
            continue
        reqs = by_launch_requests.get(cur["launch"], [])
        srv = c.get("server")
        if srv:
            facts = []
            fail = None
            for p in srv.get("absent", []):
                k = sum(1 for q in reqs if q.get("path") == p)
                facts.append("%s received %dx" % (p, k) if k else "%s not received" % p)
                if k and not fail:
                    fail = "server received " + p
            for p in srv.get("present", []):
                k = sum(1 for q in reqs if q.get("path") == p)
                facts.append("%s received %dx" % (p, k) if k else "%s not received" % p)
                if not k and not fail:
                    fail = "server never received " + p
            if fail and cur["result"] == "works":
                cur["result"] = "fails"
                cur["detail"] = fail + ("; page reported: " + flat(cur["detail"]) if cur["detail"] else "")
            else:
                cur["detail"] = (flat(cur["detail"]) + " " if cur["detail"] else "") + "[server: " + "; ".join(facts) + "]"
        if c["id"] == "cookies.set":
            since = cur["received"] if cur["received"] is not None else cur["t"]
            carried = any(num(q.get("t"), 0) > since and "ass_ext_cookie" in (q.get("cookies") or []) for q in reqs)
            cur["detail"] = (flat(cur["detail"]) + " " if cur["detail"] else "") + \
                "[later request carried ass_ext_cookie: %s]" % ("yes" if carried else "no")

    # records on one path only (result records of extension and pages; script records are not paths)
    http_only = sum(1 for r in records if r.get("phase") != "begin" and r["via"] == {"http"})
    cdp_only = sum(1 for r in records if r.get("phase") != "begin" and r["via"] == {"cdp"})
    from_ext = any(r.get("by") in EXT_CONTEXTS for r in records)
    launch_numbers = set(num(ln.get("launch")) for ln in launches) | set(r["launch"] for r in records if r["launch"] is not None)
    return {
        "checks": checks, "chosen": chosen, "launches": launches, "launch_count": len(launch_numbers),
        "raw_counts": raw_counts, "bad": bad[0], "disagreements": disagreements, "differs": differs,
        "notes": notes, "crash_events": crash_events, "screen_events": screen_events, "http_only": http_only, "cdp_only": cdp_only,
        "from_ext": from_ext, "run_dir": run_dir,
    }


# ---------- output ----------

def table(rows):
    """Fixed-width columns; the last column is not padded."""
    widths = [max(len(r[i]) for r in rows) for i in range(len(rows[0]) - 1)]
    out = []
    for r in rows:
        out.append("  ".join(r[i].ljust(widths[i]) for i in range(len(widths))) + "  " + r[-1])
    return [s.rstrip() for s in out]


def ids_text(ids):
    return ", ".join(ids) if ids else "none"


def build_report(a):
    checks, chosen = a["checks"], a["chosen"]
    rc = a["raw_counts"]
    out = ["Extension run: %s   launches: %d   records: http %d, cdp %d, script %d" % (
        a["run_dir"], a["launch_count"], rc["http"], rc["cdp"], rc["script"])]
    if a["bad"]:
        out.append("Unparseable lines skipped: %d" % a["bad"])
    rows = [("ID", "GROUP", "RESULT", "VIA", "LAUNCH", "DETAIL")]
    for c in checks:
        cur = chosen[c["id"]]
        rows.append((c["id"], c["group"], str(cur["result"]), via_text(cur["via"]),
                     "-" if cur["launch"] is None else str(cur["launch"]), flat(cur["detail"], DETAIL_MAX)))
    out += table(rows)

    groups = []
    for c in checks:
        if c["group"] not in groups:
            groups.append(c["group"])
    out += ["", "Totals per group"]
    rows = [tuple(["GROUP"] + RESULTS[:-1] + ["no-record"])]
    total = dict((k, 0) for k in RESULTS)
    for g in groups:
        cnt = dict((k, 0) for k in RESULTS)
        for c in checks:
            if c["group"] == g:
                res = chosen[c["id"]]["result"]
                cnt[res] = cnt.get(res, 0) + 1
        for k in cnt:
            total[k] = total.get(k, 0) + cnt[k]
        rows.append(tuple([g] + [str(cnt[k]) for k in RESULTS]))
    rows.append(tuple(["TOTAL"] + [str(total[k]) for k in RESULTS]))
    out += table_even(rows)

    out += ["", 'Decision inputs']
    sp = chosen["load.secure_preferences"]
    out.append("  Extension loaded from the command line: %s %s" % (sp["result"], flat(sp["detail"], DETAIL_MAX)))
    out.append("  Basics:")
    for g, label in BASIC_GROUPS:
        basics = [c for c in checks if c["group"] == g and c.get("basic")]
        if not basics:
            continue
        bad = ["%s (%s)" % (c["id"], chosen[c["id"]]["result"]) for c in basics if chosen[c["id"]]["result"] != "works"]
        out.append("    %s: %s" % (label, "not working: " + ", ".join(bad) if bad else "all work"))

    def group_lines(group, label, events):
        sel = [c for c in checks if c["group"] == group and bool(c.get("event")) == events]
        bad = [c for c in sel if chosen[c["id"]]["result"] != "works"]
        res = ["  chrome.%s %s not %s (%d of %d):" % (group, label, "firing" if events else "working", len(bad), len(sel))]
        for c in bad:
            cur = chosen[c["id"]]
            res.append("    %s  %s  %s" % (c["id"], cur["result"], flat(cur["detail"], DETAIL_MAX)))
        return res

    for g in ("tabs", "windows"):
        out += group_lines(g, "calls", False)
        out += group_lines(g, "events", True)
    out.append("  Popup page in a view of ours: page.popup.loads %s, page.popup.chrome_apis %s" % (
        chosen["page.popup.loads"]["result"], chosen["page.popup.chrome_apis"]["result"]))
    crashes = a["crash_events"]
    if crashes:
        out.append("  Browser process crashes:")
        for n, exit_, sig, cid in crashes:
            out.append("    launch %s, exit %s, signal %s, check in flight: %s" % (n, exit_, sig, cid or "none"))
    else:
        out.append("  Browser process crashes: none")
    if a["screen_events"]:
        out.append("  Ended by the window probe (something showed on screen):")
        for n, shown, cid in a["screen_events"]:
            out.append("    launch %s, check in flight: %s; %s" % (n, cid or "none", shown))
    else:
        out.append("  Ended by the window probe: none")
    for note in a["notes"]:
        out.append("  Note: " + note)
    out.append("  Quit:")
    if a["launches"]:
        for ln in a["launches"]:
            if ln.get("quit_requested"):
                out.append("    launch %s: exit %s, signal %s, %s s after the quit request, helpers left %s" % (
                    ln.get("launch"), dash(ln.get("exit")), dash(ln.get("signal")),
                    dash(ln.get("quit_seconds")), dash(ln.get("helpers_left"))))
            else:
                out.append("    launch %s: ended before the quit request (exit %s, signal %s), helpers left %s" % (
                    ln.get("launch"), dash(ln.get("exit")), dash(ln.get("signal")), dash(ln.get("helpers_left"))))
    else:
        out.append("    no launches.jsonl")
    out.append("  Disagreements: " + ids_text(a["disagreements"]))
    out.append("  Differs between launches: " + ids_text(a["differs"]))
    out.append("  Records on one path only: %d (http only %d, cdp only %d)" % (
        a["http_only"] + a["cdp_only"], a["http_only"], a["cdp_only"]))
    return "\n".join(out)


def table_even(rows):
    """Like table(), but every column is padded to line up (all cells are short)."""
    widths = [max(len(r[i]) for r in rows) for i in range(len(rows[0]))]
    return ["  ".join(r[i].ljust(widths[i]) for i in range(len(widths))).rstrip() for r in rows]


def plan_ids(a):
    # Only checks that ended a launch (a crash, or the window probe) are skipped
    # next launch. Everything else runs again: page checks depend on what the
    # worker sets up in the same launch (dnr.dynamic.add, alarms.create...).
    return [c["id"] for c in a["checks"]
            if c["by"] != "script" and a["chosen"][c["id"]].get("pinned")]


# ---------- lint ----------

LITERAL = re.compile(r"""(['"`])([A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z0-9_]+)+)\1""")
LINT_EXT = (".js", ".html", ".sh", ".py")


def lint_files(paths):
    files = []
    for p in paths:
        if os.path.isdir(p):
            for root, dirs, names in os.walk(p):
                dirs[:] = sorted(d for d in dirs if d not in (".git", "node_modules"))
                for n in sorted(names):
                    if n.endswith(LINT_EXT):
                        files.append(os.path.join(root, n))
        else:
            files.append(p)
    return files


def cmd_lint(checks_path, paths):
    with open(checks_path, encoding="utf-8") as f:
        spec = json.load(f)
    ids = [c["id"] for c in spec["checks"]] + [c["id"] for c in spec.get("control", [])]
    dup = sorted(set(i for i in ids if ids.count(i) > 1))
    known = set(ids)
    prefixes = set(i.split(".")[0] for i in ids)
    found = set()
    unknown = 0
    total = 0
    for path in lint_files(paths):
        try:
            with open(path, encoding="utf-8", errors="replace") as f:
                lines = f.read().split("\n")
        except OSError as e:
            print("cannot read %s: %s" % (path, e))
            unknown += 1
            continue
        for n, line in enumerate(lines, 1):
            for m in LITERAL.finditer(line):
                lit = m.group(2)
                if lit.startswith("chrome.") or lit.split(".")[0] not in prefixes:
                    continue
                total += 1
                if lit in known:
                    found.add(lit)
                else:
                    unknown += 1
                    print("UNKNOWN %s %s:%d" % (lit, path, n))
    unused = [i for i in ids if i not in found]
    for i in dict.fromkeys(unused):
        print("UNUSED " + i)
    for i in dup:
        print("DUPLICATE " + i)
    print("lint: %d ids found, %d unknown, %d unused" % (total, unknown, len(set(unused))))
    return 0 if not unknown and not unused and not dup else 1


# ---------- selftest ----------

def cmd_selftest(checks_path):
    run = tempfile.mkdtemp(prefix="ass-ext-selftest-")
    with open(checks_path, encoding="utf-8") as f:
        spec = json.load(f)
    checks = spec["checks"]
    T = 1000000
    http, cdp, script, reqs = [], [], [], []

    def rec(cid, by, launch, t, result="works", detail="ok", **kw):
        d = {"id": cid, "group": "x", "result": result, "detail": detail, "by": by, "launch": launch, "t": T + t}
        d.update(kw)
        return d

    special = {"tabs.create", "cs.document_idle.iframe_cross_origin", "tabs.onCreated", "dnr.static.block_fetch",
               "tabs.query.all", "tabs.get", "alarms.onAlarm", "windows.get", "runtime.getManifest",
               "load.secure_preferences", "cookies.set"}
    for i, c in enumerate(checks):
        cid = c["id"]
        if cid in special or c["by"] == "script":
            continue
        http.append(rec(cid, c["by"], 2, 1000 + i, received=T + 1000 + i, via="http"))
    # script-owned checks go to script.jsonl
    for i, c in enumerate(checks):
        if c["by"] == "script" and c["id"] != "load.secure_preferences":
            script.append(rec(c["id"], "script", 2, 5000 + i, via="script"))
    script.append(rec("load.secure_preferences", "script", 1, 10, detail="location 8", via="script"))
    # launch 1 crashed; launch 2 (works, above) must not hide it
    script.append(rec("lifecycle.no_browser_crash", "script", 1, 900, result="crashes", detail="signal 11", via="script"))
    # the same crashed launch has no quit; launch 2's clean quit (works, above) must show
    script.append(rec("lifecycle.quit", "script", 1, 901, result="not-run", detail="no quit request", via="script"))
    # crash: begin marker in launch 1 with no result
    http.append(rec("tabs.create", "worker", 1, 500, result=None, detail=None, phase="begin"))
    # provisional fails, then final works
    http.append(rec("tabs.onCreated", "worker", 1, 600, result="fails", detail="not fired", final=False))
    http.append(rec("tabs.onCreated", "worker", 2, 2600, detail="fired"))
    # page says works, but the server saw the blocked request
    http.append(rec("dnr.static.block_fetch", "page", 2, 2700, detail="rejected"))
    reqs.append({"t": T + 2650, "launch": 2, "method": "GET", "path": "/__dnr/static-block.txt", "host": "127.0.0.1:1", "cookies": []})
    # present paths that the server checks need
    for p in ("/__dnr/control.txt", "/__dnr/redirect-target.txt", "/__ass/login"):
        reqs.append({"t": T + 2650, "launch": 2, "method": "GET", "path": p, "host": "127.0.0.1:1", "cookies": []})
    # one path only (cdp), and both paths
    cdp.append(rec("windows.get", "worker", 2, 2800, detail="cdp only", via="cdp"))
    both = rec("runtime.getManifest", "worker", 2, 2900, detail="both")
    http.append(dict(both, via="http", received=T + 2901))
    cdp.append(dict(both, via="cdp"))
    # result differs between launches
    http.append(rec("tabs.query.all", "worker", 1, 700, result="fails", detail="empty"))
    http.append(rec("tabs.query.all", "worker", 2, 2950, detail="list"))
    # two finals in one launch that disagree
    http.append(rec("tabs.get", "worker", 2, 3000, detail="first"))
    http.append(rec("tabs.get", "worker", 2, 3100, result="fails", detail="later"))
    # provisional only
    http.append(rec("alarms.onAlarm", "worker", 2, 3200, result="fails", detail="not fired", final=False))
    # cookie set, then a later request carries it
    http.append(rec("cookies.set", "worker", 2, 3300, detail="set", received=T + 3301))
    reqs.append({"t": T + 3400, "launch": 2, "method": "GET", "path": "/start.html", "host": "localhost:1", "cookies": ["ass_ext_cookie"]})
    # control records and one bad line
    http.append(rec("ctl.worker.boot", "worker", 1, 5, detail={"bootId": "a"}))
    # launch 3: the window probe ended the app during windows.create (not a crash)
    http.append(rec("ctl.worker.boot", "worker", 3, 3900, detail={"bootId": "c"}))
    http.append(rec("windows.create", "worker", 3, 4000, result=None, detail=None, phase="begin"))
    script.append(rec("lifecycle.no_browser_crash", "script", 3, 4100, result="not-run",
                      detail="the window probe ended the app", via="script"))
    script.append(rec("lifecycle.quit", "script", 3, 4101, result="not-run", detail="no quit request", via="script"))

    def write(name, objs, extra=""):
        with open(os.path.join(run, name), "w") as f:
            for o in objs:
                f.write(json.dumps(o) + "\n")
            f.write(extra)

    write("records.jsonl", http, "this is not json\n")
    write("cdp-records.jsonl", cdp)
    write("script.jsonl", script)
    write("requests.jsonl", reqs)
    write("logins.jsonl", [])
    write("launches.jsonl", [
        {"launch": 1, "pid": 100, "profile": "p", "started": 1, "ended": 2, "exit": None, "signal": 11, "quit_requested": False,
         "ended_before_quit": True, "quit_seconds": None, "helpers_left": 3, "tmp_left": 1},
        {"launch": 2, "pid": 101, "profile": "p", "started": 3, "ended": 4, "exit": 0, "signal": None, "quit_requested": True,
         "ended_before_quit": False, "quit_seconds": 1.2, "helpers_left": 0, "tmp_left": 0},
        {"launch": 3, "pid": 102, "profile": "p", "started": 5, "ended": 6, "exit": None, "signal": 9, "quit_requested": False,
         "ended_before_quit": True, "quit_seconds": None, "helpers_left": 0, "tmp_left": 0, "probe_killed": True,
         "probe_saw": "window-visible (1 violations); the window probe sent SIGKILL 31 ms after the first"}])

    a = analyze(run, checks_path)
    text = build_report(a)
    ids = plan_ids(a)
    print(text)
    print()
    ch = a["chosen"]
    line = dict((ln.split()[0], ln) for ln in text.split("\n") if ln and not ln.startswith(" "))
    tests = [
        ("tabs.create is crashes", ch["tabs.create"]["result"] == "crashes"),
        ("lifecycle.no_browser_crash shows the launch 1 crash", ch["lifecycle.no_browser_crash"]["result"] == "crashes"),
        ("lifecycle.quit shows launch 2's clean quit over launch 1's not-run",
         (ch["lifecycle.quit"]["result"], ch["lifecycle.quit"]["launch"]) == ("works", 2)),
        ("tabs.create detail names the signal", "signal 11" in flat(ch["tabs.create"]["detail"])),
        ("cs.document_idle.iframe_cross_origin is no-record", ch["cs.document_idle.iframe_cross_origin"]["result"] == "no-record"),
        ("tabs.onCreated is works from launch 2", (ch["tabs.onCreated"]["result"], ch["tabs.onCreated"]["launch"]) == ("works", 2)),
        ("dnr.static.block_fetch became fails", ch["dnr.static.block_fetch"]["result"] == "fails"),
        ("its detail says the server received it", "server received /__dnr/static-block.txt" in flat(ch["dnr.static.block_fetch"]["detail"])),
        ("dnr.static.block_script still works", ch["dnr.static.block_script"]["result"] == "works"),
        ("dnr.control_not_blocked works (path present)", ch["dnr.control_not_blocked"]["result"] == "works"),
        ("pm.submit.main_frame works (login seen)", ch["pm.submit.main_frame"]["result"] == "works"),
        ("windows.get VIA is cdp", " cdp " in line["windows.get"] and "http+cdp" not in line["windows.get"]),
        ("runtime.getManifest VIA is http+cdp", "http+cdp" in line["runtime.getManifest"]),
        ("load.secure_preferences is works via script", (ch["load.secure_preferences"]["result"], via_text(ch["load.secure_preferences"]["via"])) == ("works", "script")),
        ("tabs.query.all differs between launches", "tabs.query.all" in a["differs"]),
        ("tabs.get is a disagreement and keeps the latest", "tabs.get" in a["disagreements"] and ch["tabs.get"]["result"] == "fails"),
        ("cookies.set detail says the cookie was carried", "carried ass_ext_cookie: yes" in flat(ch["cookies.set"]["detail"])),
        ("alarms.onAlarm provisional stays fails", ch["alarms.onAlarm"]["result"] == "fails" and not ch["alarms.onAlarm"]["final"]),
        ("plan contains tabs.create", "tabs.create" in ids),
        ("plan does not contain cs.document_idle.iframe_cross_origin", "cs.document_idle.iframe_cross_origin" not in ids),
        ("plan does not contain the provisional alarms.onAlarm", "alarms.onAlarm" not in ids),
        ("plan holds only the checks that ended a launch", ids == ["tabs.create", "windows.create"]),
        ("windows.create fails, showed on screen", ch["windows.create"]["result"] == "fails"
         and "showed on screen" in flat(ch["windows.create"]["detail"]) and "window-visible" in flat(ch["windows.create"]["detail"])),
        ("the probe's launch is not a crash", all(e[0] != 3 for e in a["crash_events"])),
        ("report names the probe's launch", "launch 3, check in flight: windows.create; window-visible" in text),
        ("one unparseable line was counted", a["bad"] == 1),
        ("extension records present (exit code 0)", a["from_ext"]),
        ("report names the crash", "launch 1, exit -, signal 11, check in flight: tabs.create" in text),
    ]
    failed = 0
    for name, ok in tests:
        print(("PASS " if ok else "FAIL ") + name)
        failed += 0 if ok else 1
    if failed:
        print("kept: " + run)
    else:
        shutil.rmtree(run, ignore_errors=True)
    return 1 if failed else 0


# ---------- main ----------

def main(argv):
    if len(argv) < 2:
        print(__doc__)
        return 1
    cmd = argv[1]
    try:
        if cmd == "report" and len(argv) == 4:
            a = analyze(argv[2], argv[3])
            print(build_report(a))
            return 0 if a["from_ext"] else 3
        if cmd == "plan" and len(argv) == 4:
            print(json.dumps(plan_ids(analyze(argv[2], argv[3]))))
            return 0
        if cmd == "lint" and len(argv) >= 4:
            return cmd_lint(argv[2], argv[3:])
        if cmd == "selftest" and len(argv) == 3:
            return cmd_selftest(argv[2])
    except Exception as e:  # internal error
        print("internal error: %s: %s" % (type(e).__name__, e))
        return 1
    print(__doc__)
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
