"""Debug-port and run-directory helper for scripts/extension-test.sh.

Every subcommand prints one line and exits 0, or prints "TOOL ERROR: <message>"
and exits 2. Every debug-port connection is opened, used and closed inside one
subcommand, so nothing stays attached to the app: a DevTools session on a hung
page keeps quit from finishing (docs/ARCHITECTURE.md, known issue 10), and one
on the service worker keeps it from going idle.

Standard library only. Run with python3 -I.
"""

import base64, hashlib, http.client, json, os, signal, socket, struct, subprocess, sys, time
import urllib.request

# No proxy: the debug port is on 127.0.0.1, and a proxy from the environment or
# the system settings must never see it.
OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))
# Seconds one DevTools command may take, and one connection attempt.
CALL_SECONDS = 15
CONNECT_SECONDS = 5
POLL_SECONDS = 0.5
RESULTS = ("works", "fails", "crashes", "absent", "not-run")
LAUNCH_FIELDS = ("launch", "pid", "profile", "started", "ended", "exit", "signal", "quit_requested",
                 "ended_before_quit", "quit_seconds", "helpers_left", "tmp_left")
# Errors a listing can raise while the app starts, stops or is busy.
LISTING_ERRORS = (OSError, ValueError, http.client.HTTPException)


# ---- Processes ----

def alive(pid):
    """Whether the process exists and is not a zombie. The app is the run
    script's child, so it stays a zombie until the shell reaps it."""
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        pass
    state = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)],
                           capture_output=True, text=True).stdout.strip()
    return bool(state) and not state.startswith("Z")


def appkit_quit(pid):
    """Asks the app to quit through AppKit, as Cmd+Q does (-[NSApplication terminate:])."""
    script = ("ObjC.import('AppKit');"
              "var app = $.NSRunningApplication.runningApplicationWithProcessIdentifier(%d);"
              "app.isNil() ? 'no such app' : String(app.terminate)") % pid
    done = subprocess.run(["osascript", "-l", "JavaScript", "-e", script],
                          capture_output=True, text=True, timeout=30)
    answer = done.stdout.strip()
    if not answer:
        raise RuntimeError("osascript gave no answer: %s" % done.stderr.strip()[:200])
    return answer


# ---- Debug port ----

def get_json(port, path):
    with OPENER.open("http://127.0.0.1:%d%s" % (port, path), timeout=CONNECT_SECONDS) as reply:
        return json.load(reply)


def targets(port):
    listing = get_json(port, "/json/list")
    if not isinstance(listing, list):
        raise ValueError("/json/list did not return a list")
    return listing


def find_target(port, target_id):
    found = [t for t in targets(port) if t.get("id") == target_id]
    if not found:
        raise RuntimeError("no target %s on the debug port" % target_id)
    return found[0]


class WebSocket:
    """A minimal client: text frames, no extensions. No Origin header, because
    the debug port refuses origins it does not know."""

    def __init__(self, url):
        if not url.startswith("ws://"):
            raise RuntimeError("not a ws:// address: %s" % url)
        hostport, _, path = url[len("ws://"):].partition("/")
        host, _, port = hostport.partition(":")
        self.sock = socket.create_connection((host, int(port)), timeout=CONNECT_SECONDS)
        key = base64.b64encode(os.urandom(16)).decode()
        self.sock.sendall((
            "GET /%s HTTP/1.1\r\nHost: %s\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
            "Sec-WebSocket-Key: %s\r\nSec-WebSocket-Version: 13\r\n\r\n" % (path, hostport, key)
        ).encode())
        head = b""
        while b"\r\n\r\n" not in head:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise RuntimeError("the debug port closed the connection")
            head += chunk
        status = head.split(b"\r\n", 1)[0]
        if b" 101 " not in status:
            raise RuntimeError("the debug port refused: %r" % status)
        self.buffer = head.split(b"\r\n\r\n", 1)[1]

    def send(self, text):
        data = text.encode()
        size = len(data)
        if size < 126:
            head = struct.pack("!BB", 0x81, 0x80 | size)
        elif size < 65536:
            head = struct.pack("!BBH", 0x81, 0x80 | 126, size)
        else:
            head = struct.pack("!BBQ", 0x81, 0x80 | 127, size)
        mask = os.urandom(4)
        self.sock.sendall(head + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(data)))

    def read(self, size):
        while len(self.buffer) < size:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise RuntimeError("the debug port closed the connection")
            self.buffer += chunk
        data, self.buffer = self.buffer[:size], self.buffer[size:]
        return data

    def receive(self):
        message = b""
        while True:
            first, second = self.read(2)
            size = second & 0x7F
            if size == 126:
                size = struct.unpack("!H", self.read(2))[0]
            elif size == 127:
                size = struct.unpack("!Q", self.read(8))[0]
            payload = self.read(size)
            opcode = first & 0x0F
            if opcode == 8:
                raise RuntimeError("the debug port closed the session")
            if opcode in (0, 1, 2):
                message += payload
                if first & 0x80:
                    return message.decode()

    def close(self):
        self.sock.close()


class Session:
    """One DevTools session on one target. Through the target's own address when
    /json/list gives one; otherwise through the browser endpoint, attached with
    flatten and detached again on close."""

    def __init__(self, port, target):
        self.next_id = 0
        self.session_id = None
        url = target.get("webSocketDebuggerUrl")
        if url:
            self.ws = WebSocket(url)
            return
        self.ws = WebSocket(get_json(port, "/json/version")["webSocketDebuggerUrl"])
        try:
            self.session_id = self.call("Target.attachToTarget",
                                        {"targetId": target["id"], "flatten": True})["sessionId"]
        except BaseException:
            self.ws.close()
            raise

    def call(self, method, params=None):
        self.next_id += 1
        message = {"id": self.next_id, "method": method, "params": params or {}}
        if self.session_id and method != "Target.detachFromTarget":
            message["sessionId"] = self.session_id
        self.ws.send(json.dumps(message))
        deadline = time.time() + CALL_SECONDS
        while True:
            left = deadline - time.time()
            if left <= 0:
                raise RuntimeError("%s: no answer within %d s" % (method, CALL_SECONDS))
            self.ws.sock.settimeout(left)
            try:
                reply = json.loads(self.ws.receive())
            except socket.timeout:
                raise RuntimeError("%s: no answer within %d s" % (method, CALL_SECONDS))
            if reply.get("id") == self.next_id:
                if "error" in reply:
                    raise RuntimeError("%s: %s" % (method, reply["error"].get("message", reply["error"])))
                return reply.get("result", {})

    def evaluate(self, expression, gesture=False):
        result = self.call("Runtime.evaluate", {
            "expression": expression, "returnByValue": True, "userGesture": gesture})
        if "exceptionDetails" in result:
            details = result["exceptionDetails"]
            text = details.get("exception", {}).get("description") or details.get("text", "")
            raise RuntimeError("script threw: %s" % text.splitlines()[0][:200] if text else "script threw")
        return result.get("result", {}).get("value")

    def close(self):
        try:
            if self.session_id:
                self.call("Target.detachFromTarget", {"sessionId": self.session_id})
        finally:
            self.ws.close()


def with_session(port, target, work):
    session = Session(port, target)
    try:
        return work(session)
    finally:
        session.close()


# ---- Records ----

# localStorage keys starting with ass-records, as {key: raw value}.
PAGE_RECORDS = r"""(() => {
  const out = {};
  for (let i = 0; i < localStorage.length; i++) {
    const key = localStorage.key(i);
    if (key.startsWith('ass-records')) out[key] = localStorage.getItem(key);
  }
  return JSON.stringify(out);
})()"""
EXTENSION_PAGE_RECORDS = "JSON.stringify(window.__assRecords === undefined ? null : window.__assRecords)"
WORKER_RECORDS = "JSON.stringify(self.__assRecords === undefined ? null : self.__assRecords)"


def as_records(value):
    """A parsed value as (records, unreadable count): an array of records or one record."""
    items = value if isinstance(value, list) else [value]
    records = [item for item in items if isinstance(item, dict)]
    return records, len(items) - len(records)


def read_jsonl(path):
    """Every complete JSON line of a file; a half-written last line is skipped."""
    try:
        with open(path) as f:
            lines = f.readlines()
    except FileNotFoundError:
        return []
    out = []
    for line in lines:
        try:
            out.append(json.loads(line))
        except ValueError:
            continue
    return out


def append_jsonl(path, values):
    with open(path, "a") as f:
        for value in values:
            f.write(json.dumps(value, sort_keys=True) + "\n")


def is_result(record, check_id, launch):
    """A result for check_id in this launch: not a begin marker, not provisional."""
    return (isinstance(record, dict) and record.get("id") == check_id
            and str(record.get("launch")) == launch
            and record.get("phase") is None and record.get("final") is not False)


def has_result(run_dir, check_id, launch):
    for name in ("records.jsonl", "cdp-records.jsonl"):
        for line in read_jsonl(os.path.join(run_dir, name)):
            for record in (line if isinstance(line, list) else [line]):
                if is_result(record, check_id, launch):
                    return True
    return False


# ---- Subcommands ----

def ext_id(manifest_path):
    with open(manifest_path) as f:
        key = json.load(f)["key"]
    digest = hashlib.sha256(base64.b64decode(key, validate=True)).hexdigest()[:32]
    return "".join(chr(ord("a") + int(c, 16)) for c in digest)


def write_config(ext_dir, server, xserver, extension_id, launch, risky, skip_path):
    if risky not in ("0", "1"):
        raise RuntimeError("risky must be 0 or 1, got %s" % risky)
    with open(skip_path) as f:
        skip = json.load(f)
    if not isinstance(skip, list) or not all(isinstance(i, str) for i in skip):
        raise RuntimeError("%s is not a JSON array of check ids" % skip_path)
    fields = [("server", server), ("xserver", xserver), ("extensionId", extension_id),
              ("launch", int(launch)), ("risky", risky == "1"), ("skip", skip)]
    text = ("// Written by scripts/extension-test.sh for launch %d.\nself.ASS_TEST = {%s};\n"
            % (int(launch), ", ".join("%s: %s" % (k, json.dumps(v)) for k, v in fields)))
    with open(os.path.join(ext_dir, "config.js"), "w") as f:
        f.write(text)
    return "config.js written: launch %d, risky %s, %d skipped" % (int(launch), risky == "1", len(skip))


def wait_target(port, kind, prefix, seconds, pid):
    deadline = time.time() + seconds
    while True:
        try:
            for target in targets(port):
                if target.get("type") == kind and target.get("url", "").startswith(prefix):
                    return target["id"]
        except LISTING_ERRORS:  # not listening yet, or busy
            pass
        if not alive(pid):
            return "app-exited"
        if time.time() >= deadline:
            return "none"
        time.sleep(POLL_SECONDS)


def wait_gone(port, kind, prefix, seconds, pid):
    start = time.time()
    while True:
        # Only a listing that answered counts: a refused or slow one proves nothing.
        try:
            listed = any(t.get("type") == kind and t.get("url", "").startswith(prefix)
                         for t in targets(port))
            if not listed:
                return "%.1f" % (time.time() - start)
        except LISTING_ERRORS:
            pass
        if not alive(pid):
            return "app-exited"
        if time.time() - start >= seconds:
            return "still-there"
        time.sleep(POLL_SECONDS)


def wait_record(run_dir, check_id, launch, seconds, pid):
    deadline = time.time() + seconds
    while True:
        if has_result(run_dir, check_id, launch):
            return "found"
        if not alive(pid):
            # A record may have landed between the read and the exit.
            return "found" if has_result(run_dir, check_id, launch) else "app-exited"
        if time.time() >= deadline:
            return "timeout"
        time.sleep(POLL_SECONDS)


def navigate(port, target_id, url):
    result = with_session(port, find_target(port, target_id),
                          lambda s: s.call("Page.navigate", {"url": url}))
    return result.get("errorText") or "ok"


def open_victim(port, target_id, url):
    expression = ("window.open(%s, 'ass_victim', 'popup,width=420,height=320') ? 'opened' : 'blocked'"
                  % json.dumps(url))
    return str(with_session(port, find_target(port, target_id),
                            lambda s: s.evaluate(expression, gesture=True)))


def snapshot(port, run_dir, launch, extension_id, content_id, worker):
    """Path 2: the records the pages and the worker hold in memory or storage."""
    listing = targets(port)
    path = os.path.join(run_dir, "cdp-records.jsonl")
    saved = {json.dumps(r, sort_keys=True) for r in read_jsonl(path) if isinstance(r, dict)}
    parts = []

    def take(label, records, unreadable):
        fresh, other = [], 0
        for record in records:
            record = dict(record, via="cdp")
            if str(record.get("launch")) != launch:
                other += 1
            line = json.dumps(record, sort_keys=True)
            if line not in saved:
                saved.add(line)
                fresh.append(record)
        append_jsonl(path, fresh)
        note = "".join([", %d from another launch" % other if other else "",
                        ", %d unreadable" % unreadable if unreadable else ""])
        parts.append("%s %d records, %d new%s" % (label, len(records), len(fresh), note))

    if content_id != "-":
        try:
            found = [t for t in listing if t.get("id") == content_id]
            if not found:
                raise RuntimeError("no target %s" % content_id)
            url = found[0].get("url", "")
            if url.startswith(("http://", "https://")):
                raw = json.loads(with_session(port, found[0], lambda s: s.evaluate(PAGE_RECORDS)))
                records, unreadable = [], 0
                for value in raw.values():
                    try:
                        more, bad = as_records(json.loads(value))
                    except (TypeError, ValueError):
                        more, bad = [], 1
                    records += more
                    unreadable += bad
                take("page (%d keys)" % len(raw), records, unreadable)
            elif url.startswith("chrome-extension://"):
                raw = json.loads(with_session(port, found[0], lambda s: s.evaluate(EXTENSION_PAGE_RECORDS)))
                if raw is None:
                    parts.append("extension page: no window.__assRecords")
                else:
                    take("extension page", *as_records(raw))
            else:
                parts.append("page: not read, its URL is %s" % url[:80])
        except Exception as error:  # one source failing must not hide the other
            parts.append("page: %s" % error)

    if worker == "1":
        prefix = "chrome-extension://%s/" % extension_id
        workers = [t for t in listing
                   if t.get("type") == "service_worker" and t.get("url", "").startswith(prefix)]
        if not workers:
            # Never started by this snapshot: listing a target does not start it.
            parts.append("worker: no service_worker target")
        for target in workers:
            try:
                via = "" if target.get("webSocketDebuggerUrl") else " (through the browser endpoint)"
                raw = json.loads(with_session(port, target, lambda s: s.evaluate(WORKER_RECORDS)))
                if raw is None:
                    parts.append("worker%s: no self.__assRecords" % via)
                else:
                    take("worker%s" % via, *as_records(raw))
            except Exception as error:
                parts.append("worker: %s" % error)

    return "; ".join(parts) or "nothing to read"


def note(run_dir, launch, check_id, group, result, detail, provisional=None):
    if result not in RESULTS:
        raise RuntimeError("result must be one of %s, got %s" % ("|".join(RESULTS), result))
    if provisional not in (None, "provisional"):
        raise RuntimeError("the last argument may only be 'provisional'")
    record = {"id": check_id, "group": group, "result": result, "detail": detail,
              "by": "script", "via": "script", "launch": int(launch), "t": int(time.time() * 1000)}
    if provisional:
        record["final"] = False
    append_jsonl(os.path.join(run_dir, "script.jsonl"), [record])
    return "noted %s %s%s" % (check_id, result, " (provisional)" if provisional else "")


def secure_prefs(profile_dir, extension_id):
    path = os.path.join(profile_dir, "Default", "Secure Preferences")
    try:
        with open(path) as f:
            prefs = json.load(f)
    except FileNotFoundError:
        return "missing: no file %s" % path
    except ValueError as error:
        return "missing: %s is not JSON (%s)" % (path, error)
    settings = prefs.get("extensions", {}).get("settings")
    if not isinstance(settings, dict):
        return "missing: no extensions.settings in %s" % path
    entry = settings.get(extension_id)
    if not isinstance(entry, dict):
        return "missing: %s not in extensions.settings (%d ids there)" % (extension_id, len(settings))
    return "location=%s path=%s" % (entry.get("location"), entry.get("path"))


def launch_end(run_dir, text):
    value = json.loads(text)
    if not isinstance(value, dict):
        raise RuntimeError("not a JSON object")
    missing = [k for k in LAUNCH_FIELDS if k not in value]
    if missing:
        raise RuntimeError("missing fields: %s" % ", ".join(missing))
    append_jsonl(os.path.join(run_dir, "launches.jsonl"), [value])
    return "launch %s recorded" % value["launch"]


# name: (usage, argument count, the most seconds it may take, the function).
# A time of (i, m) means the value of argument i plus m seconds.
COMMANDS = {
    "ext-id": ("<manifest.json>", 1, 10, lambda a: ext_id(a[0])),
    "write-config": ("<ext_dir> <server> <xserver> <ext_id> <launch> <risky 0|1> <skip.json>", 7, 10,
                     lambda a: write_config(*a)),
    "targets": ("<port>", 1, 15,
                lambda a: "\n".join("%s\t%s\t%s" % (t.get("type"), t.get("id"), t.get("url"))
                                    for t in targets(int(a[0])))),
    "wait-target": ("<port> <type> <url_prefix> <seconds> <pid>", 5, (3, 15),
                    lambda a: wait_target(int(a[0]), a[1], a[2], float(a[3]), int(a[4]))),
    "wait-gone": ("<port> <type> <url_prefix> <seconds> <pid>", 5, (3, 15),
                  lambda a: wait_gone(int(a[0]), a[1], a[2], float(a[3]), int(a[4]))),
    "wait-record": ("<run_dir> <id> <launch> <seconds> <pid>", 5, (3, 15),
                    lambda a: wait_record(a[0], a[1], str(int(a[2])), float(a[3]), int(a[4]))),
    "navigate": ("<port> <target_id> <url>", 3, 45, lambda a: navigate(int(a[0]), a[1], a[2])),
    "open-victim": ("<port> <target_id> <url>", 3, 45, lambda a: open_victim(int(a[0]), a[1], a[2])),
    "snapshot": ("<port> <run_dir> <launch> <ext_id> <content_target_id|-> <worker 0|1>", 6, 120,
                 lambda a: snapshot(int(a[0]), a[1], str(int(a[2])), a[3], a[4], a[5])),
    "note": ("<run_dir> <launch> <id> <group> <result> <detail> [provisional]", (6, 7), 10,
             lambda a: note(*a)),
    "secure-prefs": ("<profile_dir> <ext_id>", 2, 10, lambda a: secure_prefs(a[0], a[1])),
    "quit": ("<pid>", 1, 40, lambda a: appkit_quit(int(a[0]))),
    "launch-end": ("<run_dir> <json>", 2, 10, lambda a: launch_end(a[0], a[1])),
}


def on_alarm(signum, frame):
    raise RuntimeError("the subcommand did not finish in time")


def main(args):
    if not args or args[0] not in COMMANDS:
        raise RuntimeError("unknown subcommand; one of: %s" % ", ".join(COMMANDS))
    usage, count, budget, run = COMMANDS[args[0]]
    rest = args[1:]
    if len(rest) not in (count if isinstance(count, tuple) else (count,)):
        raise RuntimeError("usage: %s %s" % (args[0], usage))
    # A backstop, so no subcommand can hang the run script.
    if isinstance(budget, tuple):
        budget = int(float(rest[budget[0]])) + budget[1]
    signal.signal(signal.SIGALRM, on_alarm)
    signal.alarm(budget)
    print(run(rest))
    signal.alarm(0)


if __name__ == "__main__":
    try:
        main(sys.argv[1:])
    except KeyboardInterrupt:
        print("TOOL ERROR: interrupted")
        sys.exit(2)
    except Exception as error:
        # One line on stdout, so a failed call can never look like a result.
        print("TOOL ERROR: %s" % str(error).replace("\n", " "))
        sys.exit(2)
