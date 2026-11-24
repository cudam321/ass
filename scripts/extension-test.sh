#!/bin/sh
# Extension run: which extension features work in our
# Alloy-style views, check by check. The contract is tests/extension/checks.json:
# every check id, what each one proves, and the files a run leaves.
#
#   scripts/extension-test.sh             # tests target/debug/app.noindex/Ass.app
#   scripts/extension-test.sh release     # tests target/release/app.noindex/Ass.app
#   scripts/extension-test.sh --dry-run   # no app: checks the pieces of the run
#
# One launch: the test extension (tests/extension) is loaded unpacked into a
# fresh profile, and the content view opens the test pages from a page server
# on 127.0.0.1. The extension's service worker runs its suite on its own. This
# script does through the debug port what the extension cannot do itself: it
# opens a popup for the tabs checks, waits for the worker to go idle and wake
# again, and loads the extension's popup and options pages in the content view.
# Then it quits the app through AppKit by PID and checks the exit, the helper
# processes and the profile. Records arrive on two paths, the page server and
# the debug port; the report merges them.
#
# It prints its inputs first: the app, the test window setting, the debug port,
# the page server, the extension's copy and id, the load switches, the risky
# setting, the launch limit and the run directory.
#
# Environment:
#   ASS_TEST_WINDOW             passed to the app untouched. When it is not set
#                               the run refuses to launch, because the app's
#                               windows would show on the screen, unless
#                               ASS_EXT_TEST_VISIBLE_OK=1.
#   ASS_EXT_TEST_VISIBLE_OK     1 allows launches whose windows show.
#   ASS_EXT_TEST_PORT           debug port, 9391 to 9397 (default 9391). Must be free.
#   ASS_EXT_TEST_RISKY          1 (default) runs the risky checks; 0 records them not-run.
#   ASS_EXT_TEST_MAX_LAUNCHES   most launches in one run (default 3).
#   ASS_EXT_TEST_OUT            run directory, new or empty (default: a new folder
#                               under $TMPDIR). Always kept: it is the evidence.
#   ASS_EXT_TEST_LOAD_SWITCHES  replaces LOAD_SWITCHES (below). Set it empty to
#                               launch without the extension. The extension's
#                               copy is $ASS_EXT_TEST_OUT/extension.
#
# Launch budget: one launch of about 3 to 4 minutes (about 7 when every wait
# runs out), plus one more for each launch whose browser process ends during a
# check, up to ASS_EXT_TEST_MAX_LAUNCHES. The next launch skips only the checks
# that crashed it and runs everything else again.
#
# Warning: tabs.create, tabs.duplicate, windows.create, action.openPopup and
# runtime.openOptionsPage may open windows the test window mode does not hide:
# it hides only the windows this app creates itself (src/test_window.rs).
# ASS_EXT_TEST_RISKY=0 leaves them out. The window probe (target/appprobe, built
# from scripts/appprobe.swift as scripts/smoke.sh does) runs as this script's
# child for the whole run and ends a launch the moment it is frontmost, active
# or the menu bar owner, or has an on-screen window above alpha 0. The check in
# flight is then recorded as fails, with what showed, and the next launch skips
# it. With ASS_EXT_TEST_VISIBLE_OK=1 the probe only watches.
#
# Exit code: the report's (0, or 3 when no extension record arrived); 2 when the
# run could not start ("NOT RUN: <reason>") or was stopped. --dry-run exits 0
# only if every step passes.

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
EXT_SRC="$ROOT/tests/extension"
CHECKS="$EXT_SRC/checks.json"
CDP="$ROOT/scripts/extension-test-cdp.py"
SERVER_PY="$ROOT/scripts/extension-test-server.py"
REPORT_PY="$ROOT/scripts/extension-test-report.py"
TMP_BASE="${TMPDIR:-/tmp}"
TMP_BASE="${TMP_BASE%/}"
MODE=
RUN=
APP_PID=
SERVER_PID=
CTMP=

cdp() { python3 -I "$CDP" "$@"; }
report() { python3 -I "$REPORT_PY" "$@"; }
not_run() { echo "NOT RUN: $*"; exit 2; }

# running PID: the process exists and is not a zombie. The app is this shell's
# child and stays a zombie until the shell reaps it.
running() {
  kill -0 "$1" 2>/dev/null || return 1
  case "$(ps -o stat= -p "$1" 2>/dev/null | tr -d ' ')" in Z*|'') return 1 ;; esac
}

# port_open PORT: something accepts connections on 127.0.0.1:PORT.
port_open() {
  python3 -I -c 'import socket, sys
s = socket.socket()
s.settimeout(1)
sys.exit(0 if s.connect_ex(("127.0.0.1", int(sys.argv[1]))) == 0 else 1)' "$1"
}

# stop_server: SIGTERM by PID, SIGKILL after 5 s.
stop_server() {
  [ -n "$SERVER_PID" ] || return 0
  kill -TERM "$SERVER_PID" 2>/dev/null
  n=0
  while running "$SERVER_PID" && [ $n -lt 10 ]; do n=$((n + 1)); sleep 0.5; done
  running "$SERVER_PID" && kill -KILL "$SERVER_PID" 2>/dev/null
  wait "$SERVER_PID" 2>/dev/null
  SERVER_PID=
}

# ---- Dry run: every piece of the run, without the app ----

dry_run() {
  DRY_FAILED=0
  pass() { echo "PASS  $1"; }
  fail() { echo "FAIL  $1"; DRY_FAILED=1; }
  DRY="$(mktemp -d "$TMP_BASE/ass-ext-dry.XXXXXX")" || { echo "FAIL  cannot create a temporary folder"; exit 1; }
  trap stop_server EXIT
  trap 'exit 130' INT TERM

  # 1. Every script of the extension and its pages parses.
  if ! command -v node > /dev/null 2>&1; then
    fail "1 JavaScript syntax: node not found, so nothing was checked"
  else
    find "$EXT_SRC" -type f -name '*.js' | sort > "$DRY/js.txt"
    total=0
    broken=0
    while IFS= read -r file; do
      total=$((total + 1))
      if ! node --check "$file" > "$DRY/node.txt" 2>&1; then
        broken=$((broken + 1))
        echo "      $(head -n 1 "$DRY/node.txt") $(grep -m 1 'Error' "$DRY/node.txt")"
      fi
    done < "$DRY/js.txt"
    if [ $total -eq 0 ]; then
      fail "1 JavaScript syntax: no .js file under tests/extension"
    elif [ $broken -eq 0 ]; then
      pass "1 JavaScript syntax: node --check passes on $total files"
    else
      fail "1 JavaScript syntax: $broken of $total files fail node --check"
    fi
  fi

  # 2. The JSON files parse, and every file the manifest names exists.
  python3 -I - "$EXT_SRC" > "$DRY/files.txt" 2>&1 <<'EOF'
import json, os, sys
root = sys.argv[1]
problems, named = [], []

def load(name):
    try:
        with open(os.path.join(root, name)) as f:
            return json.load(f)
    except (OSError, ValueError) as error:
        problems.append("%s: %s" % (name, error))

manifest = load("manifest.json")
load("rules.json")
load("checks.json")
if isinstance(manifest, dict):
    try:
        background = manifest.get("background", {})
        if not background.get("service_worker"):
            problems.append("manifest.json names no background service worker")
        named.append(background.get("service_worker"))
        for script in manifest.get("content_scripts", []):
            named += script.get("js", []) + script.get("css", [])
        action = manifest.get("action", {})
        named.append(action.get("default_popup"))
        icon = action.get("default_icon")
        named += list(icon.values()) if isinstance(icon, dict) else [icon]
        named += list(manifest.get("icons", {}).values())
        named.append(manifest.get("options_page"))
        named.append(manifest.get("options_ui", {}).get("page"))
        for rules in manifest.get("declarative_net_request", {}).get("rule_resources", []):
            named.append(rules.get("path"))
    except (AttributeError, TypeError) as error:
        problems.append("manifest.json has an unexpected shape: %s" % error)
named = {n for n in named if n}
missing = sorted(n for n in named if not os.path.isfile(os.path.join(root, n.lstrip("/"))))
if missing:
    problems.append("named in manifest.json but missing: " + ", ".join(missing))
print("; ".join(problems) if problems else "%d files named, all present" % len(named))
sys.exit(1 if problems else 0)
EOF
  if [ $? -eq 0 ]; then
    pass "2 manifest.json, rules.json and checks.json parse; $(cat "$DRY/files.txt")"
  else
    fail "2 files: $(cat "$DRY/files.txt")"
  fi

  # 3. The manifest key gives the id the contract names.
  got="$(cdp ext-id "$EXT_SRC/manifest.json")"
  want="$(python3 -I -c 'import json, sys; print(json.load(open(sys.argv[1]))["extension_id"])' "$CHECKS" 2>&1)"
  if [ "$got" = "$want" ]; then
    pass "3 extension id $got, as checks.json says"
  else
    fail "3 extension id: the manifest key gives [$got], checks.json says [$want]"
  fi

  # 4. The check ids used in the code match the contract.
  if [ ! -f "$REPORT_PY" ]; then
    fail "4 check ids: scripts/extension-test-report.py not found"
  else
    report lint "$CHECKS" "$EXT_SRC" "$ROOT/scripts/extension-test.sh" "$CDP" > "$DRY/lint.txt" 2>&1
    code=$?
    sed 's/^/      /' "$DRY/lint.txt"
    if [ $code -eq 0 ]; then pass "4 check ids match checks.json"; else fail "4 check ids: lint exit code $code"; fi
  fi

  # 5. The page server answers as the contract says, and stops on SIGTERM.
  if [ ! -f "$SERVER_PY" ]; then
    fail "5 page server: scripts/extension-test-server.py not found"
  else
    dry_server
  fi

  # 6. The report's own expectations.
  if [ ! -f "$REPORT_PY" ]; then
    fail "6 report self-test: scripts/extension-test-report.py not found"
  else
    report selftest "$CHECKS" > "$DRY/selftest.txt" 2>&1
    code=$?
    sed 's/^/      /' "$DRY/selftest.txt"
    if [ $code -eq 0 ]; then pass "6 report self-test"; else fail "6 report self-test: exit code $code"; fi
  fi

  if [ $DRY_FAILED -eq 0 ]; then
    rm -rf "$DRY"
    echo "dry run: all steps passed"
    exit 0
  fi
  echo "dry run: FAILED. Files kept in $DRY"
  exit 1
}

dry_server() {
  mkdir "$DRY/run"
  echo 1 > "$DRY/run/launch"
  python3 -I "$SERVER_PY" "$EXT_SRC/pages" "$DRY/run" > "$DRY/server.log" 2>&1 &
  SERVER_PID=$!
  sport=
  n=0
  while [ $n -lt 20 ]; do
    sport="$(cat "$DRY/run/server.port" 2>/dev/null)"
    case "$sport" in ''|*[!0-9]*) sport= ;; *) break ;; esac
    running "$SERVER_PID" || break
    n=$((n + 1))
    sleep 0.5
  done
  if [ -z "$sport" ]; then
    fail "5 page server: no port in server.port within 10 s (log: $DRY/server.log)"
    stop_server
    return
  fi
  u="http://127.0.0.1:$sport"
  get() { curl -s --noproxy '*' -m 5 "$@"; }

  code="$(get -D "$DRY/start.head" -o /dev/null -w '%{http_code}' "$u/start.html")"
  if [ "$code" = 200 ] && grep -q -i '^set-cookie: *ass_server_cookie=1' "$DRY/start.head" 2>/dev/null; then
    pass "5 page server on port $sport: GET /start.html 200, sets ass_server_cookie=1"
  else
    fail "5 page server: GET /start.html: status $code, $(grep -i '^set-cookie' "$DRY/start.head" 2>/dev/null | tr -d '\r' | head -n 1)"
  fi

  printf '{"id": "ctl.page.ready", "by": "page", "launch": 1, "t": %s000, "detail": "dry run"}' \
    "$(date +%s)" > "$DRY/record.json"
  code="$(get -o /dev/null -w '%{http_code}' -H 'Content-Type: text/plain' \
    --data-binary @"$DRY/record.json" "$u/__ass/record")"
  if [ "$code" = 204 ]; then pass "5 page server: POST /__ass/record 204"; else fail "5 page server: POST /__ass/record: status $code"; fi

  has="$(get "$u/__ass/has?id=ctl.page.ready" | python3 -I -c 'import json, sys; print(json.load(sys.stdin).get("has"))' 2>&1)"
  if [ "$has" = True ]; then pass "5 page server: GET /__ass/has says true"; else fail "5 page server: GET /__ass/has: $has"; fi

  # tabs.reload is judged by this count growing: a second ctl.page.ready must count as 2.
  get -o /dev/null -H 'Content-Type: text/plain' --data-binary @"$DRY/record.json" "$u/__ass/record"
  count="$(get "$u/__ass/count?id=ctl.page.ready" | python3 -I -c 'import json, sys; print(json.load(sys.stdin).get("count"))' 2>&1)"
  if [ "$count" = 2 ]; then pass "5 page server: GET /__ass/count says 2 after two records"; else fail "5 page server: GET /__ass/count: $count (expected 2)"; fi

  body="$(get "$u/__dnr/control.txt")"
  if [ "$body" = control ]; then pass "5 page server: GET /__dnr/control.txt says control"; else fail "5 page server: GET /__dnr/control.txt: [$body]"; fi

  code="$(get -X OPTIONS -D "$DRY/options.head" -o /dev/null -w '%{http_code}' "$u/__ass/record")"
  if [ "$code" = 204 ] && grep -q -i '^access-control-allow-origin: *\*' "$DRY/options.head" 2>/dev/null; then
    pass "5 page server: OPTIONS 204 with Access-Control-Allow-Origin: *"
  else
    fail "5 page server: OPTIONS: status $code, $(grep -i '^access-control-allow-origin' "$DRY/options.head" 2>/dev/null | tr -d '\r')"
  fi

  kill -TERM "$SERVER_PID"
  n=0
  while running "$SERVER_PID" && [ $n -lt 10 ]; do n=$((n + 1)); sleep 0.5; done
  if running "$SERVER_PID"; then
    fail "5 page server: still running 5 s after SIGTERM"
    stop_server
  else
    wait "$SERVER_PID"
    status=$?
    SERVER_PID=
    if port_open "$sport"; then
      fail "5 page server: exited on SIGTERM (status $status), but port $sport is still open"
    else
      pass "5 page server: exited on SIGTERM (status $status), port $sport closed"
    fi
  fi

  requests=0
  [ -f "$DRY/run/requests.jsonl" ] && requests=$(wc -l < "$DRY/run/requests.jsonl" | tr -d ' ')
  records=0
  [ -f "$DRY/run/records.jsonl" ] && records=$(grep -c 'ctl.page.ready' "$DRY/run/records.jsonl")
  if [ "$requests" = 7 ] && [ "$records" = 2 ]; then
    pass "5 page server: requests.jsonl has the 7 requests, records.jsonl the 2 records"
  else
    fail "5 page server: requests.jsonl has $requests lines (expected 7), records.jsonl $records records (expected 2)"
  fi
}

# ---- Arguments, and the gate that keeps windows off the screen ----

case "$1" in
  --dry-run) [ $# -eq 1 ] && dry_run ;;
  ''|debug|release) [ $# -le 1 ] && MODE="${1:-debug}" ;;
esac
[ -n "$MODE" ] || { echo "usage: scripts/extension-test.sh [debug|release] | --dry-run"; exit 2; }

if [ -z "$ASS_TEST_WINDOW" ] && [ "$ASS_EXT_TEST_VISIBLE_OK" != 1 ]; then
  not_run "ASS_TEST_WINDOW is not set, so the app's windows would show on the screen. Set ASS_TEST_WINDOW=invisible, or ASS_EXT_TEST_VISIBLE_OK=1 to launch with visible windows."
fi

PORT="${ASS_EXT_TEST_PORT:-9391}"
RISKY="${ASS_EXT_TEST_RISKY:-1}"
MAX="${ASS_EXT_TEST_MAX_LAUNCHES:-3}"
case "$PORT" in 939[1-7]) ;; *) not_run "ASS_EXT_TEST_PORT must be 9391 to 9397, not $PORT" ;; esac
case "$RISKY" in 0|1) ;; *) not_run "ASS_EXT_TEST_RISKY must be 0 or 1, not $RISKY" ;; esac
case "$MAX" in ''|0*|*[!0-9]*) not_run "ASS_EXT_TEST_MAX_LAUNCHES must be a whole number from 1, not $MAX" ;; esac

APP="$ROOT/target/$MODE/app.noindex/Ass.app"
BIN="$APP/Contents/MacOS/Ass"
[ -x "$BIN" ] || not_run "$BIN not found. Build it: cargo run --bin bundle"
# A binary that does not know this test window setting ignores it and shows
# its windows (scripts/smoke.sh checks the same way).
if [ -n "$ASS_TEST_WINDOW" ] && [ "$ASS_EXT_TEST_VISIBLE_OK" != 1 ] &&
  ! grep -q -a -F "Ass: test window mode: $ASS_TEST_WINDOW" "$BIN"; then
  not_run "$BIN does not know ASS_TEST_WINDOW=$ASS_TEST_WINDOW, so its windows would show. Rebuild it: cargo run --bin bundle"
fi
# The probe knows the app by the path the kernel reports for its executable
# (proc_pidpath). For a file with several hard links that is whichever name was
# looked up last, so the probe can stop recognising the app and refuse to end
# it: 1507 refused kills on 2026-11-24, with windows on screen. A hard-linked
# binary is refused; copy it with ditto or cp -c.
LINKS="$(stat -f %l "$BIN" 2>/dev/null)"
[ "$LINKS" = 1 ] || not_run "$BIN has $LINKS hard links, so the window probe could not end it. Copy the app with ditto or cp -c."
PROBE="$ROOT/target/appprobe"
if [ ! -x "$PROBE" ] || [ "$ROOT/scripts/appprobe.swift" -nt "$PROBE" ]; then
  not_run "$PROBE is missing or older than scripts/appprobe.swift. Build it: scripts/smoke.sh does, or swiftc -O -o target/appprobe scripts/appprobe.swift"
fi
port_open "$PORT" && not_run "something already listens on 127.0.0.1:$PORT (ASS_EXT_TEST_PORT)"
for piece in "$CDP" "$SERVER_PY" "$REPORT_PY" "$CHECKS" "$EXT_SRC/manifest.json"; do
  [ -f "$piece" ] || not_run "$piece is missing"
done

# The run directory. Its path may hold no space: LOAD_SWITCHES is split on spaces.
if [ -n "$ASS_EXT_TEST_OUT" ]; then
  case "$ASS_EXT_TEST_OUT" in *[!A-Za-z0-9/._-]*) not_run "ASS_EXT_TEST_OUT may hold only letters, digits and / . _ -" ;; esac
  mkdir -p "$ASS_EXT_TEST_OUT" && RUN="$(cd "$ASS_EXT_TEST_OUT" && pwd)" || not_run "cannot use $ASS_EXT_TEST_OUT"
  [ -z "$(ls -A "$RUN")" ] || not_run "$RUN is not empty; a run needs a new or empty folder"
else
  RUN="$(mktemp -d "$TMP_BASE/ass-ext-test.XXXXXX")" || not_run "cannot create a run directory under $TMP_BASE"
fi
case "$RUN" in *[!A-Za-z0-9/._-]*) not_run "the run directory's path may hold only letters, digits and / . _ -: $RUN" ;; esac

# From here on, something may be running. On any exit, Ctrl+C or signal, the
# trap ends what this script started and removes the folders it made (the
# profiles and Chromium's temporary folder); the run directory is the evidence
# and is kept. As scripts/smoke.sh: a PID is signalled only while it is still
# this script's child, the trap runs once, and signals during it are ignored.
CLEANED=0
PROBE_PID=
ours() { [ -n "$1" ] && [ "$(ps -o ppid= -p "$1" 2>/dev/null | tr -d ' ')" = "$$" ] && running "$1"; }
cleanup() {
  status=$?
  [ "$CLEANED" = 1 ] && return
  CLEANED=1
  trap '' INT TERM HUP
  if ours "$APP_PID"; then
    echo "Quitting the app (pid $APP_PID) through AppKit"
    cdp quit "$APP_PID" > /dev/null
    n=0
    while running "$APP_PID" && [ $n -lt 60 ]; do n=$((n + 1)); sleep 0.5; done
    if ours "$APP_PID"; then
      echo "Still running 30 s after the quit request: SIGKILL"
      kill -KILL "$APP_PID" 2>/dev/null
    fi
    wait "$APP_PID" 2>/dev/null
  fi
  APP_PID=
  stop_probe
  stop_server
  drop_chromium_tmp
  rm -rf "$RUN"/profile-*
  echo "Run directory (kept): $RUN"
  exit "$status"
}

# stop_probe: SIGTERM, so the probe writes its summary line, then waits for it.
stop_probe() {
  ours "$PROBE_PID" || { PROBE_PID=; return 0; }
  kill -TERM "$PROBE_PID" 2>/dev/null
  n=0
  while running "$PROBE_PID" && [ $n -lt 10 ]; do n=$((n + 1)); sleep 0.5; done
  ours "$PROBE_PID" && kill -KILL "$PROBE_PID" 2>/dev/null
  wait "$PROBE_PID" 2>/dev/null
  PROBE_PID=
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

# The run's own copy of the extension: config.js is written into it before each
# launch. The repository's copy is never written.
cp -R "$EXT_SRC" "$RUN/extension" || not_run "cannot copy $EXT_SRC"
EXT_ID="$(cdp ext-id "$RUN/extension/manifest.json")"
WANT_ID="$(python3 -I -c 'import json, sys; print(json.load(open(sys.argv[1]))["extension_id"])' "$CHECKS" 2>&1)"
[ "$EXT_ID" = "$WANT_ID" ] || not_run "the manifest key gives extension id [$EXT_ID], checks.json says [$WANT_ID]"

# How the extension is loaded: this one variable. kLoadExtension = "load-extension"
# is Chromium's own switch (extensions/common/switches.cc:35). At tag
# 156.0.8078.100, chrome/browser/extensions/extension_service.cc:412-449 honors it
# in builds without GOOGLE_CHROME_BRANDING (CEF builds are not Chrome-branded),
# unless Enhanced Safe Browsing is on or the ExtensionInstallTypeBlocklist policy
# lists "command_line". No CEF patch or libcef file touches it (grep of CEF
# e603d97, patch/ and libcef/). Our spike profile on this pin recorded the
# extension with location 8 (kCommandLine). If it ever stops working, the
# candidate is --disable-extensions-except=<path>: the same loader, gated by
# kDisableDisableExtensionsExceptCommandLineSwitch, which is disabled by default
# outside Chrome-branded builds (extensions/common/extension_features.cc:218-224).
# The variable is split on spaces on purpose, so it can hold several switches;
# the run directory's path holds no space (checked above).
if [ "${ASS_EXT_TEST_LOAD_SWITCHES+set}" = set ]; then
  LOAD_SWITCHES="$ASS_EXT_TEST_LOAD_SWITCHES"
else
  LOAD_SWITCHES="--load-extension=$RUN/extension"
fi

# The page server, for every launch of the run.
echo 1 > "$RUN/launch"
python3 -I "$SERVER_PY" "$RUN/extension/pages" "$RUN" > "$RUN/server.log" 2>&1 &
SERVER_PID=$!
SPORT=
n=0
while [ $n -lt 20 ]; do
  SPORT="$(cat "$RUN/server.port" 2>/dev/null)"
  case "$SPORT" in ''|*[!0-9]*) SPORT= ;; *) break ;; esac
  running "$SERVER_PID" || break
  n=$((n + 1))
  sleep 0.5
done
[ -n "$SPORT" ] || not_run "the page server did not start within 10 s (log: $RUN/server.log)"
SERVER="http://127.0.0.1:$SPORT"
XSERVER="http://localhost:$SPORT"

# The window probe, started before any launch as this script's direct child (it
# exits on its own if its parent changes). It watches every launch of $BIN by
# this script. probe.jsonl is kept in the run directory.
PROBE_KILL=--kill
[ "$ASS_EXT_TEST_VISIBLE_OK" = 1 ] && PROBE_KILL=
"$PROBE" --binary "$BIN" $PROBE_KILL > "$RUN/probe.jsonl" 2>&1 &
PROBE_PID=$!
n=0
until grep -q '"k":"ready"' "$RUN/probe.jsonl" 2>/dev/null || [ $n -ge 20 ] || ! running "$PROBE_PID"; do
  n=$((n + 1))
  sleep 0.5
done
grep -q '"k":"ready"' "$RUN/probe.jsonl" 2>/dev/null ||
  not_run "the window probe did not start within 10 s, so nothing was launched (log: $RUN/probe.jsonl)"

echo "Extension run"
echo "  app:              $BIN"
echo "  window probe:     pid $PROBE_PID, ${PROBE_KILL:-watching only (ASS_EXT_TEST_VISIBLE_OK=1)}"
echo "  test window:      ASS_TEST_WINDOW=${ASS_TEST_WINDOW:-(not set: windows will show)}"
echo "  debug port:       $PORT"
echo "  page server:      $SERVER and $XSERVER (pid $SERVER_PID)"
echo "  extension:        $RUN/extension, id $EXT_ID"
echo "  load switches:    ${LOAD_SWITCHES:-(none)}"
echo "  risky checks:     $([ "$RISKY" = 1 ] && echo on || echo off)"
echo "  launches:         at most $MAX"
echo "  run directory:    $RUN"

# ---- One launch ----

say() { echo "[launch $N +$(($(date +%s) - STARTED))s] $*"; }

# note ID GROUP RESULT DETAIL [provisional]: a record by this script. An empty
# RESULT records nothing: a tool failure is no evidence either way.
note() {
  [ -n "$3" ] || return 0
  noted="$(cdp note "$RUN" "$N" "$@")"
  case "$noted" in "TOOL ERROR"*) say "note $1: $noted" ;; esac
}

# track: adds the app's current child processes to HELPERS, so a launch that
# ends early still has its list.
track() {
  for helper in $(pgrep -P "$APP_PID" 2>/dev/null); do
    case " $HELPERS " in *" $helper "*) ;; *) HELPERS="$HELPERS $helper" ;; esac
  done
}

# step: called before each phase. False once the browser process has ended.
step() {
  track
  running "$APP_PID" && return 0
  say "the browser process has ended"
  return 1
}

# drop_chromium_tmp: counts what Chromium left in its temporary folder, keeps
# the listing in the run directory, and deletes the folder.
drop_chromium_tmp() {
  [ -n "$CTMP" ] || return 0
  TMP_LEFT=$(ls -A "$CTMP" 2>/dev/null | wc -l | tr -d ' ')
  ls -lAR "$CTMP" > "$RUN/chromium-tmp-$N.txt" 2>&1
  rm -rf "$CTMP"
  CTMP=
}

status_text() {
  if [ "$STATUS" -gt 128 ]; then echo "signal $((STATUS - 128))"; else echo "exit code $STATUS"; fi
}

start_app() {
  echo "$N" > "$RUN/launch"
  [ "$N" -gt 1 ] || echo '[]' > "$RUN/plan-1.json"
  out="$(cdp write-config "$RUN/extension" "$SERVER" "$XSERVER" "$EXT_ID" "$N" "$RISKY" "$RUN/plan-$N.json")"
  case "$out" in "TOOL ERROR"*) STOP_REASON="config.js for launch $N: $out"; return 1 ;; esac
  PROFILE="$RUN/profile-$N"
  mkdir "$PROFILE" || { STOP_REASON="cannot create $PROFILE"; return 1; }
  # Chromium's temporary folder holds the single-instance socket, whose path has
  # at most 104 bytes; Chromium adds 39 (scripts/smoke.sh). A folder inside the
  # run directory is too long under the default $TMPDIR, so it is in /tmp; its
  # path and its listing after quit are kept in the run directory.
  CTMP="$(mktemp -d /tmp/ass-ext.XXXXXX)" || { CTMP=; STOP_REASON="cannot create a folder in /tmp"; return 1; }
  echo "$CTMP" > "$RUN/chromium-tmp-$N.path"
  HELPERS=
  TMP_LEFT=null
  # A build may load every core at the same time; timing results are read against this.
  LOAD_START="$(sysctl -n vm.loadavg 2>/dev/null | tr -d '{}' | sed 's/^ *//; s/ *$//')"
  STARTED=$(date +%s)
  set -f
  ASS_PROFILE_DIR="$PROFILE" MAC_CHROMIUM_TMPDIR="$CTMP" "$BIN" --use-mock-keychain \
    --remote-debugging-port="$PORT" --disable-backgrounding-occluded-windows \
    "--url=$SERVER/boot.html" $LOAD_SWITCHES > "$RUN/app-$N.log" 2>&1 &
  APP_PID=$!
  set +f
  LAUNCH_PID=$APP_PID
  say "launched: pid $APP_PID, ASS_PROFILE_DIR=$PROFILE, MAC_CHROMIUM_TMPDIR=$CTMP, load average $LOAD_START"
}

# run_phases: everything between launch and quit. Returns early when the
# browser process ends.
run_phases() {
  CONTENT=-
  SUITE_AT=$(date +%s)
  SUITE_DONE=
  WORKER_SEEN=

  # The app says it runs in test window mode before it shows anything.
  if [ -n "$ASS_TEST_WINDOW" ]; then
    n=0
    until grep -q -F "Ass: test window mode: $ASS_TEST_WINDOW" "$RUN/app-$N.log" || [ $n -ge 20 ] ||
      ! running "$APP_PID"; do
      n=$((n + 1))
      sleep 0.5
    done
    if grep -q -F "Ass: test window mode: $ASS_TEST_WINDOW" "$RUN/app-$N.log"; then
      say "test window mode $ASS_TEST_WINDOW reported by the app"
    elif ! running "$APP_PID"; then
      say "the browser process has ended"
      return
    elif [ "$ASS_EXT_TEST_VISIBLE_OK" = 1 ]; then
      say "test window mode not reported; windows may show (ASS_EXT_TEST_VISIBLE_OK=1)"
    else
      STOP_REASON="the app did not report test window mode $ASS_TEST_WINDOW within 10 s, so its windows may show"
      say "$STOP_REASON"
      return
    fi
  fi

  # 1. The extension's service worker appears.
  step || return
  out="$(cdp wait-target "$PORT" service_worker "chrome-extension://$EXT_ID/" 30 "$APP_PID")"
  result=
  case "$out" in
    app-exited) say "service worker: the browser process has ended"; return ;;
    none) result=fails; detail="no service_worker target at chrome-extension://$EXT_ID/ within 30 s of launch" ;;
    "TOOL ERROR"*) ;;
    *) result=works; detail="target appeared $(($(date +%s) - STARTED)) s after launch"; WORKER_SEEN=1 ;;
  esac
  say "service worker: $out"
  note "load.service_worker_target" load "$result" "$detail"

  # 2. The content view's target. Its id stays the same across navigations.
  step || return
  out="$(cdp wait-target "$PORT" page "$SERVER/" 30 "$APP_PID")"
  case "$out" in
    app-exited) say "content view: the browser process has ended"; return ;;
    none|"TOOL ERROR"*) say "content view: no page target on $SERVER/ ($out)" ;;
    *) CONTENT="$out"; say "content view: target $CONTENT" ;;
  esac

  # 3. start.html is ready; then the popup the tabs checks act on.
  step || return
  out="$(cdp wait-record "$RUN" "ctl.page.ready" "$N" 60 "$APP_PID")"
  say "start.html ready: $out"
  [ "$out" = app-exited ] && return
  if [ "$out" = found ] && [ "$RISKY" = 1 ] && [ "$CONTENT" != - ]; then
    say "victim popup: $(cdp open-victim "$PORT" "$CONTENT" "$SERVER/victim.html")"
  fi

  # 4. The worker's suite ends, at most 240 s after launch.
  step || return
  left=$((STARTED + 240 - $(date +%s)))
  [ $left -ge 1 ] || left=1
  SUITE_DONE="$(cdp wait-record "$RUN" "ctl.suite.done" "$N" "$left" "$APP_PID")"
  SUITE_AT=$(date +%s)
  say "worker suite done: $SUITE_DONE"
  [ "$SUITE_DONE" = app-exited ] && return

  # 5. What the page and the worker hold, read before the worker can stop.
  step || return
  say "snapshot of the page and the worker: $(cdp snapshot "$PORT" "$RUN" "$N" "$EXT_ID" "$CONTENT" 1)"

  # 6. The worker stops when idle. The snapshot's session reset its idle
  #    clock, so the time counts from the end of the snapshot. A worker never
  #    listed cannot go idle: that is not-run, never works.
  step || return
  if [ -z "$WORKER_SEEN" ]; then
    out="$(cdp wait-target "$PORT" service_worker "chrome-extension://$EXT_ID/" 1 "$APP_PID")"
    case "$out" in none|app-exited|"TOOL ERROR"*) ;; *) WORKER_SEEN=1 ;; esac
  fi
  result=
  suite_note=
  [ "$SUITE_DONE" = found ] || suite_note="; ctl.suite.done: $SUITE_DONE"
  if [ -z "$WORKER_SEEN" ]; then
    out="no service_worker target was ever listed"
    result=not-run
    detail="no service_worker target at chrome-extension://$EXT_ID/ was listed in this launch, so none could go idle"
  else
    out="$(cdp wait-gone "$PORT" service_worker "chrome-extension://$EXT_ID/" 45 "$APP_PID")"
  fi
  case "$out" in
    app-exited) say "worker idle: the browser process has ended"; return ;;
    still-there) result=fails; detail="still listed 45 s after the snapshot's debug session closed$suite_note" ;;
    "TOOL ERROR"*|"no service_worker"*) ;;
    *) result=works; detail="target gone $out s after the snapshot's debug session closed, $(($(date +%s) - SUITE_AT)) s after ctl.suite.done$suite_note" ;;
  esac
  say "worker idle: $out"
  note "worker.stops_when_idle" worker "$result" "$detail"

  # 7. A new worker instance handles an event: the extension's wake alarm,
  #    or else a navigation the worker listens for.
  step || return
  left=$((SUITE_AT + 60 - $(date +%s)))
  [ $left -ge 1 ] || left=1
  out="$(cdp wait-record "$RUN" "worker.wakes_after_idle" "$N" "$left" "$APP_PID")"
  [ "$out" = app-exited ] && { say "worker wake: the browser process has ended"; return; }
  if [ "$out" != found ]; then
    tried="no worker.wakes_after_idle within 60 s of ctl.suite.done$suite_note"
    if [ "$CONTENT" != - ]; then
      nav="$(cdp navigate "$PORT" "$CONTENT" "$SERVER/nav2.html")"
      say "worker wake: nothing within 60 s; content view sent to nav2.html: $nav"
      out="$(cdp wait-record "$RUN" "worker.wakes_after_idle" "$N" 20 "$APP_PID")"
      [ "$out" = app-exited ] && { say "worker wake: the browser process has ended"; return; }
      tried="$tried; then the content view was sent to nav2.html (navigate: $nav), and nothing came within 20 s"
    else
      tried="$tried; no content view target to send to nav2.html"
    fi
    [ "$out" = found ] || note "worker.wakes_after_idle" worker fails "$tried" provisional
  fi
  say "worker wake: $out"

  # 8. The records written since the first snapshot, before the content view
  #    leaves the page server: the page's (nav2.html's among them) and the
  #    woken worker's (worker.wakes_after_idle's second path). Nothing depends
  #    on the worker being idle any more.
  [ "$CONTENT" != - ] || { say "extension pages: not loaded, no content view target"; return; }
  step || return
  say "snapshot of the page and the woken worker: $(cdp snapshot "$PORT" "$RUN" "$N" "$EXT_ID" "$CONTENT" 1)"

  # 9. The extension's popup and options pages, in the content view.
  extension_page popup.html "ctl.popup.done" || return
  extension_page options.html "ctl.options.done" || return
}

# extension_page FILE DONE_ID: loads chrome-extension://<id>/FILE in the content
# view, waits 20 s for DONE_ID, then reads the page's records.
extension_page() {
  step || return 1
  nav="$(cdp navigate "$PORT" "$CONTENT" "chrome-extension://$EXT_ID/$1")"
  out="$(cdp wait-record "$RUN" "$2" "$N" 20 "$APP_PID")"
  say "$1: navigate $nav, $2 $out"
  [ "$out" = app-exited ] && return 1
  say "snapshot of $1: $(cdp snapshot "$PORT" "$RUN" "$N" "$EXT_ID" "$CONTENT" 0)"
}

# probe_saw PID: what the window probe saw of this launch: its first violation,
# how many there were, and whether it ended the launch. Prints nothing when
# there was none.
probe_saw() {
  python3 -I - "$RUN/probe.jsonl" "$1" <<'EOF'
import json, sys
pid = int(sys.argv[2])
first = killed = None
count = 0
for line in open(sys.argv[1], errors="replace"):
    try:
        r = json.loads(line)
    except ValueError:
        continue
    if not isinstance(r, dict) or r.get("pid") != pid:
        continue
    if r.get("k") == "violation":
        count += 1
        first = first or r
    elif r.get("k") == "killed" and killed is None:
        killed = r
if first:
    text = "%s (%d violations)" % ("+".join(first.get("what") or ["?"]), count)
    if killed:
        text += "; the window probe sent SIGKILL %d ms after the first" % round(1000 * (killed["t"] - first["t"]))
    print(text)
EOF
}

# end_launch: quit through AppKit by PID (SIGKILL after 30 s), then the
# script's own checks, and the launch's line in launches.jsonl.
end_launch() {
  QUIT_REQUESTED=false
  ENDED_EARLY=true
  QUIT_SECONDS=null
  KILLED=false
  REPLY=
  if running "$APP_PID"; then
    track
    QUIT_REQUESTED=true
    ENDED_EARLY=false
    REPLY="$(cdp quit "$APP_PID")"
    n=0
    while running "$APP_PID" && [ $n -lt 60 ]; do n=$((n + 1)); sleep 0.5; done
    if running "$APP_PID"; then
      kill -KILL "$APP_PID" 2>/dev/null
      KILLED=true
    else
      QUIT_SECONDS="$((n / 2)).$((n % 2 * 5))"
    fi
  fi
  wait "$APP_PID" 2>/dev/null
  STATUS=$?
  ENDED=$(date +%s)
  APP_PID=
  if [ "$STATUS" -gt 128 ]; then EXIT=null; SIGNAL=$((STATUS - 128)); else EXIT=$STATUS; SIGNAL=null; fi
  LOAD_END="$(sysctl -n vm.loadavg 2>/dev/null | tr -d '{}' | sed 's/^ *//; s/ *$//')"
  # As scripts/smoke.sh: helpers get 3 s to end after the browser process. The
  # probe samples every 50 ms, so by then it has logged anything it saw.
  sleep 3
  SAW="$(probe_saw "$LAUNCH_PID" 2>&1)"
  PROBE_KILLED=false
  case "$SAW" in *"sent SIGKILL"*) PROBE_KILLED=true ;; esac
  [ -z "$SAW" ] || say "window probe: $SAW"
  say "load average: $LOAD_START at launch, $LOAD_END at the end"
  LEFT=0
  for helper in $HELPERS; do kill -0 "$helper" 2>/dev/null && LEFT=$((LEFT + 1)); done
  drop_chromium_tmp
  if [ $KILLED = true ]; then
    say "quit: AppKit reply $REPLY; still running after 30 s, ended with SIGKILL; $LEFT helpers left"
  elif [ $QUIT_REQUESTED = true ]; then
    say "quit: AppKit reply $REPLY; $(status_text) within $QUIT_SECONDS s; $LEFT helpers left; $TMP_LEFT entries left in Chromium's temporary folder"
  else
    say "ended before the quit request: $(status_text); $LEFT helpers left"
  fi

  if [ $QUIT_REQUESTED = true ]; then
    result=works
    detail="running at the quit request"
  elif [ $PROBE_KILLED = true ]; then
    # Ended by the probe, not by a crash: nothing is known about a crash.
    result=not-run
    detail="the window probe ended the app before the quit request: $SAW"
  else
    result=crashes
    detail="the browser process ended before the quit request: $(status_text)"
  fi
  note "lifecycle.no_browser_crash" lifecycle "$result" "$detail"

  if [ $QUIT_REQUESTED = false ]; then
    result=not-run
    detail="no quit request: the browser process had already ended ($(status_text))"
  elif [ $KILLED = true ]; then
    result=fails
    detail="still running 30 s after the quit request (AppKit reply $REPLY), ended with SIGKILL; $LEFT helpers left"
  elif [ "$STATUS" -eq 0 ] && [ $LEFT -eq 0 ]; then
    result=works
    detail="exit code 0 within $QUIT_SECONDS s, no helper left (AppKit reply $REPLY)"
  else
    result=fails
    detail="$(status_text) within $QUIT_SECONDS s (AppKit reply $REPLY); $LEFT helpers left"
  fi
  note "lifecycle.quit" lifecycle "$result" "$detail"

  prefs="$(cdp secure-prefs "$PROFILE" "$EXT_ID")"
  case "$prefs" in "location=8 "*) result=works ;; "TOOL ERROR"*) result= ;; *) result=fails ;; esac
  say "Secure Preferences: $prefs"
  note "load.secure_preferences" load "$result" "$prefs"

  json="$(python3 -I -c 'import json, sys
print(json.dumps({"probe_saw": sys.argv[1] or None, "load_start": sys.argv[2], "load_end": sys.argv[3]})[1:-1])' \
    "$SAW" "$LOAD_START" "$LOAD_END")"
  json="$(printf '{"launch": %s, "pid": %s, "profile": "%s", "started": %s, "ended": %s, "exit": %s, "signal": %s, "quit_requested": %s, "ended_before_quit": %s, "quit_seconds": %s, "helpers_left": %s, "tmp_left": %s, "probe_killed": %s, %s}' \
    "$N" "$LAUNCH_PID" "$PROFILE" "$STARTED" "$ENDED" "$EXIT" "$SIGNAL" "$QUIT_REQUESTED" "$ENDED_EARLY" \
    "$QUIT_SECONDS" "$LEFT" "$TMP_LEFT" "$PROBE_KILLED" "$json")"
  out="$(cdp launch-end "$RUN" "$json")"
  case "$out" in "TOOL ERROR"*) say "launches.jsonl: $out" ;; esac
}

# new_crashes OLD_PLAN NEW_PLAN: how many check ids NEW_PLAN skips that OLD_PLAN
# did not, that is, checks newly found to have crashed the browser. A plan
# holds only crashed checks (report.py plan). Prints nothing when either file
# is not a JSON array of ids.
new_crashes() {
  python3 -I -c 'import json, sys
old, new = (json.load(open(p)) for p in sys.argv[1:3])
for skip in (old, new):
    if not isinstance(skip, list) or not all(isinstance(i, str) for i in skip):
        sys.exit(1)
print(len(set(new) - set(old)))' "$1" "$2" 2>/dev/null
}

# next_launch: true to launch again: only after the browser process ended
# before the quit request, below the launch limit, and only when that crash
# was pinned to a check, which the next launch then skips. A crash with no
# check in flight would only repeat, so it ends the run.
next_launch() {
  [ "$ENDED_EARLY" = true ] || return 1
  if [ "$N" -ge "$MAX" ]; then
    echo "No relaunch: the limit of $MAX launches is reached"
    return 1
  fi
  next=$((N + 1))
  if ! report plan "$RUN" "$CHECKS" > "$RUN/plan-$next.json" 2> "$RUN/plan-$next.log"; then
    echo "No relaunch: report.py plan failed (see $RUN/plan-$next.log)"
    return 1
  fi
  left="$(new_crashes "$RUN/plan-$N.json" "$RUN/plan-$next.json")"
  case "$left" in
    ''|*[!0-9]*) echo "No relaunch: report.py plan did not print a JSON array of check ids ($RUN/plan-$next.json)"; return 1 ;;
    0) echo "No relaunch: the crash was not pinned to a check, so a relaunch would repeat it"; return 1 ;;
  esac
  n=0
  while port_open "$PORT" && [ $n -lt 20 ]; do n=$((n + 1)); sleep 0.5; done
  if port_open "$PORT"; then
    echo "No relaunch: port $PORT is still in use 10 s after the app ended"
    return 1
  fi
  echo "Relaunching: the browser process ended during a check; launch $next skips $(cat "$RUN/plan-$next.json")"
}

N=1
STOP_REASON=
while :; do
  echo
  start_app || break
  run_phases
  end_launch
  [ -z "$STOP_REASON" ] || break
  next_launch || break
  N=$((N + 1))
done

stop_server
stop_probe
# The probe's summary over the whole run: samples, windows ordered in at alpha 0,
# and every violation (frontmost, active, menu bar owner, a window above alpha 0).
echo
echo "Window probe over the run: $(python3 -I -c 'import json, sys
s = [r for r in map(lambda l: json.loads(l) if l.startswith("{") else {}, open(sys.argv[1], errors="replace")) if r.get("k") == "summary"]
if not s:
    sys.exit("no summary line in " + sys.argv[1])
c = s[0]["counts"]
v = {k[len("violation-"):]: n for k, n in c.items() if k.startswith("violation-")}
print("%d launches watched, %d samples (longest gap %d ms), %d with a window ordered in at alpha 0; violations: %s; ended by the probe: %s"
      % (len(s[0]["watched"]), c.get("samples", 0), round(1000 * s[0]["maxSampleGap"]), c.get("onscreen-alpha0", 0),
         ", ".join("%s %d" % kv for kv in sorted(v.items())) or "none",
         ", ".join(str(w["pid"]) for w in s[0]["watched"] if w["killed"]) or "none"))' "$RUN/probe.jsonl" 2>&1)"
echo
report report "$RUN" "$CHECKS" > "$RUN/report.txt" 2>&1
CODE=$?
cat "$RUN/report.txt"
echo "The table is kept in $RUN/report.txt"
if [ -n "$STOP_REASON" ]; then
  echo "STOPPED: $STOP_REASON"
  exit 2
fi
exit $CODE
