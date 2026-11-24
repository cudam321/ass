#!/bin/sh
# Leftover helpers ended at start (src/leftover.rs), and a stderr whose reader has
# gone (src/main.rs, log_line!). Four cases, each runnable alone:
#
#   scripts/leftover-test.sh [debug|release] [t2] [t3] [t4] [t5]
#
# With no case named, all four run, t4 last (its starts are the first on a profile
# whose browser process was killed).
#
# t2  Decoys: processes this script starts from a program of its own (it only
#     sleeps) named "Ass Helper (Renderer)", with launchd as their parent and the
#     arguments --type=renderer --user-data-dir=TEXT. One start of the app on a
#     profile given by its real path (/private/var/...): the decoy carrying that
#     path is ended and the app prints "Ass: ended 1 ...". Left running: a decoy
#     carrying another profile's path; one carrying the profile's path with "2"
#     appended; and one carrying the same folder's path through /var, a text that
#     names the folder but is neither the given text nor the real path (the match
#     compares text only and never looks at a path another process names).
# t3  The profile given through /var (the text mktemp gives); decoys carrying that
#     text and the real path: both ended, "Ass: ended 2 ...".
# t4  t4a: an instance of the app on a profile given through /var; the
#     --user-data-dir its helpers carry is printed. Its network service helper is
#     stopped (SIGSTOP), then its browser process is ended with SIGKILL. Once the set
#     of its helpers still running with launchd as their parent has settled, a second
#     start on the same profile must print that it ended exactly as many as ps saw
#     right before it, and each must be gone.
#     t4b: the same without stopping the network helper. Its helpers may all end on
#     their own; then 0 with no line is the result expected, which shows only that
#     nothing was ended wrongly.
#     Every profile folder is made before its first start: a test start on a folder
#     that does not exist is refused (src/profile.rs).
# t5  The app's stderr on a pipe (a FIFO) whose only reader stops after the line
#     "Ass: test window shown: ...", then a quit through AppKit: exit code 0, no
#     helper left. Every quit prints the cookie flush line (src/flush.rs), so that
#     line is written to a pipe with no reader.
#
# Same rules as scripts/smoke.sh: test window mode, temporary profiles and HOME,
# --use-mock-keychain, its own debugging port (ASS_LEFTOVER_PORT, default 9371) and
# MAC_CHROMIUM_TMPDIR, the window probe with --kill, quits through AppKit by PID.
# The load is read before every start, and a start waits while the first number is
# above 30 (2 minutes at most). Signals go only to processes this script started:
# its decoys (checked by start time and path before each signal), its app instances,
# and helpers recorded as their children whose --user-data-dir names a folder of
# this run. If the probe kills the app, cannot guard, or sees an activation, every
# start stops and the script exits 1. The logs stay in the run folder, which is
# printed; the profiles and HOME are deleted. Chromium's bundle clones left by the
# killed instances are counted, not deleted.

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WHICH=debug CASES=
for arg in "$@"; do
  case "$arg" in
    debug|release) WHICH=$arg ;;
    t2|t3|t4|t5) CASES="$CASES $arg" ;;
    *) echo "usage: scripts/leftover-test.sh [debug|release] [t2] [t3] [t4] [t5]"; exit 2 ;;
  esac
done
CASES=${CASES:-t2 t3 t5 t4}
BIN="$ROOT/target/$WHICH/app.noindex/Ass.app/Contents/MacOS/Ass"
PORT="${ASS_LEFTOVER_PORT:-9371}"
PROBE="$ROOT/target/appprobe"
ENDED_RE='^Ass: ended [0-9]+ helper process\(es\) left running by an earlier run of this profile$'
FAILED=0 STARTS=0 PASSES=0 FAILS=0

pass() { echo "PASS  $1"; PASSES=$((PASSES + 1)); }
fail() { echo "FAIL  $1 (load $(sysctl -n vm.loadavg))"; FAILED=1; FAILS=$((FAILS + 1)); }
await() { n=0; while kill -0 "$1" 2>/dev/null && [ $n -lt "$2" ]; do n=$((n + 1)); sleep 0.5; done; }
ours() { [ -n "$1" ] && [ "$(ps -o ppid= -p "$1" 2>/dev/null | tr -d ' ')" = "$$" ]; }
not_run() { echo "NOT RUN: $*"; exit 1; }
appkit_quit() {
  osascript -l JavaScript -e "ObjC.import('AppKit');
    var app = \$.NSRunningApplication.runningApplicationWithProcessIdentifier($1);
    app.isNil() ? 'no such app' : String(app.terminate)" 2>&1
}
# alive_under PID PARENT: the process exists, is not a zombie, and has that parent.
alive_under() { case "$(echo $(ps -o ppid=,state= -p "$1" 2>/dev/null))" in "$2 "[!Z]*) return 0 ;; esac; return 1; }
gone() { case "$(ps -o state= -p "$1" 2>/dev/null | tr -d ' ')" in ""|Z*) return 0 ;; esac; return 1; }
# orphans_of PIDS: those of PIDS running with launchd as their parent.
orphans_of() { for helper in $1; do alive_under "$helper" 1 && printf '%s ' "$helper"; done; }
# A recorded helper of this run: its --user-data-dir names a folder of this run.
of_this_run() {
  case "$(ps -ww -o command= -p "$1" 2>/dev/null) " in
    *"--user-data-dir=$RUN/"*|*"--user-data-dir=$RUN_REAL/"*) return 0 ;;
  esac
  return 1
}

# On any exit: decoys, the app, recorded helpers of this run (SIGCONT first, for the
# stopped one), the guard and the probe; then the profiles, HOME and Chromium's
# temporary folder are deleted. Runs once.
PID= PROBE_PID= GUARD_PID= READER= ALL_HELPERS= RUN= RUN_REAL= MADE_TMP= CLEANED=0
cleanup() {
  status=$?
  [ "$CLEANED" = 1 ] && return
  CLEANED=1
  trap '' INT TERM HUP
  ours "$GUARD_PID" && kill -TERM "$GUARD_PID"
  ours "$READER" && kill -TERM "$READER"
  [ -n "$RUN" ] && end_decoys
  if ours "$PID"; then
    appkit_quit "$PID" > /dev/null
    await "$PID" 30
    ours "$PID" && kill -TERM "$PID" && await "$PID" 20
    ours "$PID" && kill -KILL "$PID"
    echo "leftover test: stopped the app"
  fi
  for helper in $ALL_HELPERS; do
    gone "$helper" && continue
    of_this_run "$helper" || continue
    kill -CONT "$helper" 2>/dev/null
    kill -TERM "$helper" 2>/dev/null && await "$helper" 10
    kill -KILL "$helper" 2>/dev/null
    echo "leftover test: ended a helper of this run that was left, pid $helper"
  done
  ours "$PROBE_PID" && kill -TERM "$PROBE_PID" && await "$PROBE_PID" 10
  [ -n "$MADE_TMP" ] && rm -rf "$MADE_TMP"
  if [ -n "$RUN" ]; then
    rm -rf "$RUN/t2" "$RUN/t3" "$RUN/t4a" "$RUN/t4b" "$RUN/t5" "$RUN/home" "$RUN/decoys" "$RUN/t5.fifo"
    echo "leftover test: logs in $RUN"
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

[ -x "$BIN" ] || not_run "$BIN not found. Run: cargo run --bin bundle"
ASS_TEST_WINDOW=invisible
grep -q -a -F "Ass: test window mode: $ASS_TEST_WINDOW" "$BIN" || not_run "$BIN has no test window mode"
if [ ! -x "$PROBE" ] || [ "$ROOT/scripts/appprobe.swift" -nt "$PROBE" ]; then
  swiftc -O -o "$PROBE" "$ROOT/scripts/appprobe.swift" || not_run "cannot build $PROBE"
fi
curl -s -m 2 "http://127.0.0.1:$PORT/json/version" > /dev/null 2>&1 && not_run "something already answers on port $PORT"

# The run folder as mktemp gives it, through /var, and its real path.
RUN="$(mktemp -d -t ass-leftover)"
RUN_REAL="$(cd "$RUN" && pwd -P)"
[ "$RUN_REAL" = "/private$RUN" ] || not_run "the run folder $RUN is not under /var, so /var and /private/var cannot be compared"
mkdir "$RUN/home" "$RUN/decoys"
MADE_TMP="$(mktemp -d /tmp/ass-left.XXXXXX)"
MAC_CHROMIUM_TMPDIR="$MADE_TMP"
export ASS_TEST_WINDOW MAC_CHROMIUM_TMPDIR
PROBE_LOG="$RUN/probe.jsonl"
CLONE_DIR="$(dirname "$(getconf DARWIN_USER_TEMP_DIR)")/X/dev.ass.browser.code_sign_clone"
CLONES_BEFORE=$(ls -d "$CLONE_DIR"/code_sign_clone.* 2>/dev/null | wc -l | tr -d ' ')

# The decoy: sleeps 300 s whatever its arguments, so one that outlives this
# script ends by itself.
DECOY="$RUN/decoys/Ass Helper (Renderer)"
printf '#include <unistd.h>\nint main(void) { sleep(300); return 0; }\n' |
  cc -O -x c - -o "$DECOY" || not_run "cannot build the decoy"

# decoy NAME TEXT: starts a decoy with launchd as its parent (its first parent, a
# subshell, exits at once), carrying --type=renderer --user-data-dir=TEXT.
# Recorded in $RUN/decoys.txt as "NAME PID START".
decoy() {
  ( "$DECOY" --type=renderer "--user-data-dir=$2" < /dev/null > /dev/null 2>&1 &
    echo $! > "$RUN/decoys/pid" )
  pid=$(cat "$RUN/decoys/pid")
  n=0
  until alive_under "$pid" 1 || [ $n -ge 20 ]; do n=$((n + 1)); sleep 0.1; done
  [ $n -lt 20 ] || not_run "decoy $1 (pid $pid) did not get launchd as its parent"
  echo "$1 $pid $(ps -o lstart= -p "$pid" | tr ' ' '_')" >> "$RUN/decoys.txt"
}
# is_decoy PID START: still the decoy this script started.
is_decoy() {
  [ "$(ps -o lstart= -p "$1" 2>/dev/null | tr ' ' '_')" = "$2" ] &&
    [ "$(ps -o comm= -p "$1" 2>/dev/null)" = "$DECOY" ]
}
decoy_running() { grep "^$1 " "$RUN/decoys.txt" | { read -r _ pid start; is_decoy "$pid" "$start"; }; }
# Ends every decoy still running, after the check that it is ours.
end_decoys() {
  [ -f "$RUN/decoys.txt" ] || return 0
  while read -r name pid start; do
    is_decoy "$pid" "$start" && kill -KILL "$pid" && echo "  ended decoy $name (pid $pid), still running at the end of its case"
  done < "$RUN/decoys.txt"
  rm -f "$RUN/decoys.txt"
}

# The probe, for every start of this script, and the guard, as in smoke.sh.
"$PROBE" --binary "$BIN" --kill > "$PROBE_LOG" 2>&1 &
PROBE_PID=$!
n=0
until grep -q '"k":"ready"' "$PROBE_LOG" 2>/dev/null || [ $n -ge 20 ]; do n=$((n + 1)); sleep 0.5; done
[ $n -lt 20 ] || not_run "the probe did not start; the app was not launched"
guarded() { ours "$PROBE_PID" && ! grep -q '"k":"guard-failed"' "$PROBE_LOG"; }
(
  while guarded; do sleep 0.2; done
  ours "$PID" && kill -KILL "$PID" && echo "leftover test: the probe cannot guard the run; the app was ended by SIGKILL"
) &
GUARD_PID=$!
# Stops everything when the probe killed the app, cannot guard, or saw it active.
probe_stop() {
  reason=$(grep -m 1 -e '"k":"killed"' -e '"k":"guard-failed"' -e '"k":"active".*"to":true' "$PROBE_LOG")
  [ -z "$reason" ] && guarded && return 0
  echo "STOPPED: the probe reported: ${reason:-it ended early}. No further start."
  exit 1
}

# Waits while the load's first number is above 30, 2 minutes at most.
load_gate() {
  n=0
  while :; do
    LOAD=$(sysctl -n vm.loadavg | awk '{print $2}')
    awk -v l="$LOAD" 'BEGIN { exit !(l <= 30) }' && return 0
    [ $n -ge 24 ] && not_run "the load stayed above 30 for 2 minutes ($LOAD); no further start"
    n=$((n + 1)); sleep 5
  done
}

# launch NAME PROFILE [STDERR]: the app on ASS_PROFILE_DIR=PROFILE as given, its
# output to $RUN/NAME.log (stderr to STDERR when given); waits up to 20 s for its
# page. Sets PID. Call load_gate first.
launch() {
  curl -s -m 2 "http://127.0.0.1:$PORT/json/version" > /dev/null 2>&1 && not_run "something already answers on port $PORT"
  echo "  start $((STARTS + 1)): $1, load $(sysctl -n vm.loadavg)"
  if [ -n "$3" ]; then
    ASS_PROFILE_DIR="$2" HOME="$RUN/home" "$BIN" --use-mock-keychain --remote-debugging-port="$PORT" \
      --disable-backgrounding-occluded-windows "--url=data:text/html,<title>ass-leftover-$1</title>" \
      > "$RUN/$1.log" 2> "$3" &
  else
    ASS_PROFILE_DIR="$2" HOME="$RUN/home" "$BIN" --use-mock-keychain --remote-debugging-port="$PORT" \
      --disable-backgrounding-occluded-windows "--url=data:text/html,<title>ass-leftover-$1</title>" \
      > "$RUN/$1.log" 2>&1 &
  fi
  PID=$!
  STARTS=$((STARTS + 1))
  n=0
  until curl -s -m 2 "http://127.0.0.1:$PORT/json" 2>/dev/null | grep -q "ass-leftover-$1" || [ $n -ge 40 ]; do
    n=$((n + 1)); sleep 0.5
  done
  probe_stop
  ALL_HELPERS="$ALL_HELPERS $(pgrep -P "$PID" | tr '\n' ' ')"
  [ $n -lt 40 ] || { fail "$1: the page did not load within 20 s"; return 1; }
}

# quit_app NAME: quits $PID through AppKit; exit code 0, and none of its helpers
# running 3 s after the end.
quit_app() {
  helpers=$(pgrep -P "$PID" | tr '\n' ' ')
  ALL_HELPERS="$ALL_HELPERS $helpers"
  reply=$(appkit_quit "$PID")
  await "$PID" 30
  if ours "$PID"; then
    kill -TERM "$PID"; await "$PID" 20
    ours "$PID" && kill -KILL "$PID"
    wait "$PID"
    fail "$1: the AppKit quit (reply $reply) did not end the app within 15 s; ended by signal, exit code $?"
    PID=
    return
  fi
  wait "$PID"
  code=$?
  PID=
  probe_stop
  sleep 3
  left=
  for helper in $helpers; do gone "$helper" || left="$left $helper"; done
  if [ "$reply" = true ] && [ $code -eq 0 ] && [ -z "$left" ]; then
    pass "$1: quit through AppKit, exit code 0, none of its $(echo $helpers | wc -w | tr -d ' ') helpers left"
  else
    fail "$1: quit through AppKit: reply $reply, exit code $code, helpers left:${left:- none} (ended at cleanup)"
  fi
}

# ended_count NAME: the N of the app's "Ass: ended N ..." line in $RUN/NAME.log, 0
# when there is none; "bad: ..." for any other form or more than one line.
ended_count() {
  lines=$(grep '^Ass: ended' "$RUN/$1.log")
  [ -z "$lines" ] && { echo 0; return; }
  [ "$(echo "$lines" | wc -l | tr -d ' ')" = 1 ] && echo "$lines" | grep -q -E "$ENDED_RE" &&
    { echo "$lines" | awk '{print $3}'; return; }
  echo "bad: $lines" | paste -s -d ' ' -
}

run_t2() {
  echo "t2: decoys; the profile given by its real path, $RUN_REAL/t2/profile"
  mkdir -p "$RUN/t2/profile"
  P="$RUN_REAL/t2/profile"
  load_gate
  decoy t2-same "$P"
  decoy t2-other "$RUN_REAL/t2/other"
  decoy t2-prefix "${P}2"
  decoy t2-via-var "$RUN/t2/profile"
  if launch t2 "$P"; then
    count=$(ended_count t2)
    running=
    for name in t2-same t2-other t2-prefix t2-via-var; do decoy_running "$name" && running="$running $name"; done
    if [ "$count" = 1 ] && [ "$running" = " t2-other t2-prefix t2-via-var" ]; then
      pass "t2: the start ended the decoy carrying this profile's path and printed 'ended 1'; left running: another profile's path, the path with 2 appended, the same folder through /var"
    else
      fail "t2: 'ended' count $count (1 expected); decoys running:${running:- none} (t2-other t2-prefix t2-via-var expected)"
    fi
  fi
  quit_app t2
  end_decoys
}

run_t3() {
  echo "t3: the profile given through /var, $RUN/t3/profile"
  mkdir -p "$RUN/t3/profile"
  load_gate
  decoy t3-given "$RUN/t3/profile"
  decoy t3-real "$RUN_REAL/t3/profile"
  if launch t3 "$RUN/t3/profile"; then
    count=$(ended_count t3)
    running=
    for name in t3-given t3-real; do decoy_running "$name" && running="$running $name"; done
    if [ "$count" = 2 ] && [ -z "$running" ]; then
      pass "t3: the start ended both decoys, the one carrying the given /var text and the one carrying the real /private/var path, and printed 'ended 2'"
    else
      fail "t3: 'ended' count $count (2 expected); decoys running:${running:- none} (none expected)"
    fi
  fi
  quit_app t3
  end_decoys
}

run_t5() {
  echo "t5: stderr on a pipe whose reader has gone, then a quit"
  mkdir -p "$RUN/t5/profile"
  mkfifo "$RUN/t5.fifo"
  load_gate
  sed '/^Ass: test window shown:/q' < "$RUN/t5.fifo" > "$RUN/t5-stderr-read.txt" &
  READER=$!
  if launch t5 "$RUN/t5/profile" "$RUN/t5.fifo"; then
    await "$READER" 20
    if gone "$READER"; then
      wait "$READER"
      READER=
      echo "  the reader stopped after: $(tail -n 1 "$RUN/t5-stderr-read.txt")"
      quit_app "t5 (stderr's reader gone)"
    else
      fail "t5: not run: the reader never saw the line 'Ass: test window shown'; read: $RUN/t5-stderr-read.txt"
      quit_app t5
    fi
  else
    quit_app t5
  fi
  ours "$READER" && kill -TERM "$READER"
  READER=
}

# t4 NAME PROFILE STOP: the first instance, killed; the second start ends what it
# left.
t4() {
  name=$1 profile=$2 stop=$3
  load_gate
  launch "$name-first" "$profile" || { quit_app "$name-first"; return; }
  first=$(pgrep -P "$PID" | tr '\n' ' ')
  ALL_HELPERS="$ALL_HELPERS $first"
  carried=$(for helper in $first; do
    ps -ww -o command= -p "$helper" 2>/dev/null | grep -o -e '--user-data-dir=[^ ]*'
  done | sort | uniq -c | sed 's/^ *//' | paste -s -d ';' -)
  echo "  the first instance's $(echo $first | wc -w | tr -d ' ') helpers carry: $carried"
  echo "  (given: $profile; real path: $(cd "$profile" && pwd -P))"
  net=
  for helper in $first; do
    case "$(ps -ww -o command= -p "$helper" 2>/dev/null)" in
      *--utility-sub-type=network.mojom.NetworkService*) net=$helper ;;
    esac
  done
  if [ "$stop" = stop ]; then
    [ -n "$net" ] || { fail "$name: no network service helper found"; quit_app "$name-first"; return; }
    kill -STOP "$net"
    echo "  the network helper, pid $net, stopped"
  fi
  killed=$PID
  kill -KILL "$PID"
  # Without its stderr, so the shell's own "Killed: 9" note stays out of the output.
  wait "$PID" 2> /dev/null
  echo "  the browser process, pid $killed, ended with SIGKILL (exit status $?)"
  PID=
  probe_stop
  # Its helpers still running with launchd as their parent, until that set has been
  # the same for 2 s (20 s at most).
  previous=$(orphans_of "$first") same=0 n=0
  while [ $same -lt 4 ] && [ $n -lt 40 ]; do
    sleep 0.5; n=$((n + 1))
    current=$(orphans_of "$first")
    if [ "$current" = "$previous" ]; then same=$((same + 1)); else same=0 previous=$current; fi
  done
  echo "  orphans settled after $n half-seconds: ${previous:-none}"
  load_gate
  # Counted again as late as possible: right before the start.
  orphans=$(echo $(orphans_of "$first"))
  expected=$(echo $orphans | wc -w | tr -d ' ')
  if launch "$name-second" "$profile"; then
    count=$(ended_count "$name-second")
    still=
    for helper in $orphans; do gone "$helper" || still="$still $helper"; done
    if [ "$count" = "$expected" ] && [ -z "$still" ]; then
      pass "$name: the start after the kill printed 'ended $count'$([ "$count" = 0 ] && echo " (no line)"), as many as ps saw orphaned right before it (${orphans:-none}), and none of them is left"
    else
      fail "$name: 'ended' count $count, $expected orphaned right before the start (${orphans:-none}); still running:${still:- none}"
    fi
  fi
  quit_app "$name-second"
}

run_t4() {
  echo "t4a: the network helper stopped, then the browser killed; the profile given through /var"
  mkdir -p "$RUN/t4a/profile"
  t4 t4a "$RUN/t4a/profile" stop
  echo "t4b: the browser killed, the network helper not stopped; the profile given through /var"
  mkdir -p "$RUN/t4b/profile"
  t4 t4b "$RUN/t4b/profile" no
}

echo "leftover test: $BIN"
echo "  cases:$(echo " $CASES"); run folder $RUN (real path $RUN_REAL)"
for case_name in $CASES; do
  "run_$case_name"
done

CLONES_AFTER=$(ls -d "$CLONE_DIR"/code_sign_clone.* 2>/dev/null | wc -l | tr -d ' ')
echo "leftover test: $PASSES passed, $FAILS failed; $STARTS starts of the app; bundle clones in $CLONE_DIR: $CLONES_BEFORE before, $CLONES_AFTER after (not deleted)"
exit $FAILED
