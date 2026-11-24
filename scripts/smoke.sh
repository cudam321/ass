#!/bin/sh
# Smoke test for Ass.app. Run after every build.
#
#   scripts/smoke.sh            # tests target/debug/app.noindex/Ass.app
#   scripts/smoke.sh release    # tests target/release/app.noindex/Ass.app
#
# Runs the app in test window mode (src/test_window.rs): its window is ordered
# in where users get it, at alpha 0, above normal windows (layer 8) and on every
# desktop, and the app is meant never to be activated.
# scripts/appprobe.swift checks that, and kills the app the moment it is
# frontmost, active or the menu bar owner, or has an on-screen window with
# alpha above 0. Building the probe needs swiftc (Xcode Command Line Tools).
#
# Uses a throwaway profile, a mock keychain and its own temporary folder, so it
# never touches the real profile, the login keychain or $TMPDIR. Set
# ASS_SMOKE_PORT to run two copies at once. Quits through AppKit by PID
# (NSRunningApplication.terminate, the path Cmd+Q takes); pressing Cmd+Q
# itself is not covered here.

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/target/${1:-debug}/app.noindex/Ass.app"
BIN="$APP/Contents/MacOS/Ass"
PORT="${ASS_SMOKE_PORT:-9333}"
PROBE="$ROOT/target/appprobe"
FAILED=0
HANDED_OFF=0

pass() { echo "PASS  $1"; }
fail() { echo "FAIL  $1"; FAILED=1; }
pause() { n=0; while [ $n -lt "$1" ]; do n=$((n + 1)); sleep 0.5; done; }
# Waits up to $2 half-seconds for process $1 to end.
await() { n=0; while kill -0 "$1" 2>/dev/null && [ $n -lt "$2" ]; do n=$((n + 1)); sleep 0.5; done; }

# On any exit, Ctrl+C or signal: end what this script started and remove the
# folders it made. A PID is signalled only while it is still this script's child,
# so a PID that was never set, already ended or was reused is left alone. Runs
# once; signals during it are ignored. A completed run that failed keeps its logs.
PID= SECOND_PID= REFUSED_PID= PROBE_PID= GUARD_PID= MADE_PROFILE= MADE_TMP= KEEP_LOGS=0 CLEANED=0 HELPERS=
ours() { [ -n "$1" ] && [ "$(ps -o ppid= -p "$1" 2>/dev/null | tr -d ' ')" = "$$" ]; }
# end_leftover PID: ends a helper that outlived the app, and says what it did. Only a
# PID in $HELPERS, which holds the app's children recorded while the app lived, and only
# while its command line still carries this run's profile folder (--user-data-dir, which
# Chromium gives every helper), so a PID reused since is never signalled. First what it
# is doing goes into the kept logs, leftover-PID.txt: its ps line and its open files
# (lsof, without name lookups). Nothing attaches to it (no sample, no lldb): that can
# raise a system prompt. Then SIGCONT, in case it is stopped, SIGTERM, and SIGKILL after
# 5 s.
end_leftover() {
  case " $(echo $HELPERS) " in *" $1 "*) ;; *) echo "pid $1: not recorded while the app lived, left alone"; return ;; esac
  case "$(ps -ww -o command= -p "$1" 2>/dev/null) " in
    *"--user-data-dir=$MADE_PROFILE "*|*"--user-data-dir=$PROFILE_REAL "*) ;;
    *) echo "pid $1: its command line does not carry this run's profile folder, left alone"; return ;;
  esac
  { date; ps -ww -o pid,ppid,state,pcpu,etime,command -p "$1"; lsof -n -P -p "$1"; } \
    > "$MADE_PROFILE/leftover-$1.txt" 2>&1
  # Which files under the profile it holds open, with lsof's FD column (u: read
  # and write, r: read, w: write).
  held=$(awk -v a="$MADE_PROFILE/" -v b="$PROFILE_REAL/" 'NR > 1 {
      name = $9; for (i = 10; i <= NF; i++) name = name " " $i
      if (index(name, a) == 1) print substr(name, length(a) + 1) " (" $4 ")"
      else if (index(name, b) == 1) print substr(name, length(b) + 1) " (" $4 ")"
    }' "$MADE_PROFILE/leftover-$1.txt" | sort -u | paste -s -d ',' - | sed 's/,/, /g')
  kill -CONT "$1" 2>/dev/null
  kill -TERM "$1" 2>/dev/null && await "$1" 10
  if kill -0 "$1" 2>/dev/null; then
    kill -KILL "$1" 2>/dev/null
    echo "pid $1: ended by SIGKILL; held under the profile: ${held:-nothing}; what it was doing: $MADE_PROFILE/leftover-$1.txt"
  else
    echo "pid $1: ended by SIGTERM; held under the profile: ${held:-nothing}; what it was doing: $MADE_PROFILE/leftover-$1.txt"
  fi
}
# Stops the app this script started: the AppKit quit, then SIGTERM, then SIGKILL.
stop_app() {
  ours "$PID" || return 0
  how="the AppKit quit"
  osascript -l JavaScript -e "ObjC.import('AppKit');
    var app = \$.NSRunningApplication.runningApplicationWithProcessIdentifier($PID);
    app.isNil() ? 'no such app' : String(app.terminate)" > /dev/null 2>&1
  await "$PID" 30
  ours "$PID" && how=SIGTERM && kill -TERM "$PID" && await "$PID" 20
  ours "$PID" && how=SIGKILL && kill -KILL "$PID"
  echo "smoke: stopped the app, by $how"
}
cleanup() {
  status=$?
  [ "$CLEANED" = 1 ] && return
  CLEANED=1
  trap '' INT TERM HUP
  ours "$GUARD_PID" && kill -TERM "$GUARD_PID"
  # First, so a second launch caught mid hand-off cannot become the instance.
  ours "$SECOND_PID" && kill -KILL "$SECOND_PID"
  ours "$REFUSED_PID" && kill -KILL "$REFUSED_PID"
  stop_app
  for helper in $HELPERS; do
    kill -0 "$helper" 2>/dev/null && await "$helper" 6
    kill -0 "$helper" 2>/dev/null && KEEP_LOGS=1 && echo "smoke: a helper outlived the app: $(end_leftover "$helper")"
  done
  ours "$PROBE_PID" && kill -TERM "$PROBE_PID" && await "$PROBE_PID" 10
  [ -n "$MADE_TMP" ] && rm -rf "$MADE_TMP"
  [ -n "$MADE_PROFILE" ] && [ "$KEEP_LOGS" = 0 ] && rm -rf "$MADE_PROFILE"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

[ -x "$BIN" ] || { echo "FAIL  $BIN not found. Run: cargo run --bin bundle"; exit 1; }
# A binary built without this test window mode ignores ASS_TEST_WINDOW and
# opens a normal window, so it is never launched.
ASS_TEST_WINDOW=invisible
grep -q -a -F "Ass: test window mode: $ASS_TEST_WINDOW" "$BIN" ||
  { echo "FAIL  $BIN has no test window mode. Rebuild it: cargo run --bin bundle"; exit 1; }
if [ ! -x "$PROBE" ] || [ "$ROOT/scripts/appprobe.swift" -nt "$PROBE" ]; then
  swiftc -O -o "$PROBE" "$ROOT/scripts/appprobe.swift" || { echo "FAIL  cannot build $PROBE"; exit 1; }
fi

MADE_PROFILE="$(mktemp -d)"
PROFILE_REAL="$(cd "$MADE_PROFILE" && pwd -P)"
ASS_PROFILE_DIR="$MADE_PROFILE"
LOG="$ASS_PROFILE_DIR/smoke.log"
PROBE_LOG="$ASS_PROFILE_DIR/probe.jsonl"
# Chromium's temporary folder for this run, deleted at the end. It holds the
# single-instance socket, whose path has at most 104 bytes; Chromium adds
# "/dev.ass.browser.XXXXXX/SingletonSocket" (39 bytes) and a final NUL.
MADE_TMP="$(mktemp -d /tmp/ass-smoke.XXXXXX)"
MAC_CHROMIUM_TMPDIR="$MADE_TMP"
if [ ${#MAC_CHROMIUM_TMPDIR} -gt 64 ]; then
  rmdir "$MAC_CHROMIUM_TMPDIR"
  echo "FAIL  temporary folder path too long for a socket: $MAC_CHROMIUM_TMPDIR"
  exit 1
fi
export ASS_PROFILE_DIR ASS_TEST_WINDOW MAC_CHROMIUM_TMPDIR

# Started first: it watches every process that runs $BIN with this script as
# its parent, the second launch included.
"$PROBE" --binary "$BIN" --kill > "$PROBE_LOG" 2>&1 &
PROBE_PID=$!
n=0
until grep -q '"k":"ready"' "$PROBE_LOG" 2>/dev/null || [ $n -ge 20 ]; do n=$((n + 1)); sleep 0.5; done
if [ $n -ge 20 ]; then
  kill -TERM "$PROBE_PID"
  rmdir "$MAC_CHROMIUM_TMPDIR"
  KEEP_LOGS=1
  echo "FAIL  the probe did not start; the app was not launched. Log: $PROBE_LOG"
  exit 1
fi

# The window is ordered in at alpha 0, which macOS may report as occluded; an
# occluded page counts as hidden and is throttled. The switch counts occluded
# as visible (src/test_window.rs).
"$BIN" --use-mock-keychain --remote-debugging-port="$PORT" --disable-backgrounding-occluded-windows \
  '--url=data:text/html,<title>ass-smoke</title>' > "$LOG" 2>&1 &
PID=$!

# The probe is the guard. If it reports a violation it did not stop
# ("guard-failed") or ends before this script stops it, this watcher does what
# the probe would have done: SIGKILL to the app, at once. The checks after that
# fail, and check 13 fails on the probe's exit code. It signals nothing but the
# app, and only while the app is this script's child, so it is harmless if the
# script itself is gone.
guarded() { ours "$PROBE_PID" && ! grep -q '"k":"guard-failed"' "$PROBE_LOG"; }
(
  while guarded; do sleep 0.2; done
  echo "FAIL  the probe cannot guard the run: $(grep -m 1 '"k":"guard-failed"' "$PROBE_LOG" || echo "it ended early")"
  ours "$PID" && kill -KILL "$PID" && echo "smoke: stopped the app at once, by SIGKILL"
) &
GUARD_PID=$!

# 1. A page loads, in test window mode.
n=0
until curl -s -m 2 "http://127.0.0.1:$PORT/json" 2>/dev/null | grep -q 'ass-smoke' || [ $n -ge 40 ]; do
  n=$((n + 1)); sleep 0.5
done
if [ $n -lt 40 ]; then pass "page loaded"; else fail "page did not load within 20 s"; fi
if grep -q "^Ass: test window mode: $ASS_TEST_WINDOW\$" "$LOG"; then
  pass "test window mode on"
else
  fail "test window mode not reported"
fi

# 2. Every helper process is sandboxed by the OS.
HELPERS=$(pgrep -P "$PID")
if [ -z "$HELPERS" ]; then
  fail "no helper processes found"
else
  UNSANDBOXED=0
  for helper in $HELPERS; do
    state=$(python3 -I -c "import ctypes, sys; print(ctypes.CDLL('/usr/lib/libSystem.dylib').sandbox_check(int(sys.argv[1]), None, 0))" "$helper")
    [ "$state" = 1 ] || UNSANDBOXED=$((UNSANDBOXED + 1))
  done
  if [ $UNSANDBOXED -eq 0 ]; then
    pass "all $(echo "$HELPERS" | wc -l | tr -d ' ') helper processes sandboxed"
  else
    fail "$UNSANDBOXED helper process(es) not sandboxed"
  fi
fi

# 3. No process was started with the sandbox disabled.
if ps -o command= -p "$PID" $HELPERS | grep -q -- '--no-sandbox'; then
  fail "--no-sandbox present"
else
  pass "no --no-sandbox flag"
fi

# 4. The window is ordered in, so its pages draw, every on-screen window of the
#    app has alpha 0, and each is at layer 8, above normal windows, where test
#    window mode puts it (src/test_window.rs).
WINDOW=$(python3 -I - "$PROBE_LOG" 2>&1 <<'EOF'
import json, sys, time
deadline = time.time() + 10
while True:
    records = [json.loads(line) for line in open(sys.argv[1]) if line.endswith("\n") and line.startswith("{")]
    shown = [r for r in records if r["k"] == "window" and r["onscreen"] and r["bounds"][2] > 0 and r["bounds"][3] > 0]
    if shown or time.time() > deadline:
        break
    time.sleep(0.2)
seen = [r for r in records if r["k"] == "window" and r["onscreen"] and r.get("alpha", 1) > 0]
if seen:
    sys.exit("an on-screen window had alpha %s: %s" % (seen[0].get("alpha"), seen[0]["state"]))
if any(r["k"] == "killed" for r in records):
    sys.exit("the probe killed the app: %s" % [r["reason"] for r in records if r["k"] == "killed"])
if not shown:
    sys.exit("no window was ordered in within 10 s, so the pages may not draw")
wrong = [r for r in shown if r["layer"] != 8]
if wrong:
    sys.exit("an on-screen window was at layer %s, not 8: %s" % (wrong[0]["layer"], wrong[0]["state"]))
print("window at %d,%d %dx%d on screen at layer 8" % tuple(shown[-1]["bounds"]))
EOF
)
if [ $? -eq 0 ]; then
  INVISIBLE=1
  pass "$WINDOW, alpha 0"
else
  INVISIBLE=0
  fail "$WINDOW"
fi

# 5. A second launch on the same profile hands off and exits 0. Skipped if
#    check 4 failed, or if the first instance is gone, since this launch would
#    then stay up.
if [ $INVISIBLE = 0 ]; then
  fail "second launch skipped: check 4 failed, so the hand-off could show a visible window"
elif kill -0 "$PID" 2>/dev/null && guarded; then
  # For check 5b: the first instance's helpers right before the hand-off, each
  # with its --type.
  BEFORE_HANDOFF=$(for helper in $(pgrep -P "$PID"); do
    echo "$helper $(ps -ww -o command= -p "$helper" 2>/dev/null | grep -o -e '--type=[a-z-]*' | head -n 1)"
  done)
  "$BIN" --use-mock-keychain > "$ASS_PROFILE_DIR/second.log" 2>&1 &
  SECOND_PID=$!
  await "$SECOND_PID" 40
  kill -0 "$SECOND_PID" 2>/dev/null && kill -KILL "$SECOND_PID"
  wait "$SECOND_PID"
  SECOND=$?
  if [ $SECOND -eq 0 ] && kill -0 "$PID" 2>/dev/null; then
    HANDED_OFF=1
    pass "second launch handed off"
  else
    fail "second launch exited $SECOND (137: still running after 20 s), or the first instance died"
  fi
else
  fail "first instance not running, or the probe cannot guard: second launch skipped"
fi

# 5b. The second launch ended none of the first instance's helpers (src/leftover.rs
#     runs in it before the hand-off, and must find none: they are children of a
#     living browser process). Every helper recorded right before the hand-off is
#     still running, not a zombie, with the first instance as its parent; and
#     neither launch's log has an "Ass: ended" line.
if [ "$HANDED_OFF" = 1 ]; then
  GONE=$(echo "$BEFORE_HANDOFF" | while read -r helper type; do
    [ -n "$helper" ] || continue
    state=$(ps -o ppid=,state= -p "$helper" 2>/dev/null)
    # The leading parentheses keep bash 3.2 from ending the $( ) at a pattern's ")".
    case "$(echo $state)" in ("$PID "[!Z]*) ;; (*) echo "pid $helper ${type:-type unknown} (now: ${state:-gone})" ;; esac
  done)
  KEPT=$(echo "$BEFORE_HANDOFF" | grep -c .)
  ENDED_HANDOFF=$(grep -h '^Ass: ended' "$LOG" "$ASS_PROFILE_DIR/second.log" 2>/dev/null | head -n 1)
  if [ "$KEPT" -gt 0 ] && [ -z "$GONE" ] && [ -z "$ENDED_HANDOFF" ]; then
    pass "the hand-off left the first instance's $KEPT helpers running, and neither log has an 'Ass: ended' line"
  else
    fail "the hand-off and the first instance's helpers: $KEPT recorded; not running as its children now: ${GONE:-none}; ${ENDED_HANDOFF:-no 'Ass: ended' line}"
  fi
else
  fail "the first instance's helpers across the hand-off: not checked, the second launch did not hand off"
fi

# 6. The shell page and the content page are visible and drawing, after the
#    hand-off showed the window again.
kill -0 "$PID" 2>/dev/null && PAGES=$(python3 -I - "$PORT" 2>&1 <<'EOF'
import base64, json, os, socket, struct, sys, urllib.request

port = int(sys.argv[1])
# Frames drawn in 1 s. A hidden page draws none, so a timer ends the count.
COUNT = """new Promise(done => {
  let frames = 0;
  const start = performance.now();
  const end = () => done(document.visibilityState + ' ' + frames);
  setTimeout(end, 1500);
  const frame = () => { frames++; performance.now() - start < 1000 ? requestAnimationFrame(frame) : end(); };
  requestAnimationFrame(frame);
})"""

def evaluate(url, expression):
    host, _, path = url[len("ws://"):].partition("/")
    sock = socket.create_connection((host.split(":")[0], int(host.split(":")[1])), timeout=10)
    sock.sendall(("GET /%s HTTP/1.1\r\nHost: %s\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                  "Sec-WebSocket-Key: %s\r\nSec-WebSocket-Version: 13\r\n\r\n"
                  % (path, host, base64.b64encode(os.urandom(16)).decode())).encode())
    data = b""
    while b"\r\n\r\n" not in data:
        data += sock.recv(4096)
    data = data.split(b"\r\n\r\n", 1)[1]
    body = json.dumps({"id": 1, "method": "Runtime.evaluate", "params": {
        "expression": expression, "awaitPromise": True, "returnByValue": True}}).encode()
    mask = os.urandom(4)
    sock.sendall(struct.pack("!BBH", 0x81, 0xFE, len(body)) + mask
                 + bytes(b ^ mask[i % 4] for i, b in enumerate(body)))

    def read(size):
        nonlocal data
        while len(data) < size:
            data += sock.recv(65536)
        chunk, data = data[:size], data[size:]
        return chunk
    while True:
        size = read(2)[1] & 0x7F
        size = struct.unpack("!H", read(2))[0] if size == 126 else \
            struct.unpack("!Q", read(8))[0] if size == 127 else size
        reply = json.loads(read(size))
        if reply.get("id") == 1:
            return reply["result"]["result"].get("value")

pages = json.load(urllib.request.urlopen("http://127.0.0.1:%d/json" % port, timeout=10))
for name, prefix in (("shell", "ass://shell/"), ("content", "data:")):
    found = [p for p in pages if p.get("type") == "page" and p["url"].startswith(prefix)]
    print(name, evaluate(found[0]["webSocketDebuggerUrl"], COUNT) if len(found) == 1 else "missing")
EOF
)
DRAWING=$(echo "$PAGES" | awk '$2 == "visible" && $3 >= 30' | wc -l | tr -d ' ')
if ! kill -0 "$PID" 2>/dev/null; then
  fail "pages not checked: the app is not running"
elif [ "$DRAWING" = 2 ]; then
  pass "pages visible and drawing: $(echo $PAGES) frames/s"
else
  fail "pages not visible or not drawing: $(echo $PAGES)"
fi

# 7. Quit through AppKit by PID: exit 0, no helper left, and the socket folder
#    removed. SIGTERM, then SIGKILL, only if that quit does not end the app. A
#    helper still running 3 s later is reported, then ended (end_leftover). The
#    helpers are recorded again right before the quit, so one started after check
#    2 counts too.
SOCKETS_BEFORE=$(ls -d "$MAC_CHROMIUM_TMPDIR"/dev.ass.browser.*/SingletonSocket 2>/dev/null | wc -l | tr -d ' ')
ours "$PID" && HELPERS=$(echo $HELPERS $(pgrep -P "$PID") | tr ' ' '\n' | sort -un | tr '\n' ' ')
if kill -0 "$PID" 2>/dev/null; then
  REPLY=$(osascript -l JavaScript -e "ObjC.import('AppKit');
    var app = \$.NSRunningApplication.runningApplicationWithProcessIdentifier($PID);
    app.isNil() ? 'no such app' : String(app.terminate)" 2>&1)
  await "$PID" 30
  if kill -0 "$PID" 2>/dev/null; then
    kill -TERM "$PID"
    await "$PID" 20
    kill -0 "$PID" 2>/dev/null && kill -KILL "$PID"
    wait "$PID"
    fail "AppKit quit (reply: $REPLY) did not end the app within 15 s; ended by signal, exit code $?"
  else
    wait "$PID"
    CODE=$?
    if [ "$REPLY" = true ] && [ $CODE -eq 0 ]; then
      pass "quit through AppKit, exit code 0"
    else
      fail "quit through AppKit: reply $REPLY, exit code $CODE"
    fi
  fi
else
  wait "$PID"
  fail "the app had already exited, code $?"
fi

pause 6
LEFT=
for helper in $HELPERS; do
  kill -0 "$helper" 2>/dev/null || continue
  line="$(ps -ww -o ppid=,command= -p "$helper" 2>/dev/null)"
  what="$(echo "$line" | grep -o -e '--type=[a-z-]*' -e '--utility-sub-type=[A-Za-z_.]*' | paste -s -d " " -)"
  LEFT="$LEFT; pid $helper, ${what:-type unknown}, parent $(echo $line | cut -d ' ' -f 1): $(end_leftover "$helper")"
done
HELPERS=
if [ -z "$LEFT" ]; then
  pass "no leftover processes"
else
  fail "helper process(es) still running 3 s after the app ended${LEFT}"
fi

ls -lAR "$MAC_CHROMIUM_TMPDIR" > "$ASS_PROFILE_DIR/tmpdir-after-quit.txt" 2>&1
SOCKETS_AFTER=$(ls -d "$MAC_CHROMIUM_TMPDIR"/dev.ass.browser.*/SingletonSocket 2>/dev/null | wc -l | tr -d ' ')
OTHER=$(ls -A "$MAC_CHROMIUM_TMPDIR" | wc -l | tr -d ' ')
if [ "$SOCKETS_BEFORE" -lt 1 ]; then
  fail "no socket folder in $MAC_CHROMIUM_TMPDIR while the app ran, so the cleanup check proves nothing"
elif [ "$SOCKETS_AFTER" -eq 0 ]; then
  pass "socket folder removed at quit ($OTHER other entries left in the run's temporary folder)"
else
  fail "socket folder left behind after the quit"
fi
rm -rf "$MAC_CHROMIUM_TMPDIR"

# 8. The keychain gate (src/keychain.rs) was skipped in both launches, because both
#    passed --use-mock-keychain: Chromium used its stand-in, never the login keychain.
GATE_SKIPPED="^Ass: keychain gate skipped: --use-mock-keychain\$"
if grep -q "$GATE_SKIPPED" "$LOG" && grep -q "$GATE_SKIPPED" "$ASS_PROFILE_DIR/second.log" 2>/dev/null; then
  pass "keychain gate skipped in both launches: --use-mock-keychain"
else
  fail "the keychain gate did not report itself skipped in both launches"
fi

# 9. Test window mode without --use-mock-keychain is refused before NSApp and before any
#    keychain call: exit 64, the refusal on stderr, nothing written to its profile folder.
#    HOME points at an empty folder of this run, so a refusal that failed could not open
#    the real profile.
#    This is the one launch without the switch. It runs only once the first launch has
#    shown that the switch is read, and after that launch, so the probe watches it and the
#    first process the probe watched is the app.
if grep -q "$GATE_SKIPPED" "$LOG"; then
  REFUSED_HOME="$ASS_PROFILE_DIR/refused-home"
  mkdir "$ASS_PROFILE_DIR/refused" "$REFUSED_HOME"
  ASS_PROFILE_DIR="$ASS_PROFILE_DIR/refused" HOME="$REFUSED_HOME" "$BIN" > "$ASS_PROFILE_DIR/refused.log" 2>&1 &
  REFUSED_PID=$!
  await "$REFUSED_PID" 20
  ours "$REFUSED_PID" && kill -KILL "$REFUSED_PID"
  wait "$REFUSED_PID"
  REFUSED=$?
  REFUSAL="^Ass: ASS_TEST_WINDOW refused: --use-mock-keychain is not on the command line\$"
  if [ $REFUSED -eq 64 ] && grep -q "$REFUSAL" "$ASS_PROFILE_DIR/refused.log" &&
    [ -z "$(ls -A "$ASS_PROFILE_DIR/refused")" ] && [ -z "$(ls -A "$REFUSED_HOME")" ]; then
    pass "test window mode without --use-mock-keychain refused: exit code 64, profile folder empty"
  else
    fail "test window mode without --use-mock-keychain: exit code $REFUSED (137: still running after 10 s), log $ASS_PROFILE_DIR/refused.log"
  fi
else
  fail "launch without --use-mock-keychain skipped: the first launch did not show that the switch is read"
fi

# 10. --use-mock-keychain on the default profile is refused before NSApp: exit 64, the
#     refusal on stderr, nothing written. HOME points at an empty folder of this run, so
#     the default profile is that folder's, never the real one. Test window mode stays on
#     with no ASS_PROFILE_DIR: if this refusal ever failed, the mode's own refusal would
#     still end the process, and the message tells the two apart.
mkdir "$ASS_PROFILE_DIR/home"
env -u ASS_PROFILE_DIR HOME="$ASS_PROFILE_DIR/home" "$BIN" --use-mock-keychain \
  > "$ASS_PROFILE_DIR/default-profile.log" 2>&1 &
REFUSED_PID=$!
await "$REFUSED_PID" 20
ours "$REFUSED_PID" && kill -KILL "$REFUSED_PID"
wait "$REFUSED_PID"
REFUSED=$?
REFUSAL="^Ass: --use-mock-keychain refused: the profile is the default profile folder\$"
if [ $REFUSED -eq 64 ] && grep -q "$REFUSAL" "$ASS_PROFILE_DIR/default-profile.log" &&
  [ -z "$(ls -A "$ASS_PROFILE_DIR/home")" ]; then
  pass "--use-mock-keychain on the default profile refused: exit code 64, nothing written"
else
  fail "--use-mock-keychain on the default profile: exit code $REFUSED (137: still running after 10 s), log $ASS_PROFILE_DIR/default-profile.log"
fi

# 11. A relative ASS_PROFILE_DIR is refused before NSApp: exit 64, the refusal on
#     stderr, nothing written. It runs from an empty working folder of this run, with HOME
#     at another, and without --use-mock-keychain: if this refusal ever failed, the
#     refusal of test window mode without the switch (check 9) would still end the
#     process, with another message. So it runs only once the first launch has shown
#     that the switch is read, as check 9 does. exec keeps the app this script's child.
if grep -q "$GATE_SKIPPED" "$LOG"; then
  RELATIVE_WORK="$ASS_PROFILE_DIR/relative-work"
  RELATIVE_HOME="$ASS_PROFILE_DIR/relative-home"
  mkdir "$RELATIVE_WORK" "$RELATIVE_HOME"
  (cd "$RELATIVE_WORK" && exec env ASS_PROFILE_DIR=profile HOME="$RELATIVE_HOME" "$BIN") \
    > "$ASS_PROFILE_DIR/relative.log" 2>&1 &
  REFUSED_PID=$!
  await "$REFUSED_PID" 20
  ours "$REFUSED_PID" && kill -KILL "$REFUSED_PID"
  wait "$REFUSED_PID"
  REFUSED=$?
  REFUSAL="^Ass: ASS_PROFILE_DIR refused: not an absolute path\$"
  if [ $REFUSED -eq 64 ] && grep -q "$REFUSAL" "$ASS_PROFILE_DIR/relative.log" &&
    [ -z "$(ls -A "$RELATIVE_WORK")" ] && [ -z "$(ls -A "$RELATIVE_HOME")" ]; then
    pass "a relative ASS_PROFILE_DIR refused: exit code 64, nothing written"
  else
    fail "a relative ASS_PROFILE_DIR: exit code $REFUSED (137: still running after 10 s), log $ASS_PROFILE_DIR/relative.log"
  fi
else
  fail "launch with a relative ASS_PROFILE_DIR skipped: the first launch did not show that the switch is read"
fi

# 12. An empty ASS_PROFILE_DIR counts as not set, so with --use-mock-keychain it names
#     the default profile and is refused before NSApp: exit 64 and check 10's refusal,
#     not the one for a path that is not absolute; nothing written. HOME at an empty
#     folder of this run. If this refusal ever failed, test window mode, which treats an
#     empty value as missing, would still refuse, with another message.
EMPTY_HOME="$ASS_PROFILE_DIR/empty-home"
mkdir "$EMPTY_HOME"
env ASS_PROFILE_DIR= HOME="$EMPTY_HOME" "$BIN" --use-mock-keychain > "$ASS_PROFILE_DIR/empty.log" 2>&1 &
REFUSED_PID=$!
await "$REFUSED_PID" 20
ours "$REFUSED_PID" && kill -KILL "$REFUSED_PID"
wait "$REFUSED_PID"
REFUSED=$?
REFUSAL="^Ass: --use-mock-keychain refused: the profile is the default profile folder\$"
if [ $REFUSED -eq 64 ] && grep -q "$REFUSAL" "$ASS_PROFILE_DIR/empty.log" && [ -z "$(ls -A "$EMPTY_HOME")" ]; then
  pass "an empty ASS_PROFILE_DIR is the default profile: --use-mock-keychain refused, exit code 64, nothing written"
else
  fail "an empty ASS_PROFILE_DIR: exit code $REFUSED (137: still running after 10 s), log $ASS_PROFILE_DIR/empty.log"
fi

# 12b. A profile folder named in ASS_PROFILE_DIR must already exist: Chromium is never
#     left to make it (src/profile.rs). One that does not exist is refused before NSApp:
#     exit 64, the refusal on stderr, nothing written. It runs with HOME at an empty
#     folder of this run and the profile folder under it, so a refusal that failed could
#     only make a throwaway profile there; a start that is still running after 4 s is
#     ended at once with the AppKit quit (SIGKILL after 30 s more).
#     Not checked by a launch, on purpose: a path that is a file, or that cannot be read.
#     If that refusal failed, Chromium would carry on in a folder of its own choosing
#     outside this run, which a temporary HOME is not known to contain. Unit tests cover
#     both (src/profile.rs).
MISSING_HOME="$ASS_PROFILE_DIR/missing-home"
mkdir "$MISSING_HOME"
env ASS_PROFILE_DIR="$MISSING_HOME/profile" HOME="$MISSING_HOME" \
  "$BIN" --use-mock-keychain > "$ASS_PROFILE_DIR/missing.log" 2>&1 &
REFUSED_PID=$!
await "$REFUSED_PID" 4
if ours "$REFUSED_PID"; then
  osascript -l JavaScript -e "ObjC.import('AppKit');
    var app = \$.NSRunningApplication.runningApplicationWithProcessIdentifier($REFUSED_PID);
    app.isNil() ? 'no such app' : String(app.terminate)" > /dev/null 2>&1
  await "$REFUSED_PID" 30
  ours "$REFUSED_PID" && kill -KILL "$REFUSED_PID"
fi
wait "$REFUSED_PID"
REFUSED=$?
REFUSAL="^Ass: ASS_PROFILE_DIR refused: the folder does not exist\$"
if [ $REFUSED -eq 64 ] && grep -q "$REFUSAL" "$ASS_PROFILE_DIR/missing.log" && [ -z "$(ls -A "$MISSING_HOME")" ]; then
  pass "an ASS_PROFILE_DIR that does not exist refused: exit code 64, the folder still absent, nothing written"
else
  fail "an ASS_PROFILE_DIR that does not exist: exit code $REFUSED (not 64: it was still running after 4 s and was quit), log $ASS_PROFILE_DIR/missing.log"
fi

# 13. Over the whole run, no launch took the screen or the keyboard: never
#    frontmost, active or the menu bar owner, no on-screen window above alpha
#    0, and the activation policy stayed Accessory once the mode set it (-1 is
#    allowed only as an exiting launch's last state; see below).
ours "$GUARD_PID" && kill -TERM "$GUARD_PID"
wait "$GUARD_PID"
kill -TERM "$PROBE_PID"
wait "$PROBE_PID"
PROBE_CODE=$?
[ $PROBE_CODE -eq 0 ] || fail "the probe exited $PROBE_CODE (4: it could not stop a violation; see guard-failed in $PROBE_LOG)"
SCREEN=$(python3 -I - "$PROBE_LOG" 2>&1 <<'EOF'
import json, sys
records = [json.loads(line) for line in open(sys.argv[1]) if line.startswith("{")]
summary = [r for r in records if r["k"] == "summary"]
if not summary:
    sys.exit("no probe summary")
summary = summary[0]
violations = {k: v for k, v in summary["counts"].items() if k.startswith("violation-")}
if violations or any(w["killed"] for w in summary["watched"]):
    sys.exit("violations %s; killed by the probe: %s" % (violations, [w["pid"] for w in summary["watched"] if w["killed"]]))
if not summary["watched"]:
    sys.exit("the probe watched no launch")
# The app registers as a regular app (0) and the mode then sets Accessory (1).
# Nothing may change it after that, with one exception: -1, which is no policy at
# all (AppKit has 0, 1 and 2), read from a process that is exiting, once its
# helpers are gone. It is allowed only as the last state of that process: no
# later policy or activation record of it, and in every later sample it reads -1,
# is not active, and is neither frontmost nor the menu bar owner.
regular = []
dropped = 0
for watched in summary["watched"]:
    pid = watched["pid"]
    mine = [r for r in records if r.get("pid") == pid]
    changes = [r for r in mine if r["k"] == "policy"]
    order = [r["to"] for r in changes]
    if 1 not in order:
        if watched is summary["watched"][0]:
            sys.exit("the app never reached policy 1 (accessory): %s" % order)
        continue
    after = order[order.index(1):]
    if after[-1] == -1 and all(p == 1 for p in after[:-1]):
        gone_at = changes[-1]["t"]
        later = [r for r in mine if r["t"] > gone_at and r["k"] in ("policy", "active", "registered")]
        samples = [(r, app) for r in records if r["k"] == "s" and r["t"] >= gone_at
                   for app in r["apps"] if app["pid"] == pid]
        if later or not samples or any(app.get("policy", -1) != -1 or app.get("active")
                                       or r["front"] == pid or r["menu"] == pid for r, app in samples):
            sys.exit("policy changed after the mode set it: %s, and -1 was not that process's last state" % order)
        dropped += 1
    elif any(p != 1 for p in after):
        sys.exit("policy changed after the mode set it: %s" % order)
    set_at = changes[order.index(1)]["t"]
    seen = [r["t"] for r in mine if (r["k"] == "registered" and r["policy"] == 0)
            or (r["k"] == "policy" and r["to"] == 0 and r["t"] < set_at)]
    regular.append("%d ms" % round(1000 * (set_at - min(seen))) if seen else "not seen")
if not summary["counts"].get("onscreen-alpha0"):
    sys.exit("no sample had a window ordered in at alpha 0, so the window check proved nothing")
print("%d launches, %d samples (longest gap %d ms), %d with the window ordered in at alpha 0, policy kept"
      " (read as -1 as the last state of %d launch(es) while exiting); seen as a regular app before that: %s"
      % (len(summary["watched"]), summary["counts"].get("samples", 0), round(1000 * summary["maxSampleGap"]),
         summary["counts"]["onscreen-alpha0"], dropped, ", ".join(regular)))
EOF
)
if [ $? -eq 0 ]; then
  pass "never frontmost or active, no visible window: $SCREEN"
else
  fail "the app took the screen or the keyboard, or the probe failed: $SCREEN"
fi

# The file and device pickers were blocked at startup (src/app.rs prints this line when not).
if grep -q 'file and device access could not be blocked' "$LOG" "$ASS_PROFILE_DIR/second.log" 2>/dev/null; then
  fail "the app reported that file and device access could not be blocked"
else
  pass "file and device access blocked: no refusal in either launch's log"
fi

# 14. Each window the app showed stayed where test window mode puts it. After every
#     show() the app reads the window back and prints one line (src/test_window.rs,
#     shown): every such line in every launch's log carries exactly these values. The
#     first launch shows its window twice, at the start and when the second launch
#     hands off; no other launch shows one. And the probe saw every on-screen window of
#     the app at layer 8 over the whole run, not only up to check 4.
SHOWN_LINE="Ass: test window shown: level 8, behaviour 0x40151, ignores the mouse true, alpha 0"
SHOWN_WANT=$((1 + HANDED_OFF))
SHOWN_FIRST=$(grep -c -x -F "$SHOWN_LINE" "$LOG")
SHOWN_ALL=$(cat "$ASS_PROFILE_DIR"/*.log | grep -c '^Ass: test window shown:')
SHOWN_OTHER=$(cat "$ASS_PROFILE_DIR"/*.log | grep '^Ass: test window shown:' | grep -v -x -F "$SHOWN_LINE" | head -n 1)
LAYERS=$(python3 -I -c 'import json, sys
records = [json.loads(line) for line in open(sys.argv[1]) if line.startswith("{")]
print(" ".join(sorted({str(r["layer"]) for r in records if r["k"] == "window" and r["onscreen"]
                       and r["bounds"][2] > 0 and r["bounds"][3] > 0})) or "none")' "$PROBE_LOG" 2>&1)
if [ "$SHOWN_FIRST" = "$SHOWN_WANT" ] && [ "$SHOWN_ALL" = "$SHOWN_WANT" ] && [ "$LAYERS" = 8 ]; then
  pass "the window was shown $SHOWN_WANT times, each read back at level 8, behaviour 0x40151, ignoring the mouse, alpha 0; the probe saw it at layer 8 throughout"
else
  fail "the window's settings after show(): $SHOWN_FIRST exact line(s) in the first launch's log and $SHOWN_ALL in all logs, $SHOWN_WANT expected; other: ${SHOWN_OTHER:-none}; layers the probe saw on screen: $LAYERS"
fi

# 15. The cookie store was flushed once for the one quit this script makes, and the
#     flush answered within its bound (src/flush.rs): exactly one line "answered in N
#     ms, at S.mmm" in the first launch's log, matched to the end of the line, so the
#     line for an answer after the bound does not count. No other flush line in any
#     log: the second launch hands off and the refused ones stop before Chromium.
FLUSH_ANSWERED='^Ass: quit: the cookie flush answered in [0-9]+ ms, at [0-9]+\.[0-9]{3}$'
FLUSH_FIRST=$(grep -c -E -x "$FLUSH_ANSWERED" "$LOG")
FLUSH_ALL=$(cat "$ASS_PROFILE_DIR"/*.log | grep -c '^Ass: quit: the cookie flush')
FLUSH_LINE=$(grep -m 1 '^Ass: quit: the cookie flush' "$LOG")
if [ "$FLUSH_FIRST" = 1 ] && [ "$FLUSH_ALL" = 1 ]; then
  pass "one cookie flush at the quit, answered in $(echo "$FLUSH_LINE" | sed -E 's/.* answered in ([0-9]+) ms.*/\1/') ms"
else
  fail "cookie flush at the quit: $FLUSH_FIRST answered line(s) in the first launch's log and $FLUSH_ALL flush line(s) in all logs, 1 and 1 expected; first: ${FLUSH_LINE:-none}"
fi

# 16. No launch ended a helper left by an earlier run (src/leftover.rs): every launch
#     here starts on a profile of this run, so such a line means the app ended a
#     process it should not have found.
ENDED_LINE=$(cat "$ASS_PROFILE_DIR"/*.log | grep -m 1 '^Ass: ended')
if [ -z "$ENDED_LINE" ]; then
  pass "no launch ended a helper left by an earlier run"
else
  fail "a launch ended helpers left by an earlier run: $ENDED_LINE"
fi

if [ $FAILED -eq 0 ]; then
  rm -rf "$ASS_PROFILE_DIR"
  echo "smoke: all checks passed"
else
  KEEP_LOGS=1
  echo "smoke: FAILED. Logs kept in $ASS_PROFILE_DIR"
fi
exit $FAILED
