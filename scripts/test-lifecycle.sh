#!/bin/sh
# F50 only: use only this task's binary, state and child PIDs. Does not clear existing pairings.
set -eu
binary=${1:?binary required}
state=${2:?isolated test state required}
address=${3:?existing address file required}
log=/tmp/cowplay-lifecycle.log
wait_ready() {
    n=0
    while ! grep -q BLUEZ_AGENT_AND_PROFILE_READY "$log"; do
        kill -0 "$task_pid" || { echo START_FAILED; exit 1; }
        n=$((n+1)); [ "$n" -lt 10 ] || { kill -TERM "$task_pid"; echo READY_DEADLINE_EXCEEDED; exit 1; }
        sleep 1
    done
}
"$binary" bluetooth-check "$state" "$address" 60 >"$log" 2>&1 &
task_pid=$!
trap 'kill -TERM "$task_pid" 2>/dev/null || true' EXIT INT TERM
wait_ready
children=$(cat "/proc/$task_pid/task/$task_pid/children")
kill -TERM "$task_pid"
wait "$task_pid"
for child in $children; do [ ! -e "/proc/$child/exe" ] || { echo TERM_LEFT_CHILD; exit 1; }; done
[ ! -e /var/lib/bluetooth ] || { echo TERM_LEFT_STORAGE_LINK; exit 1; }
echo SIGTERM_CLEANUP_PASSED
"$binary" bluetooth-check "$state" "$address" 2 >"$log" 2>&1
echo RESTART_AFTER_SIGTERM_PASSED
"$binary" bluetooth-check "$state" "$address" 60 >"$log" 2>&1 &
task_pid=$!
wait_ready
children=$(cat "/proc/$task_pid/task/$task_pid/children")
kill -KILL "$task_pid"
wait "$task_pid" 2>/dev/null || true
n=0
while :; do
    alive=0
    for child in $children; do [ ! -e "/proc/$child/exe" ] || alive=1; done
    [ "$alive" = 1 ] || break
    n=$((n+1)); [ "$n" -lt 3 ] || { echo PARENT_DEATH_LEFT_CHILD; exit 1; }
    sleep 1
done
echo PARENT_DEATH_CLEANUP_PASSED
"$binary" bluetooth-check "$state" "$address" 2 >"$log" 2>&1
[ ! -e /var/lib/bluetooth ] || { echo RESTART_LEFT_OWN_STORAGE_LINK; exit 1; }
echo RESTART_AFTER_SIGKILL_PASSED
trap - EXIT INT TERM
