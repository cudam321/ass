#!/bin/sh
# Quit with the network service helper stalled, and the cookie flush at the quit
# (known issue 2; src/flush.rs, src/leftover.rs). One launch, and
# a second one with --restart or --during=launch.
#
#   scripts/quit-stall-test.sh [debug|release] [--fixed] [--restart] [--no-stop]
#       [--cookie] [--hold=SECONDS] [--cont-after=SECONDS] [--during=quit|launch]
#
# The app flushes the cookie store once no browser is left and waits up to 2 s
# for the answer, printing one line ("answered in N ms" or "gave no answer within
# 2000 ms", each with the clock time); then shutdown() starts Chromium's teardown
# watchdog, 10 s (chrome/browser/shutdown_watchdog_mac.cc), and its UI thread
# waits up to 10 s for the network service helper to exit
# (browser_process_io_thread.cc). This test stops that helper (SIGSTOP), so it
# can neither answer nor exit, asks the app to quit through AppKit by PID and
# waits up to 20 s for the app to end. It records: seconds from the quit request
# to the end; the exit code; whether stderr has "Teardown watchdog expired"; the
# flush line(s) and when the script saw each; where the browser was during the
# stall (see "READING"); which helpers are left 3 s after the end, with what each
# holds open under the profile; whether this launch's socket folder and bundle
# clone are left. Then it ends every helper of this run that is left (the
# stopped one gets SIGCONT first), with its ps line and open files written to
# the run folder first (nothing that attaches to a process, which can raise a
# system prompt).
#
#   --restart      then a second start on the same profile while the stopped
#                  helper is still there: the app should end it at start
#                  ("Ass: ended ..."), load its page and quit with exit code 0.
#   --fixed        what this build is expected to do, enforced: the no-answer
#                  line 2 to 3 s after the request, exit code 2 with the
#                  watchdog's line 9.5 to 12 s after it, then the restart as
#                  above (--fixed includes --restart).
#   --no-stop      the network helper is not stopped: a plain quit.
#   --cookie       the first page is served by this script on 127.0.0.1 and sets
#                  a persistent cookie. The profile's cookie files are stated when
#                  the page has loaded, at the quit request, the moment the flush
#                  line appears (and copied then), after the end, and after the
#                  stopped helper is ended; each copy is read with the system's
#                  sqlite3, opened immutable, for that cookie's row.
#   --hold=S       waits S seconds after the page loaded before the quit, and
#                  lists the profile's files written in that time.
#   --cont-after=S sends SIGCONT to the stopped helper S seconds after the quit
#                  request was begun, if it is still the app's child.
#   --during=quit  a second quit request through AppKit, as soon as the
#                  debugging port lists no page (at most 1.5 s after the first).
#   --during=launch a second start on the same profile at that moment, with its
#                  own debugging port; it must hand off and exit 0. A window of
#                  the second process stops the run (exit code 1).
#
# Exit code 0 once the run was made and recorded, except with --fixed, which
# enforces its list (1 otherwise). Exit code 1 when the run could not be made,
# when the probe saw the app take the screen or the keyboard, or when the second
# launch of --during=launch showed a window.
#
# Same rules as scripts/smoke.sh: test window mode, a temporary profile and HOME,
# --use-mock-keychain, its own debugging port (ASS_QUIT_STALL_PORT, default 9345;
# the second launch takes the next one) and MAC_CHROMIUM_TMPDIR, the window
# probe with --kill, a quit through AppKit by PID, and cleanup on every exit.
# The only processes ever signalled are those this script started, and helpers
# recorded while its app lived whose command line carries this run's profile
# folder. The logs stay in the run folder, which is printed; the profile and
# HOME are deleted.

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WHICH=debug FIXED=0 RESTART=0 NO_STOP=0 COOKIE=0 HOLD= CONT_AFTER= DURING=
SECONDS_RE='^[0-9]+(\.[0-9]+)?$'
usage() {
  echo "usage: scripts/quit-stall-test.sh [debug|release] [--fixed] [--restart] [--no-stop] [--cookie]" \
    "[--hold=SECONDS] [--cont-after=SECONDS] [--during=quit|launch]"
  exit 2
}
for arg in "$@"; do
  case "$arg" in
    --fixed) FIXED=1 RESTART=1 ;;
    # A second start on the same profile
    # while the stopped helper is still there; see "RESTART" below.
    --restart) RESTART=1 ;;
    --no-stop) NO_STOP=1 ;;
    --cookie) COOKIE=1 ;;
    --hold=*) HOLD=${arg#--hold=} ;;
    --cont-after=*) CONT_AFTER=${arg#--cont-after=} ;;
    --during=quit|--during=launch) DURING=${arg#--during=} ;;
    debug|release) WHICH=$arg ;;
    *) usage ;;
  esac
done
for value in $HOLD $CONT_AFTER; do echo "$value" | grep -q -E "$SECONDS_RE" || usage; done
# Combinations that would test nothing, or two things at once.
[ "$NO_STOP" = 1 ] && { [ "$RESTART" = 1 ] || [ -n "$CONT_AFTER" ] || [ -n "$DURING" ]; } && usage
[ -n "$HOLD" ] && [ "$COOKIE" = 0 ] && usage
BIN="$ROOT/target/$WHICH/app.noindex/Ass.app/Contents/MacOS/Ass"
PORT="${ASS_QUIT_STALL_PORT:-9345}"
SECOND_PORT=$((PORT + 1))
PROBE="$ROOT/target/appprobe"

await() { n=0; while kill -0 "$1" 2>/dev/null && [ $n -lt "$2" ]; do n=$((n + 1)); sleep 0.5; done; }
ours() { [ -n "$1" ] && [ "$(ps -o ppid= -p "$1" 2>/dev/null | tr -d ' ')" = "$$" ]; }
now() { perl -MTime::HiRes=time -e 'printf "%.3f\n", time'; }
appkit_quit() {
  osascript -l JavaScript -e "ObjC.import('AppKit');
    var app = \$.NSRunningApplication.runningApplicationWithProcessIdentifier($1);
    app.isNil() ? 'no such app' : String(app.terminate)" 2>&1
}
not_run() { echo "NOT RUN: $*"; exit 1; }

# this_run PID: true for a PID in $HELPERS (the app's children recorded while it
# lived) whose command line still carries this run's profile folder, so a PID
# reused since is never signalled.
recorded() { case " $(echo $HELPERS) " in *" $1 "*) return 0 ;; esac; return 1; }
carries_profile() {
  case "$(ps -ww -o command= -p "$1" 2>/dev/null) " in
    *"--user-data-dir=$PROFILE "*|*"--user-data-dir=$PROFILE_REAL "*) return 0 ;;
  esac
  return 1
}
this_run() { recorded "$1" && carries_profile "$1"; }

# record_helper PID FILE: its ps line and open files (lsof, no name lookups) into
# FILE; prints which files under the profile it holds open, with lsof's FD column
# (u: read and write, r: read, w: write).
record_helper() {
  { date; ps -ww -o pid,ppid,state,pcpu,etime,command -p "$1"; lsof -n -P -p "$1"; } > "$2" 2>&1
  held=$(awk -v a="$PROFILE/" -v b="$PROFILE_REAL/" 'NR > 1 {
      name = $9; for (i = 10; i <= NF; i++) name = name " " $i
      if (index(name, a) == 1) print substr(name, length(a) + 1) " (" $4 ")"
      else if (index(name, b) == 1) print substr(name, length(b) + 1) " (" $4 ")"
    }' "$2" | sort -u | paste -s -d ',' - | sed 's/,/, /g')
  echo "holds under the profile: ${held:-nothing}"
}

# end_helper PID: as smoke.sh's end_leftover, for a PID this_run accepts. Writes
# its ps line and open files to the run folder, then SIGCONT, SIGTERM, and
# SIGKILL after 5 s.
end_helper() {
  kill -0 "$1" 2>/dev/null || { echo "pid $1: already ended"; return; }
  recorded "$1" || { echo "pid $1: not recorded while the app lived, left alone"; return; }
  carries_profile "$1" || { echo "pid $1: its command line does not carry this run's profile folder, left alone"; return; }
  holds=$(record_helper "$1" "$RUN/leftover-$1.txt")
  kill -CONT "$1" 2>/dev/null
  kill -TERM "$1" 2>/dev/null && await "$1" 10
  if kill -0 "$1" 2>/dev/null; then
    kill -KILL "$1" 2>/dev/null
    echo "pid $1: ended by SIGKILL; $holds; what it was doing: $RUN/leftover-$1.txt"
  else
    echo "pid $1: ended by SIGTERM; $holds; what it was doing: $RUN/leftover-$1.txt"
  fi
}

# cookies LABEL: with --cookie, the profile's cookie files (every file whose name
# has "Cookies" in it): each one's last change and size, copied to
# $RUN/cookies-LABEL/, and the copy of each database read with the system's
# sqlite3, opened immutable, for the test cookie's row (its value is encrypted;
# the host and name are not). One line out, and $RUN/cookies-LABEL.json.
cookies() {
  [ "$COOKIE" = 1 ] || return 0
  python3 -I - "$PROFILE" "$RUN" "$1" "${COOKIE_AT:-0}" <<'EOF'
import json, os, shutil, subprocess, sys, time
profile, run, label, since = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4])
t = time.time()
dest = os.path.join(run, "cookies-" + label)
files = {}
for folder, _, names in os.walk(profile):
    for name in names:
        if "Cookies" in name:
            path = os.path.join(folder, name)
            rel = os.path.relpath(path, profile)
            try:
                st = os.stat(path)
                os.makedirs(os.path.dirname(os.path.join(dest, rel)), exist_ok=True)
                shutil.copy2(path, os.path.join(dest, rel))
            except OSError:
                continue
            files[rel] = [st.st_mtime, st.st_size]
rows = {}
for rel in files:
    if rel.endswith("Cookies"):
        copy = os.path.join(dest, rel)
        uri = copy if os.path.exists(copy + "-wal") else "file:%s?immutable=1" % copy
        done = subprocess.run(["/usr/bin/sqlite3", "-readonly", uri,
                               "select host_key, name from cookies where name = 'ass_flush'"],
                              capture_output=True, text=True)
        rows[rel] = done.stdout.strip() or ("absent" if done.returncode == 0 else "error: " + done.stderr.strip()[:200])
json.dump({"t": t, "files": files, "rows": rows}, open(dest + ".json", "w"))
since_text = lambda m: ("%+.3f s from the cookie" % (m - since)) if since else "%.3f" % m
print("  cookie files %s: %s; the test cookie's row: %s" % (
    label, "; ".join("%s changed %s, %d bytes" % (rel, since_text(m), size) for rel, (m, size) in sorted(files.items()))
    or "none", "; ".join("%s %s" % kv for kv in sorted(rows.items())) or "no database"))
EOF
}

# On any exit: the stopped helper is let go first, so an app caught waiting for
# it can quit; then the second launch is ended, the app is stopped (AppKit,
# SIGTERM, SIGKILL), the helpers of this run that are left are ended, the guard,
# the probe and the cookie server stop, and the profile, HOME and Chromium's
# temporary folder are deleted. Runs once.
PID= SECOND_PID= PROBE_PID= GUARD_PID= TIMER_PID= SERVER_PID= NET= HELPERS= RUN= PROFILE= PROFILE_REAL= MADE_TMP= CLEANED=0
cleanup() {
  status=$?
  [ "$CLEANED" = 1 ] && return
  CLEANED=1
  trap '' INT TERM HUP
  ours "$GUARD_PID" && kill -TERM "$GUARD_PID"
  ours "$TIMER_PID" && kill -TERM "$TIMER_PID"
  ours "$SECOND_PID" && kill -KILL "$SECOND_PID"
  [ -n "$NET" ] && this_run "$NET" && kill -CONT "$NET"
  if ours "$PID"; then
    appkit_quit "$PID" > /dev/null
    await "$PID" 30
    ours "$PID" && kill -TERM "$PID" && await "$PID" 20
    ours "$PID" && kill -KILL "$PID"
    echo "quit stall test: stopped the app"
  fi
  for helper in $HELPERS; do
    kill -0 "$helper" 2>/dev/null && await "$helper" 6
    kill -0 "$helper" 2>/dev/null && echo "quit stall test: a helper outlived the app: $(end_helper "$helper")"
  done
  ours "$PROBE_PID" && kill -TERM "$PROBE_PID" && await "$PROBE_PID" 10
  ours "$SERVER_PID" && kill -TERM "$SERVER_PID"
  [ -n "$MADE_TMP" ] && rm -rf "$MADE_TMP"
  [ -n "$RUN" ] && rm -rf "$RUN/profile" "$RUN/home" && echo "quit stall test: logs in $RUN"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

[ -x "$BIN" ] || not_run "$BIN not found. Run: cargo run --bin bundle"
# A binary without test window mode would open a normal window: never launched.
ASS_TEST_WINDOW=invisible
grep -q -a -F "Ass: test window mode: $ASS_TEST_WINDOW" "$BIN" || not_run "$BIN has no test window mode"
if [ ! -x "$PROBE" ] || [ "$ROOT/scripts/appprobe.swift" -nt "$PROBE" ]; then
  swiftc -O -o "$PROBE" "$ROOT/scripts/appprobe.swift" || not_run "cannot build $PROBE"
fi
curl -s -m 2 "http://127.0.0.1:$PORT/json/version" > /dev/null 2>&1 && not_run "something already answers on port $PORT"
if [ "$DURING" = launch ]; then
  curl -s -m 2 "http://127.0.0.1:$SECOND_PORT/json/version" > /dev/null 2>&1 && not_run "something already answers on port $SECOND_PORT"
fi

RUN="$(mktemp -d -t ass-quit-stall)"
PROFILE="$RUN/profile"
mkdir "$PROFILE" "$RUN/home"
PROFILE_REAL="$(cd "$PROFILE" && pwd -P)"
# Holds the single-instance socket, whose path has at most 104 bytes (smoke.sh).
MADE_TMP="$(mktemp -d /tmp/ass-stall.XXXXXX)"
MAC_CHROMIUM_TMPDIR="$MADE_TMP"
ASS_PROFILE_DIR="$PROFILE"
export ASS_PROFILE_DIR ASS_TEST_WINDOW MAC_CHROMIUM_TMPDIR
LOG="$RUN/app.log"
PROBE_LOG="$RUN/probe.jsonl"

# Chromium clones the app bundle at each launch into the X folder beside the
# user's temporary folder, not into MAC_CHROMIUM_TMPDIR, and has a helper delete
# the clone after a clean quit (chrome/browser/mac/code_sign_clone_manager.mm,
# GetCloneTempDir and ~CodeSignCloneManager). The folder that appears while the
# app runs is this launch's; it is reported, never deleted. Another launch of
# the app during the run would make a second one, which is printed too.
CLONE_DIR="$(dirname "$(getconf DARWIN_USER_TEMP_DIR)")/X/dev.ass.browser.code_sign_clone"
clones() { ls -d "$CLONE_DIR"/code_sign_clone.* 2>/dev/null | sort; }
clones > "$RUN/clones-before.txt"

# With --cookie: a page on 127.0.0.1 that sets a persistent cookie (a session
# cookie never reaches the file). Only this machine; each request is logged.
URL='data:text/html,<title>ass-quit-stall</title>'
if [ "$COOKIE" = 1 ]; then
  python3 -I - "$RUN/server" <<'EOF' > "$RUN/server.log" 2>&1 &
import http.server, os, sys, time
base = sys.argv[1]
class Page(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = b"<!doctype html><title>ass-quit-stall</title><body>cookie"
        self.send_response(200 if self.path == "/cookie" else 404)
        if self.path == "/cookie":
            self.send_header("Set-Cookie", "ass_flush=1; Max-Age=86400; Path=/; SameSite=Lax")
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
        with open(base + ".hits", "a") as hits:
            hits.write("%.3f %s\n" % (time.time(), self.path))
    def log_message(self, *args):
        pass
server = http.server.HTTPServer(("127.0.0.1", 0), Page)
with open(base + ".port.tmp", "w") as f:
    f.write(str(server.server_address[1]))
os.rename(base + ".port.tmp", base + ".port")
server.serve_forever()
EOF
  SERVER_PID=$!
  n=0
  until [ -s "$RUN/server.port" ] || [ $n -ge 20 ]; do n=$((n + 1)); sleep 0.25; done
  [ -s "$RUN/server.port" ] || not_run "the cookie page's server did not start: $(cat "$RUN/server.log")"
  URL="http://127.0.0.1:$(cat "$RUN/server.port")/cookie"
fi

"$PROBE" --binary "$BIN" --kill > "$PROBE_LOG" 2>&1 &
PROBE_PID=$!
n=0
until grep -q '"k":"ready"' "$PROBE_LOG" 2>/dev/null || [ $n -ge 20 ]; do n=$((n + 1)); sleep 0.5; done
[ $n -lt 20 ] || not_run "the probe did not start; the app was not launched"

echo "quit stall test: $BIN"
echo "  network helper: $([ "$NO_STOP" = 1 ] && echo "not stopped" || echo stopped)$([ "$COOKIE" = 1 ] && echo "; cookie page $URL")$([ -n "$HOLD" ] && echo "; hold $HOLD s")$([ -n "$CONT_AFTER" ] && echo "; SIGCONT $CONT_AFTER s after the request")$([ -n "$DURING" ] && echo "; during the flush: a second $DURING")$([ "$RESTART" = 1 ] && echo "; then a restart")"
echo "  expectation: $([ "$FIXED" = 1 ] && echo "this build's, enforced" || echo "printed and not enforced")"
HOME="$RUN/home" "$BIN" --use-mock-keychain --remote-debugging-port="$PORT" --disable-backgrounding-occluded-windows \
  "--url=$URL" > "$LOG" 2>&1 &
PID=$!

# The probe is the guard, as in smoke.sh: if it reports a violation it could not
# stop, or ends early, this watcher ends the app at once.
guarded() { ours "$PROBE_PID" && ! grep -q '"k":"guard-failed"' "$PROBE_LOG"; }
(
  while guarded; do sleep 0.2; done
  ours "$PID" && kill -KILL "$PID" && echo "quit stall test: the probe cannot guard the run; the app was ended by SIGKILL"
) &
GUARD_PID=$!

n=0
until curl -s -m 2 "http://127.0.0.1:$PORT/json" 2>/dev/null | grep -q 'ass-quit-stall' || [ $n -ge 40 ]; do
  n=$((n + 1)); sleep 0.5
done
[ $n -lt 40 ] || not_run "the page did not load within 20 s"
COOKIE_AT=
if [ "$COOKIE" = 1 ]; then
  # The page has loaded, so its response, and the cookie with it, came before now.
  COOKIE_AT=$(now)
  echo "  the cookie page: served at $(cut -d ' ' -f 1 "$RUN/server.hits" | paste -s -d ' ' -), seen loaded at $COOKIE_AT"
  cookies loaded
fi
grep -q '"k":"killed"' "$PROBE_LOG" && not_run "the probe ended the app: $(grep -m 1 '"k":"killed"' "$PROBE_LOG")"

# The network service helper: exactly one child of the app with that sub-type
# and this run's profile folder.
n=0
while :; do
  HELPERS=$(pgrep -P "$PID" | tr '\n' ' ')
  NET= found=0
  for helper in $HELPERS; do
    case "$(ps -ww -o command= -p "$helper" 2>/dev/null) " in
      *--utility-sub-type=network.mojom.NetworkService*"--user-data-dir=$PROFILE "*|\
      *--utility-sub-type=network.mojom.NetworkService*"--user-data-dir=$PROFILE_REAL "*|\
      *"--user-data-dir=$PROFILE "*--utility-sub-type=network.mojom.NetworkService*|\
      *"--user-data-dir=$PROFILE_REAL "*--utility-sub-type=network.mojom.NetworkService*)
        NET=$helper found=$((found + 1)) ;;
    esac
  done
  [ $found = 1 ] || NET=
  [ $found = 1 ] || [ $n -ge 10 ] && break
  n=$((n + 1)); sleep 0.5
done
[ -n "$NET" ] || not_run "found $found network service helpers of this run among the app's children ($HELPERS)"

if [ -n "$HOLD" ]; then
  echo "  holding $HOLD s before the quit"
  sleep "$HOLD" &
  wait $!
  # What the profile wrote while the app held the cookie: every file changed
  # since the page was seen loaded.
  python3 -I -c 'import os, sys
profile, since = sys.argv[1], float(sys.argv[2])
changed = []
for folder, _, names in os.walk(profile):
    for name in names:
        path = os.path.join(folder, name)
        try:
            m = os.lstat(path).st_mtime
        except OSError:
            continue
        if m > since:
            changed.append((m - since, os.path.relpath(path, profile)))
print("  files changed in the hold, seconds after the cookie: " + ("; ".join("%s %+.2f" % (rel, d) for d, rel in sorted(changed)) or "none"))' \
    "$PROFILE" "$COOKIE_AT"
  cookies held
fi

SOCKETS_BEFORE=$(ls -d "$MAC_CHROMIUM_TMPDIR"/dev.ass.browser.*/SingletonSocket 2>/dev/null | wc -l | tr -d ' ')
HELPERS=$(echo $HELPERS $(pgrep -P "$PID") | tr ' ' '\n' | sort -un | tr '\n' ' ')
clones > "$RUN/clones-running.txt"

if [ "$NO_STOP" = 1 ]; then
  echo "  network service helper $NET left running; asking the app to quit"
else
  kill -STOP "$NET"
  echo "  network service helper $NET stopped (state $(ps -o state= -p "$NET" | tr -d ' ')); asking the app to quit"
fi
# The quit request, then up to 20 s for the app to end, timed by the kernel's
# exit event (kqueue), so a process that has ended but is not yet reaped counts
# as ended. Writes: the time before the request, the time it returned, the end
# time or "running", the reply. Meanwhile:
# - the app's children, with their ps lines, to children.jsonl: once right before
#   the request and then every 0.25 s until the app ends (only the app's own
#   children, pgrep -P; nothing attaches to any process);
# - the cookie files at the request, and every flush line of the app's log the
#   moment it appears (read every 5 ms), with the cookie files stated and copied
#   to cookies-flush/ at the first one: timer-events.jsonl;
# - with --during, once the debugging port lists no page, so no browser is left
#   (asked every 20 ms, at most 1.5 s; acted on 0.1 s after the list is first
#   empty; a renderer can outlive every browser until shutdown(), so renderers
#   say nothing here): a second quit request, or the file "fire" for this script
#   to start the second launch;
# - with --cont-after, the SIGCONT to the stopped helper, if it is still the
#   app's child.
# It runs in the background, so a signal to the script is acted on at once.
python3 -I - "$PID" "$RUN/children.jsonl" "$NET" "$DURING" "$CONT_AFTER" "$PROFILE" "$RUN" "$LOG" "$PORT" \
  > "$RUN/quit-times.txt" <<'EOF' &
import json, os, select, shutil, signal, subprocess, sys, threading, time, urllib.request
pid, children_path, net = int(sys.argv[1]), sys.argv[2], int(sys.argv[3])
during, cont_after = sys.argv[4], (float(sys.argv[5]) if sys.argv[5] else None)
profile, run, applog, port = sys.argv[6], sys.argv[7], sys.argv[8], int(sys.argv[9])
direct = urllib.request.build_opener(urllib.request.ProxyHandler({}))
out = open(children_path, "w")
notes = open(os.path.join(run, "timer-events.jsonl"), "w")
lock = threading.Lock()

def note(**record):
    record.setdefault("t", time.time())
    with lock:
        notes.write(json.dumps(record) + "\n")
        notes.flush()

kq = select.kqueue()
try:
    kq.control([select.kevent(pid, select.KQ_FILTER_PROC, select.KQ_EV_ADD | select.KQ_EV_ONESHOT,
                              select.KQ_NOTE_EXIT)], 0, 0)
except OSError:
    print("gone")
    sys.exit()

def children():
    kids = subprocess.run(["pgrep", "-P", str(pid)], capture_output=True, text=True).stdout.split()
    if not kids:
        return []
    ps = subprocess.run(["ps", "-ww", "-o", "pid=,ppid=,state=,etime=,command=", "-p", ",".join(kids)],
                        capture_output=True, text=True).stdout
    return [l.strip() for l in ps.splitlines() if l.strip() and l.split()[1] == str(pid)]

def snapshot(label):
    t = time.time()
    lines = children()
    browser = subprocess.run(["ps", "-o", "state=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    out.write(json.dumps({"t": t, "label": label, "browser": browser or "gone", "children": lines}) + "\n")
    out.flush()
    return lines

def cookie_files(copy_to=None):
    found = {}
    for folder, _, names in os.walk(profile):
        for name in names:
            if "Cookies" in name:
                path = os.path.join(folder, name)
                rel = os.path.relpath(path, profile)
                try:
                    st = os.stat(path)
                    if copy_to:
                        os.makedirs(os.path.dirname(os.path.join(copy_to, rel)), exist_ok=True)
                        shutil.copy2(path, os.path.join(copy_to, rel))
                except OSError:
                    continue
                found[rel] = [st.st_mtime, st.st_size]
    return found

def watch_flush(stop):
    seen = 0
    while True:
        ending = stop.is_set()
        try:
            text = open(applog, errors="replace").read()
        except OSError:
            text = ""
        lines = [l for l in text.split("\n")[:-1] if l.startswith("Ass: quit: the cookie flush")]
        for line in lines[seen:]:
            t = time.time()
            files = cookie_files(os.path.join(run, "cookies-flush") if seen == 0 else None)
            note(kind="flush-line", line=line, seen=t, files=files)
            seen += 1
        if ending:
            return
        time.sleep(0.005)

def osascript():
    script = ("ObjC.import('AppKit'); var app = $.NSRunningApplication.runningApplicationWithProcessIdentifier(%d);"
              " app.isNil() ? 'no such app' : String(app.terminate)" % pid)
    reply = subprocess.run(["osascript", "-l", "JavaScript", "-e", script], capture_output=True, text=True)
    return (reply.stdout.strip() or reply.stderr.strip() or "none").replace(" ", "_")

def pages():
    """The page targets the debugging port lists, or None when it does not answer."""
    try:
        with direct.open("http://127.0.0.1:%d/json/list" % port, timeout=0.5) as reply:
            return [t.get("url", "") for t in json.load(reply) if t.get("type") == "page"]
    except Exception:
        return None

def fire(word):
    with open(os.path.join(run, "fire.tmp"), "w") as f:
        f.write(word + "\n")
    os.rename(os.path.join(run, "fire.tmp"), os.path.join(run, "fire"))

snapshot("before the quit request")
note(kind="cookies", label="at the quit request", files=cookie_files())
stop = threading.Event()
watcher = threading.Thread(target=watch_flush, args=(stop,))
watcher.start()
asked_at = time.time()
reply = osascript()
quit_at = time.time()
events = []
last = 0.0
fired = not during
empty_at = None
conted = cont_after is None
while not events and time.time() < quit_at + 20:
    now = time.time()
    if now - last >= 0.25:
        snapshot("after the quit request")
        last = now
    if not fired:
        # 0.1 s after the list is first seen empty, so the browsers' own close
        # callbacks have most likely run; the flush line's clock decides.
        left = pages() if empty_at is None else []
        if left == [] and empty_at is None:
            empty_at = now
            note(kind="pages-gone")
        if empty_at is not None and now >= empty_at + 0.1:
            fired = True
            if during == "quit":
                begun = time.time()
                answer = osascript()
                note(kind="second-quit", begun=begun, returned=time.time(), reply=answer)
            else:
                fire("go")
                note(kind="fire")
        elif empty_at is None and now > asked_at + 1.5:
            fired = True
            note(kind="second-skipped", left=left if left is not None else "no answer")
            if during == "launch":
                fire("skip")
    if not conted and now >= asked_at + cont_after:
        conted = True
        info = subprocess.run(["ps", "-o", "ppid=,state=", "-p", str(net)], capture_output=True, text=True).stdout.split()
        if len(info) == 2 and info[0] == str(pid):
            os.kill(net, signal.SIGCONT)
            note(kind="cont", state_before=info[1])
        else:
            note(kind="cont-skipped", ps=info)
    events = kq.control(None, 1, 0.02)
ended = "%.3f" % time.time() if events else "running"
if not fired and during == "launch":
    fire("skip")
stop.set()
watcher.join(5)
print("%.3f %.3f %s %s" % (asked_at, quit_at, ended, reply))
EOF
TIMER_PID=$!

# With --during=launch: the second start on the same profile, by this script's
# own shell, so that the probe may end it, at the moment the timer gives. It
# must hand the request to the running instance and exit 0 (15 s at most, then
# SIGKILL). Its end is timed to 20 ms.
SECOND_BEGUN= SECOND_ENDED= SECOND_CODE=
if [ "$DURING" = launch ]; then
  n=0
  until [ -s "$RUN/fire" ] || ! kill -0 "$TIMER_PID" 2>/dev/null || [ $n -ge 250 ]; do n=$((n + 1)); sleep 0.02; done
  if [ "$(cat "$RUN/fire" 2>/dev/null)" = go ] && guarded; then
    SECOND_BEGUN=$(now)
    HOME="$RUN/home" "$BIN" --use-mock-keychain --remote-debugging-port="$SECOND_PORT" --disable-backgrounding-occluded-windows \
      '--url=data:text/html,<title>ass-quit-stall-second</title>' > "$RUN/app-second.log" 2>&1 &
    SECOND_PID=$!
    n=0
    while kill -0 "$SECOND_PID" 2>/dev/null && [ $n -lt 750 ] && guarded; do n=$((n + 1)); sleep 0.02; done
    SECOND_ENDED=$(now)
    if kill -0 "$SECOND_PID" 2>/dev/null; then
      kill -KILL "$SECOND_PID"
      wait "$SECOND_PID"
      SECOND_CODE="none: still running after 15 s or the probe could not guard it, ended by SIGKILL ($?)"
    else
      wait "$SECOND_PID"
      SECOND_CODE=$?
    fi
    SECOND_PID_RAN=$SECOND_PID
    SECOND_PID=
  fi
fi
wait "$TIMER_PID"
TIMER_PID=
read -r ASKED_AT QUIT_AT ENDED_AT REPLY < "$RUN/quit-times.txt"
[ "$ASKED_AT" = gone ] && not_run "the app had ended before the quit request"
if [ "$ENDED_AT" = running ]; then
  ours "$PID" && kill -TERM "$PID" && await "$PID" 20
  ours "$PID" && kill -KILL "$PID"
  wait "$PID"
  CODE="none: still running 20 s after the quit request, then ended by this script with code $?"
  ENDED_AT=$(now)
else
  wait "$PID"
  CODE=$?
fi
PID_RAN=$PID
PID=

# What the quit left, 3 s after the end, before anything is ended.
sleep 3
LEFT=
for helper in $HELPERS; do
  kill -0 "$helper" 2>/dev/null || continue
  what="$(ps -ww -o command= -p "$helper" | grep -o -e '--type=[a-z-]*' -e '--utility-sub-type=[A-Za-z_.]*' | paste -s -d " " -)"
  LEFT="$LEFT${LEFT:+; }pid $helper, ${what:-type unknown}, state $(ps -o state= -p "$helper" | tr -d ' '), parent $(ps -o ppid= -p "$helper" | tr -d ' ')"
done
SOCKETS_AFTER=$(ls -d "$MAC_CHROMIUM_TMPDIR"/dev.ass.browser.*/SingletonSocket 2>/dev/null | wc -l | tr -d ' ')
# This launch's clone: what is new against the list before the launch, seen
# right before the quit or now. A helper deletes it after the app has ended, so
# its removal is awaited up to 10 s after the end.
n=0
while :; do
  clones > "$RUN/clones-after.txt"
  CLONE=$(sort -u "$RUN/clones-running.txt" "$RUN/clones-after.txt" | comm -13 "$RUN/clones-before.txt" - | tr '\n' ' ')
  CLONE_LEFT=
  for c in $CLONE; do [ -e "$c" ] && CLONE_LEFT="$CLONE_LEFT $c"; done
  [ -z "$CLONE_LEFT" ] || [ $n -ge 14 ] && break
  n=$((n + 1)); sleep 0.5
done
WATCHDOG=$(grep -m 1 'Teardown watchdog expired' "$LOG")
MEASURED=$(python3 -I -c 'import sys
asked, quit, end = (float(a) for a in sys.argv[1:4])
print("%.2f|%.2f|%.2f" % (end - quit, end - asked, quit - asked))' "$ASKED_AT" "$QUIT_AT" "$ENDED_AT")
TO_END=${MEASURED%%|*}
rest=${MEASURED#*|}
FROM_ASKED=${rest%%|*}
OSASCRIPT=${rest#*|}

echo "RESULT"
echo "  quit request through AppKit, reply $REPLY: the app ended $TO_END s after the request returned" \
  "($FROM_ASKED s after it was begun; osascript took $OSASCRIPT s); exit code $CODE"
echo "  'Teardown watchdog expired' on stderr: ${WATCHDOG:-no}"
# The flush: each line with its clock against the request, and when this script
# saw it; when the flush began (the clock less the wait the line gives); the
# second request, the second launch and the SIGCONT against that interval, which
# is the state where no browser is left and the message loop still runs (an
# event is in it if it lies between the beginning and the line). Windows the
# probe saw after the request, and the window lines of each log. Writes
# flush-summary.txt for the expectations below.
python3 -I - "$ASKED_AT" "$ENDED_AT" "$LOG" "$RUN/timer-events.jsonl" "$PROBE_LOG" "$PID_RAN" \
  "${SECOND_PID_RAN:-}" "${SECOND_BEGUN:-}" "${SECOND_ENDED:-}" "$RUN/app-second.log" "$RUN/flush-summary.txt" \
  "${SECOND_CODE:-}" <<'EOF'
import json, os, re, sys
asked, end, applog, events_path, probe_log = float(sys.argv[1]), float(sys.argv[2]), sys.argv[3], sys.argv[4], sys.argv[5]
first_pid = int(sys.argv[6])
second_pid = int(sys.argv[7]) if sys.argv[7] else None
second_begun = float(sys.argv[8]) if sys.argv[8] else None
second_ended = float(sys.argv[9]) if sys.argv[9] else None
second_log, summary_path, second_code = sys.argv[10], sys.argv[11], sys.argv[12]
events = [json.loads(l) for l in open(events_path) if l.endswith("\n")] if os.path.exists(events_path) else []
seen = {e["line"]: e["seen"] for e in events if e["kind"] == "flush-line"}
lines = [l for l in open(applog, errors="replace").read().splitlines() if l.startswith("Ass: quit: the cookie flush")]
print("  the cookie flush: %d line(s)" % len(lines))
kind, flush_at, began = "none", None, None
for i, line in enumerate(lines):
    m = re.search(r", at (\d+\.\d{3})(, after the bound; ignored)?$", line)
    at = float(m.group(1)) if m else None
    print("    %s" % line)
    if at is not None:
        print("      its clock %+.3f s after the request was begun%s" % (at - asked, "; seen by this script %.3f s "
              "after its clock" % (seen[line] - at) if line in seen else ""))
    if i == 0 and at is not None:
        flush_at = at
        took = re.search(r"answered in (\d+) ms", line)
        if "gave no answer" in line:
            kind, began = "no-answer", at - 2.0
        elif "could not be started" in line:
            kind = "not-started"
        elif took and not m.group(2):
            kind, began = "answered", at - int(took.group(1)) / 1000
if began is not None:
    print("    the flush began %+.3f s after the request (the first line's clock less its wait); the app ended %.2f s "
          "after that line" % (began - asked, end - flush_at))

def place(t0, t1):
    """Where an event that happened between t0 and t1 lies against the flush."""
    if began is None:
        return "the flush's interval is not known"
    if t0 >= began - 0.001 and t1 <= flush_at + 0.001:
        return "inside the flush's interval (%.3f to %.3f s after it began)" % (t0 - began, t1 - began)
    return "NOT inside the flush's interval (%.3f to %.3f s after it began, which lasted %.3f s)" % (
        t0 - began, t1 - began, flush_at - began)

in_state = None
for e in events:
    if e["kind"] == "pages-gone":
        print("    the debugging port listed no page %.3f s after the request" % (e["t"] - asked))
    elif e["kind"] == "second-quit":
        where = place(e["begun"], e["returned"])
        in_state = where.startswith("inside")
        print("    the second quit request: begun %.3f s after the first, reply %s; %s" % (e["begun"] - asked, e["reply"], where))
    elif e["kind"] == "second-skipped":
        print("    the second request was NOT made: the debugging port still listed %s 1.5 s after the request" % e["left"])
    elif e["kind"] == "cont":
        print("    SIGCONT to the stopped helper %.3f s after the request (state before: %s)%s" % (
            e["t"] - asked, e["state_before"], "; %.0f ms after the flush began, so %s the 2000 ms bound" % (
                (e["t"] - began) * 1000, "inside" if e["t"] - began < 2.0 else "after") if began is not None else ""))
    elif e["kind"] == "cont-skipped":
        print("    SIGCONT NOT sent: the helper was no longer the app's child (%s)" % e["ps"])
if second_begun is not None:
    where = place(second_begun, second_ended)
    in_state = where.startswith("inside")
    print("    the second launch: begun %.3f s after the request, ended %.3f s after it, exit code %s; %s" % (
        second_begun - asked, second_ended - asked, second_code, where))

# Windows: every window record of the probe, by number and process.
before, after, of_second = set(), [], []
for l in open(probe_log):
    if not l.startswith('{"') or not l.endswith("\n"):
        continue
    r = json.loads(l)
    if r.get("k") != "window":
        continue
    owner = re.search(r"pid=(\d+)", r.get("state", ""))
    owner = int(owner.group(1)) if owner else None
    if second_pid and owner == second_pid:
        of_second.append(r)
    if r["t"] < asked:
        before.add(r["number"])
    elif r["number"] not in before or owner != first_pid:
        after.append(r)
print("    windows the probe saw after the request that it had not seen before, or of another process: %s" % (
    "; ".join("%.3f s %s" % (r["t"] - asked, r["state"]) for r in after) or "none"))
shown = sum(1 for l in open(applog, errors="replace") if l.startswith("Ass: test window shown:"))
print("    window lines in the first launch's log: %d (1 is the start)" % shown)
stop = []
if second_pid is not None:
    text = open(second_log, errors="replace").read() if os.path.exists(second_log) else ""
    second_lines = [l for l in text.splitlines() if l.startswith("Ass:")]
    print("    the second launch's own lines: %s" % (" | ".join(second_lines) or "none"))
    if of_second or "Ass: test window shown:" in text:
        stop.append("the second launch showed a window")
if shown > 1:
    stop.append("the first launch showed its window again")
with open(summary_path, "w") as f:
    f.write("%s %s %s %s\n" % (kind, "%.3f" % (flush_at - asked) if flush_at else "none",
                               "%.3f" % (end - flush_at) if flush_at else "none",
                               {None: "none", True: "in", False: "out"}[in_state]))
    f.write("STOP: " + "; ".join(stop) + "\n" if stop else "")
for s in stop:
    print("  STOP: " + s)
EOF
[ -s "$RUN/flush-summary.txt" ] || echo "unread none none none" > "$RUN/flush-summary.txt"
read -r FLUSH_KIND FLUSH_OFFSET END_AFTER_FLUSH IN_STATE < "$RUN/flush-summary.txt"
STOP_LINE=$(grep '^STOP: ' "$RUN/flush-summary.txt")

# Where the browser was during the stall, when the helper was stopped. (1)
# Whether the profile was written after the quit request: Local State and
# Preferences, against the time the request was begun (every file written after
# it: profile-writes.txt). (2) The app's children at the request, and in the
# last second before the watchdog is due, 10 s after the last of those writes.
# (3) The probe's last sample of the app's process list before the end, as a
# cross-check (it lists exited children not yet reaped, which pgrep does not).
# Then the reading: the GPU and storage helpers still children of the living
# browser at that second place it in the wait for the network helper; gone,
# past it; no profile write after the request, stalled before shutdown().
# Renderers are reported, not counted: they end with their tabs.
[ "$NO_STOP" = 0 ] && python3 -I - "$ASKED_AT" "$ENDED_AT" "$PROFILE" "$RUN/children.jsonl" "$PROBE_LOG" "$PID_RAN" \
  "$RUN/profile-writes.txt" <<'EOF'
import collections, json, os, re, sys
asked, end = float(sys.argv[1]), float(sys.argv[2])
profile, children_log, probe_log, pid, writes_path = sys.argv[3], sys.argv[4], sys.argv[5], int(sys.argv[6]), sys.argv[7]

def kind(line):
    t, u = re.search(r"--type=(\S+)", line), re.search(r"--utility-sub-type=(\S+)", line)
    return (t.group(1) if t else "type unknown") + (":" + u.group(1).split(".")[0] if u else "")

def summary(snap):
    kinds = collections.Counter("%s (%s)" % (kind(l), l.split()[2]) for l in snap["children"])
    return "browser %s; %d children: %s" % (snap["browser"], len(snap["children"]),
                                            ", ".join("%s x%d" % kv if kv[1] > 1 else kv[0] for kv in sorted(kinds.items())) or "none")

# (1) Profile writes after the quit request.
written = []
for folder, _, files in os.walk(profile):
    for name in files:
        path = os.path.join(folder, name)
        try:
            m = os.lstat(path).st_mtime
        except OSError:
            continue
        if m > asked:
            written.append((m - asked, os.path.relpath(path, profile)))
with open(writes_path, "w") as out:
    for offset, rel in sorted(written):
        out.write("%+.3f s  %s\n" % (offset, rel))
named = [os.path.join(profile, "Local State")] + sorted(
    os.path.join(f, "Preferences") for f, _, files in os.walk(profile) if "Preferences" in files
    and f.count(os.sep) - profile.count(os.sep) <= 1)
last_write = None
print("  (1) profile writes after the quit request (%d files in all, profile-writes.txt):" % len(written))
for path in named:
    rel = os.path.relpath(path, profile)
    if not os.path.exists(path):
        print("      %s: missing" % rel)
        continue
    m = os.stat(path).st_mtime
    if m > asked:
        last_write = max(last_write or m, m)
        print("      %s: written %.2f s after the request, %.2f s before the end" % (rel, m - asked, end - m))
    else:
        print("      %s: not written after the request (last %.2f s before it)" % (rel, asked - m))

# (2) The app's children at the request and in the last second before the watchdog is due.
snaps = [json.loads(l) for l in open(children_log) if l.endswith("\n")]
first = snaps[0] if snaps else None
print("  (2) at the quit request: %s" % (summary(first) if first else "no snapshot"))
for line in (first or {"children": []})["children"]:
    print("        " + line[:300])
due = last_write + 10 if last_write else None
living = [s for s in snaps if s["browser"] != "gone" and s["t"] < end]
window = [s for s in living if due and due - 1 <= s["t"] < due]
at_due = window[-1] if window else None
last = living[-1] if living else None
if at_due:
    print("      in the last second before the watchdog is due, %.2f s after the request (%.2f s after the last "
          "profile write): %s" % (at_due["t"] - asked, at_due["t"] - last_write, summary(at_due)))
    for line in at_due["children"]:
        print("        " + line[:300])
else:
    print("      in the last second before the watchdog is due: no snapshot (%s)"
          % ("no profile write after the request" if not due else "none of the browser alive in that second"))
if last:
    print("      last snapshot before the end, %.2f s before it: %s" % (end - last["t"], summary(last)))

# (3) The probe's last sample of the app's process list before the end.
kinds = {}
for s in snaps:
    for line in s["children"]:
        kinds[int(line.split()[0])] = kind(line)
probe = None
for line in open(probe_log):
    if not line.startswith('{"') or not line.endswith("\n"):
        continue
    r = json.loads(line)
    if r.get("k") == "s" and r.get("t", 0) < end:
        for a in r.get("apps", []):
            if a.get("pid") == pid:
                probe = (r["t"], [p for p in a.get("pidList", []) if p != pid])
if probe:
    found = collections.Counter(kinds.get(p, "not in our snapshots") for p in probe[1])
    print("  (3) the probe's last sample, %.2f s before the end: %d children: %s"
          % (end - probe[0], len(probe[1]), ", ".join("%s x%d" % kv if kv[1] > 1 else kv[0] for kv in sorted(found.items()))))
else:
    print("  (3) the probe's last sample: none with the browser in it")

# The reading.
if not last_write:
    print("  READING: the profile was not written after the quit request: the quit stalled before shutdown()")
elif not at_due:
    print("  READING: not settled: no snapshot of the living browser in the last second before the watchdog was due")
else:
    # Renderers are left out: they end with their tabs, which the quit closes
    # before shutdown() (seen in the first run, 2026-11-24: all three gone 0.09
    # to 0.38 s after the request, while the profile was being written).
    # By PID: an exiting renderer can lose its command line before it is gone.
    renderers = {l.split()[0] for l in first["children"] if kind(l) == "renderer"}
    gone = [s["t"] - asked for s in living if not renderers & {l.split()[0] for l in s["children"]}]
    print("      renderers: %s" % ("all gone %.2f s after the request" % gone[0] if gone else "still there at the end"))
    want = {"gpu-process", "utility:storage"}
    before = {kind(l) for l in first["children"]} & want
    still = {kind(l) for l in at_due["children"]} & want
    if before and still == before:
        print("  READING: stuck in the wait for the network helper: %s still children of the living browser at "
              "that second" % ", ".join(sorted(still)))
    elif not still:
        print("  READING: stalled elsewhere: the browser had ended %s by that second, so it got past the wait"
              % (", ".join(sorted(before)) or "its other helpers"))
    else:
        print("  READING: not settled: at that second %s were still there, %s gone"
              % (", ".join(sorted(still)), ", ".join(sorted(before - still))))
EOF
echo "  helpers left 3 s after the end: ${LEFT:-none}"
if [ "$SOCKETS_BEFORE" -lt 1 ]; then SOCKET="none while the app ran, so this proves nothing"
elif [ "$SOCKETS_AFTER" = 0 ]; then SOCKET=removed
else SOCKET=left; fi
echo "  socket folder: $SOCKET"
if [ -z "$CLONE" ]; then CLONED="none appeared in $CLONE_DIR while the app ran, so this proves nothing"
elif [ -z "$CLONE_LEFT" ]; then CLONED="removed ($CLONE)"
else CLONED="left:$CLONE_LEFT (not deleted)"; fi
echo "  bundle clone of this launch: $CLONED"

# With --cookie: the cookie files at each moment, and the verdict. The test
# (--no-stop): the flush answered; the cookie database was last changed after
# the page with the cookie had loaded and no later than the flush line's clock
# (cut to milliseconds, so 1 ms is allowed); it had not been changed between the
# load and the request; and its copy at the flush line holds the cookie's row.
# The control (helper stopped): no answer; the database unchanged from the
# request to the flush line; no row in that copy.
COOKIE_VERDICT=
[ "$COOKIE" = 1 ] && cookies end
# With --hold the store's own 30 s commit can come before the quit, so the run
# shows which file a commit changes and is not judged.
[ -n "$HOLD" ] && echo "  no verdict: with --hold the quit is not the first commit"
if [ "$COOKIE" = 1 ] && [ -z "$HOLD" ]; then
  COOKIE_VERDICT=$(python3 -I - "$RUN" "$COOKIE_AT" "$ASKED_AT" "$NO_STOP" "$FLUSH_KIND" "$FLUSH_OFFSET" <<'EOF'
import json, os, subprocess, sys
run, cookie_at, asked, no_stop, kind = sys.argv[1], float(sys.argv[2]), float(sys.argv[3]), sys.argv[4] == "1", sys.argv[5]
flush_at = asked + float(sys.argv[6]) if sys.argv[6] != "none" else None
events = [json.loads(l) for l in open(os.path.join(run, "timer-events.jsonl")) if l.endswith("\n")]
at_request = next((e["files"] for e in events if e["kind"] == "cookies"), {})
at_flush = next((e["files"] for e in events if e["kind"] == "flush-line"), None)
loaded = json.load(open(os.path.join(run, "cookies-loaded.json")))["files"]
end = json.load(open(os.path.join(run, "cookies-end.json")))["files"]
names = sorted(set(loaded) | set(at_request) | set(at_flush or {}) | set(end))
main = "Default/Cookies" if "Default/Cookies" in names else next((n for n in names if n.endswith("Cookies")), None)
rel = lambda m: "%+.3f" % (m - asked)
for name in names:
    print("  %s, last change against the request (s): loaded %s, at the request %s, at the flush line %s, after the end %s"
          % (name, *[rel(d[name][0]) if d and name in d else "missing" for d in (loaded, at_request, at_flush, end)]))
row = "no database"
if at_flush is not None and main:
    copy = os.path.join(run, "cookies-flush", main)
    uri = copy if os.path.exists(copy + "-wal") else "file:%s?immutable=1" % copy
    done = subprocess.run(["/usr/bin/sqlite3", "-readonly", uri, "select host_key, name from cookies where name = 'ass_flush'"],
                          capture_output=True, text=True)
    row = done.stdout.strip() or ("absent" if done.returncode == 0 else "error: " + done.stderr.strip()[:200])
print("  the copy taken at the flush line, %s: the test cookie's row: %s" % (main, row))
problems = []
if not main or at_flush is None or main not in at_flush:
    problems.append("no cookie database at the flush line")
else:
    m = at_flush[main][0]
    if no_stop:
        if kind != "answered":
            problems.append("the flush did not answer (%s)" % kind)
        if not cookie_at <= m:
            problems.append("last changed %.3f s before the cookie was seen loaded" % (cookie_at - m))
        if flush_at is None or m > flush_at + 0.001:
            problems.append("last changed after the flush line's clock")
        if main in at_request and at_request[main][0] >= cookie_at:
            problems.append("already changed between the load and the request")
        if "ass_flush" not in row:
            problems.append("no row for the cookie in the copy")
        if not problems:
            print("  PASS  the cookie reached the database before the flush line: the page loaded %.3f, the file "
                  "changed %.3f, the line's clock %.3f (s after the request; the file at the request: %s)"
                  % (cookie_at - asked, m - asked, flush_at - asked,
                     rel(at_request[main][0]) if main in at_request else "missing"))
    else:
        if kind != "no-answer":
            problems.append("the flush did not end without an answer (%s)" % kind)
        if main in at_request and m != at_request[main][0]:
            problems.append("the file changed between the request and the flush line")
        if "ass_flush" in row:
            problems.append("the cookie's row is in the copy")
        if not problems:
            print("  PASS  (control) no answer, the file unchanged from the request to the flush line, no row")
for p in problems:
    print("  FAIL  " + p)
EOF
)
  echo "$COOKIE_VERDICT"
fi

# The helpers this quit left: what each holds open under the profile (item 9 of
# the quit work: kept for every left helper). With --restart this is the only
# record of the stopped helper, because the second start ends it with SIGKILL.
if [ -n "$NET" ] && kill -0 "$NET" 2>/dev/null; then
  echo "  the stopped helper $NET, $(python3 -I -c 'import sys, time; print("%.1f" % (time.time() - float(sys.argv[1])))' "$ENDED_AT") s" \
    "after the app ended: $(record_helper "$NET" "$RUN/leftover-$NET-after-the-end.txt")"
fi

# With --restart: a second start on the same
# profile while the stopped helper is still there with launchd as its parent. The
# build that ends leftover helpers at start should print "Ass: ended ...", load
# its page and quit with exit code 0. Same rules as the first launch, its own log
# (app-restart.log) and guard; the probe started above watches it too.
RESTART_LOADED= RESTART_ENDED_LINE= RESTART_NET= CODE2= LEFT2= RESTART_FLUSH=
if [ "$RESTART" = 1 ]; then
  echo "RESTART"
  BEFORE=$(ps -o pid=,ppid=,state= -p "$NET" 2>/dev/null | xargs)
  echo "  the stopped helper before the start: ${BEFORE:-gone}"
  ours "$GUARD_PID" && kill -TERM "$GUARD_PID"
  HOME="$RUN/home" "$BIN" --use-mock-keychain --remote-debugging-port="$PORT" --disable-backgrounding-occluded-windows \
    '--url=data:text/html,<title>ass-quit-stall-restart</title>' > "$RUN/app-restart.log" 2>&1 &
  PID=$!
  (
    while guarded; do sleep 0.2; done
    ours "$PID" && kill -KILL "$PID" && echo "quit stall test: the probe cannot guard the run; the app was ended by SIGKILL"
  ) &
  GUARD_PID=$!
  n=0
  until curl -s -m 2 "http://127.0.0.1:$PORT/json" 2>/dev/null | grep -q 'ass-quit-stall-restart' || [ $n -ge 40 ]; do
    n=$((n + 1)); sleep 0.5
  done
  [ $n -lt 40 ] && RESTART_LOADED="loaded" || RESTART_LOADED="did not load within 20 s"
  RESTART_HELPERS=$(pgrep -P "$PID" | tr '\n' ' ')
  HELPERS="$HELPERS $RESTART_HELPERS"
  RESTART_ENDED_LINE=$(grep -m 1 '^Ass: ended' "$RUN/app-restart.log")
  RESTART_NET=$(kill -0 "$NET" 2>/dev/null && echo "still there, state $(ps -o state= -p "$NET" | tr -d ' ')" || echo gone)
  echo "  the page: $RESTART_LOADED"
  echo "  the app's line: ${RESTART_ENDED_LINE:-none}"
  echo "  the stopped helper after the start: $RESTART_NET"
  REPLY2=$(appkit_quit "$PID")
  await "$PID" 40
  if kill -0 "$PID" 2>/dev/null; then
    CODE2="none: still running 20 s after the quit request (ended by the cleanup)"
  else
    wait "$PID"
    CODE2=$?
    PID=
  fi
  for helper in $RESTART_HELPERS; do
    kill -0 "$helper" 2>/dev/null && await "$helper" 6
    kill -0 "$helper" 2>/dev/null && LEFT2="$LEFT2 $helper"
  done
  RESTART_FLUSH=$(grep '^Ass: quit: the cookie flush' "$RUN/app-restart.log" | paste -s -d '|' -)
  echo "  quit through AppKit, reply $REPLY2: exit code $CODE2; its helpers still there after up to 3 s each: ${LEFT2:-none}"
  echo "  its flush line(s): ${RESTART_FLUSH:-none}"
fi

# Ends what the quit left: the stopped helper first, then any other of this run.
echo "  the network helper$([ "$NO_STOP" = 0 ] && echo ", stopped"): $(end_helper "$NET")"
NET=
for helper in $HELPERS; do
  kill -0 "$helper" 2>/dev/null && echo "  another helper left: $(end_helper "$helper")"
done
HELPERS=
# The control: whether the helper, once let go and ended, wrote the cookie after all.
[ "$COOKIE" = 1 ] && [ "$NO_STOP" = 0 ] && cookies helper-ended

ours "$GUARD_PID" && kill -TERM "$GUARD_PID"
GUARD_PID=
kill -TERM "$PROBE_PID"
wait "$PROBE_PID"
PROBE_CODE=$?
PROBE_PID=
SCREEN=$(python3 -I -c 'import json, sys
records = [json.loads(line) for line in open(sys.argv[1]) if line.startswith("{")]
summary = [r for r in records if r["k"] == "summary"]
if not summary:
    sys.exit("no probe summary")
bad = {k: v for k, v in summary[0]["counts"].items() if k.startswith("violation-")}
print("violations %s, killed by the probe %s" % (bad or "none", [w["pid"] for w in summary[0]["watched"] if w["killed"]] or "none"))
sys.exit(1 if bad else 0)' "$PROBE_LOG" 2>&1)
SCREEN_OK=$?
echo "  screen and keyboard: $SCREEN; probe exit code $PROBE_CODE"
[ "$SCREEN_OK" = 0 ] && [ "$PROBE_CODE" = 0 ] || { echo "FAIL  the probe saw the app take the screen or the keyboard, or could not guard the run"; exit 1; }
[ -z "$STOP_LINE" ] || { echo "FAIL  $STOP_LINE"; exit 1; }

if [ "$FIXED" = 1 ]; then
  echo "EXPECTED (this build, the network helper stopped): the no-answer line 2 to 3 s after the request; exit code 2" \
    "with the watchdog's line, 9.5 to 12 s after it; then the restart ends the stopped helper ('Ass: ended 1 ...')," \
    "loads its page and quits with exit code 0 and an answered flush, leaving no helper"
  MISSED=$(python3 -I - "$FLUSH_KIND" "$FLUSH_OFFSET" "$END_AFTER_FLUSH" <<'EOF'
import sys
kind, offset, after = sys.argv[1:4]
missed = []
if kind != "no-answer":
    missed.append("the first flush line is %s" % kind)
elif not 2.0 <= float(offset) <= 3.0:
    missed.append("the no-answer line %s s after the request" % offset)
if after == "none" or not 9.5 <= float(after) <= 12.0:
    missed.append("the end %s s after the flush line" % after)
print(", ".join(missed))
EOF
)
  [ "$CODE" = 2 ] || MISSED="$MISSED, exit code $CODE"
  [ -n "$WATCHDOG" ] || MISSED="$MISSED, no watchdog line"
  [ "$RESTART_ENDED_LINE" = "Ass: ended 1 helper process(es) left running by an earlier run of this profile" ] ||
    MISSED="$MISSED, the restart's line: ${RESTART_ENDED_LINE:-none}"
  [ "$RESTART_NET" = gone ] || MISSED="$MISSED, the stopped helper after the restart: $RESTART_NET"
  [ "$RESTART_LOADED" = loaded ] || MISSED="$MISSED, the restart's page $RESTART_LOADED"
  [ "$CODE2" = 0 ] || MISSED="$MISSED, the restart's exit code $CODE2"
  [ -z "$LEFT2" ] || MISSED="$MISSED, the restart left helpers$LEFT2"
  echo "$RESTART_FLUSH" | grep -q -E -x 'Ass: quit: the cookie flush answered in [0-9]+ ms, at [0-9]+\.[0-9]{3}' ||
    MISSED="$MISSED, the restart's flush: ${RESTART_FLUSH:-none}"
  MISSED=${MISSED#, }
  if [ -z "$MISSED" ]; then
    echo "PASS  the stalled quit ends as this build should, and the next start ends the helper it left"
    exit 0
  fi
  echo "FAIL  ${MISSED}"
  exit 1
fi
if [ "$NO_STOP" = 0 ]; then
  echo "PREDICTION (not enforced): with the helper stopped, the no-answer line about 2 s after the request," \
    "then 'Teardown watchdog expired' and exit code 2 about 10 s after it, and the stopped helper left. These follow" \
    "by construction; where the browser was is the READING line"
fi
# The cookie verdict is enforced: its FAIL lines make the exit code 1.
case "$COOKIE_VERDICT" in *"  FAIL  "*) exit 1 ;; esac
exit 0
