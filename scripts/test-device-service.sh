#!/bin/sh
# Use an already imported isolated /tmp config; never print its contents.
set -eu
binary=${1:?binary required}
state=${2:?state required}
log=/tmp/cowplay-service-test.log
"$binary" serve "$state/config.json" >"$log" 2>&1 &
task_pid=$!
trap 'kill -TERM "$task_pid" 2>/dev/null || true; wait "$task_pid" 2>/dev/null || true' EXIT INT TERM
# Read just the public origin; the bearer token stays inside this device shell.
origin=$(jsonfilter -i "$state/config.json" -e '@.receiver.web.origin')
host=${origin#https://};host=${host%:*}
address=$(jsonfilter -i "$state/config.json" -e '@.receiver.web.bind');address=${address%:*}
port=${origin##*:}
n=0
while ! curl --fail --silent --show-error --connect-timeout 1 --max-time 2 --resolve "$host:$port:$address" "$origin/health" >/dev/null 2>&1; do
    kill -0 "$task_pid" || { echo SERVICE_EXITED; exit 1; }
    n=$((n+1)); [ "$n" -lt 10 ] || { echo HEALTH_DEADLINE_EXCEEDED; exit 1; }
    sleep 1
done
echo TRUSTED_HTTPS_HEALTH_PASSED
# Curl loads Authorization from a private file, never from command-line args or output.
auth="$state/test-curl.conf"
(umask 077; printf 'header = "Authorization: Bearer %s"\n' "$(cat "$state/access-token")" > "$auth")
code=$(curl --silent --show-error --output /tmp/cowplay-status.json --write-out '%{http_code}' --resolve "$host:$port:$address" -K "$auth" "$origin/api/pairing")
[ "$code" = 200 ] || { echo AUTHENTICATED_STATUS_FAILED; exit 1; }
jsonfilter -i /tmp/cowplay-status.json -e '@.paired' -e '@.carplay'
code=$(curl --silent --show-error --output /dev/null --write-out '%{http_code}' --resolve "$host:$port:$address" "$origin/api/pairing")
[ "$code" = 401 ] || { echo MISSING_AUTH_NOT_REJECTED; exit 1; }
echo AUTHENTICATION_BOUNDARY_PASSED
kill -TERM "$task_pid";wait "$task_pid"
# Occupy only the HTTPS port: let the Rust receiver reach its actual server-error cleanup path.
openssl s_server -accept "$address:$port" -cert "$state/tls.pem" -key "$state/tls.key" -www > /tmp/cowplay-port-holder.log 2>&1 &
blocker_pid=$!
trap 'kill -TERM "$task_pid" "$blocker_pid" 2>/dev/null || true; wait "$task_pid" "$blocker_pid" 2>/dev/null || true' EXIT INT TERM
sleep 1
kill -0 "$blocker_pid"
if "$binary" serve "$state/config.json" >/tmp/cowplay-conflict.log 2>&1; then echo CONFLICT_NOT_REJECTED;exit 1;fi
! grep -q 'panicked\|resumed after completion' /tmp/cowplay-conflict.log || { echo CONFLICT_PANIC;exit 1; }
grep -qi 'address.*in use' /tmp/cowplay-conflict.log || { echo UNEXPECTED_CONFLICT_FAILURE; exit 1; }
echo HTTPS_PORT_CONFLICT_CLEANUP_PASSED
kill -TERM "$blocker_pid";wait "$blocker_pid" 2>/dev/null || true
rm -f "$auth"
trap - EXIT INT TERM
