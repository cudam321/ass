#!/usr/bin/env python3
"""Local page server for the extension test run (tests/extension/checks.json).

Usage: python3 -I scripts/extension-test-server.py <pages_dir> <run_dir>

Binds 127.0.0.1 on an ephemeral port and writes the port to <run_dir>/server.port.
Runs until SIGTERM or SIGINT, then exits 0. Standard library only.
"""
import json
import os
import signal
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, unquote, urlsplit

MAX_BODY = 256 * 1024

TYPES = {
    ".html": "text/html; charset=utf-8",
    ".js": "text/javascript; charset=utf-8",
    ".css": "text/css; charset=utf-8",
    ".json": "application/json",
    ".txt": "text/plain; charset=utf-8",
}

# Fixed bodies of the /__dnr/* paths.
DNR = {
    "/__dnr/control.txt": ("control", "text/plain; charset=utf-8"),
    "/__dnr/static-block.txt": ("not-blocked", "text/plain; charset=utf-8"),
    "/__dnr/static-block.js": ("window.assStaticScriptRan = true;", "text/javascript; charset=utf-8"),
    "/__dnr/static-redirect-src.txt": ("not-redirected", "text/plain; charset=utf-8"),
    "/__dnr/redirect-target.txt": ("redirect-target", "text/plain; charset=utf-8"),
    "/__dnr/dynamic-block.txt": ("not-blocked", "text/plain; charset=utf-8"),
}

PAGES = ""
RUN = ""
LOCK = threading.Lock()  # one lock for every file append and the index
SEEN = {}  # (id, launch) -> how many result records (no phase) arrived so far


def now_ms():
    return int(time.time() * 1000)


def current_launch():
    try:
        with open(os.path.join(RUN, "launch")) as f:
            return int(f.read().strip())
    except (OSError, ValueError):
        return 0


def as_int(v):
    try:
        return int(v)
    except (TypeError, ValueError):
        return None


def append_lines(name, objs):
    data = "".join(json.dumps(o, separators=(",", ":")) + "\n" for o in objs)
    with open(os.path.join(RUN, name), "a") as f:
        f.write(data)


def load_index():
    try:
        with open(os.path.join(RUN, "records.jsonl")) as f:
            for line in f:
                try:
                    r = json.loads(line)
                except ValueError:
                    continue
                if isinstance(r, dict) and "phase" not in r and "id" in r:
                    key = (r["id"], as_int(r.get("launch")))
                    SEEN[key] = SEEN.get(key, 0) + 1
    except OSError:
        pass


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "AssExtTest"

    def log_message(self, *args):
        pass

    # ---- helpers ----
    def send(self, code, body=b"", ctype=None, extra=()):
        if isinstance(body, str):
            body = body.encode("utf-8")
        self.send_response(code)
        self.send_header("Cache-Control", "no-store")
        if self.is_ass:
            self.send_header("Access-Control-Allow-Origin", "*")
        for k, v in extra:
            self.send_header(k, v)
        if code != 204:
            self.send_header("Content-Type", ctype or "text/plain; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if code != 204 and self.command != "HEAD":
            self.wfile.write(body)

    def send_json(self, obj, code=200):
        self.send(code, json.dumps(obj), "application/json")

    def begin(self):
        parts = urlsplit(self.path)
        self.url_path = unquote(parts.path)
        self.query = parse_qs(parts.query)
        self.is_ass = self.url_path.startswith("/__ass/")
        self.launch = current_launch()
        cookie = self.headers.get("Cookie", "")
        names = []
        for piece in cookie.split(";"):
            name = piece.split("=", 1)[0].strip()
            if name:
                names.append(name)
        entry = {
            "t": now_ms(),
            "launch": self.launch,
            "method": self.command,
            "path": self.url_path,
            "host": self.headers.get("Host", ""),
            "cookies": names,
        }
        with LOCK:
            append_lines("requests.jsonl", [entry])

    def read_body(self):
        """Returns the body bytes, or None after answering 400 for a body that is too big."""
        n = as_int(self.headers.get("Content-Length")) or 0
        if n > MAX_BODY or n < 0:
            self.close_connection = True
            self.send(400, "body too large", extra=(("Connection", "close"),))
            return None
        return self.rfile.read(n) if n else b""

    # ---- methods ----
    def do_OPTIONS(self):
        self.begin()
        if not self.is_ass:
            return self.send(404, "not found")
        self.send(204, extra=(
            ("Access-Control-Allow-Methods", "GET, POST"),
            ("Access-Control-Allow-Headers", "Content-Type"),
        ))

    def do_GET(self):
        self.begin()
        p = self.url_path
        if p == "/__ass/launch":
            return self.send_json({"launch": self.launch})
        if p in ("/__ass/has", "/__ass/count"):
            ident = (self.query.get("id") or [""])[0]
            with LOCK:
                n = SEEN.get((ident, self.launch), 0)
            if p == "/__ass/count":
                return self.send_json({"count": n})
            return self.send_json({"has": n > 0})
        if p in DNR:
            body, ctype = DNR[p]
            return self.send(200, body, ctype)
        if self.is_ass or p.startswith("/__dnr/"):
            return self.send(404, "not found")
        self.serve_file(p)

    def do_HEAD(self):
        self.do_GET()

    def serve_file(self, p):
        root = os.path.realpath(PAGES)
        full = os.path.realpath(os.path.join(root, p.lstrip("/")))
        if "\0" in p or not full.startswith(root + os.sep) or not os.path.isfile(full):
            return self.send(404, "not found")
        try:
            with open(full, "rb") as f:
                body = f.read()
        except OSError:
            return self.send(404, "not found")
        ctype = TYPES.get(os.path.splitext(full)[1].lower(), "application/octet-stream")
        extra = ()
        if p == "/start.html":
            extra = (("Set-Cookie", "ass_server_cookie=1; Path=/; SameSite=Lax"),)
        self.send(200, body, ctype, extra)

    def do_POST(self):
        self.begin()
        if self.url_path not in ("/__ass/record", "/__ass/login"):
            return self.send(404, "not found")
        raw = self.read_body()
        if raw is None:
            return
        try:
            data = json.loads(raw.decode("utf-8"))
        except ValueError:
            return self.send(400, "invalid json")
        if self.url_path == "/__ass/login":
            if not isinstance(data, dict):
                return self.send(400, "invalid json")
            user = data.get("user")
            with LOCK:
                append_lines("logins.jsonl", [{"t": now_ms(), "launch": self.launch, "user": user}])
            return self.send_json({"ok": True, "user": user})
        records = data if isinstance(data, list) else [data]
        if not all(isinstance(r, dict) for r in records):
            return self.send(400, "records must be objects")
        received = now_ms()
        for r in records:
            r["via"] = "http"
            r["received"] = received
        with LOCK:
            append_lines("records.jsonl", records)
            for r in records:
                if "phase" not in r and "id" in r:
                    key = (r["id"], as_int(r.get("launch")))
                    SEEN[key] = SEEN.get(key, 0) + 1
        self.send(204)


def main():
    global PAGES, RUN
    if len(sys.argv) != 3:
        print("usage: extension-test-server.py <pages_dir> <run_dir>", file=sys.stderr)
        return 2
    PAGES, RUN = sys.argv[1], sys.argv[2]
    os.makedirs(RUN, exist_ok=True)
    load_index()
    srv = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    srv.daemon_threads = True
    port = srv.server_address[1]
    tmp = os.path.join(RUN, "server.port.tmp")
    with open(tmp, "w") as f:
        f.write(str(port))
    os.replace(tmp, os.path.join(RUN, "server.port"))

    def stop(signum, frame):
        # shutdown() waits for the serve loop, so it must run on another thread
        threading.Thread(target=srv.shutdown, daemon=True).start()

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    srv.serve_forever()
    srv.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
