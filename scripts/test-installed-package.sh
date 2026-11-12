#!/bin/sh
# F50 development-package acceptance; only removes/reinstalls this task's package.
# Imported state and the original DashPlay data are retained.
set -eu
package=${1:?local APK path required}
state=/etc/cowplay/state
apk info --exists 'cowplay=0.0.2-r2'
test -s "$state/config.json"
before=$(sha256sum "$state/config.json" "$state/signing-seed" "$state/accessory.pk8")
origin=$(jsonfilter -i "$state/config.json" -e '@.receiver.web.origin')
host=${origin#https://}; host=${host%:*}
address=$(jsonfilter -i "$state/config.json" -e '@.receiver.web.bind'); address=${address%:*}
port=${origin##*:}
trap '/etc/init.d/cowplay stop 2>/dev/null || true' EXIT INT TERM
/etc/init.d/cowplay start
n=0
while ! curl --fail --silent --connect-timeout 1 --max-time 2 --resolve "$host:$port:$address" "$origin/health" >/dev/null 2>&1; do
    n=$((n+1)); [ "$n" -lt 10 ] || { echo PROCD_HEALTH_FAILED; exit 1; }
    sleep 1
done
ubus call service list '{"name":"cowplay"}' | jsonfilter -e '@["cowplay"].instances.*.running' | grep -qx true
echo PROCD_START_AND_TRUSTED_HTTPS_PASSED
/etc/init.d/cowplay stop
n=0
while test -e /var/lib/bluetooth; do
    n=$((n+1)); [ "$n" -lt 5 ] || { echo PROCD_STOP_FAILED; exit 1; }
    sleep 1
done
echo PROCD_STOP_AND_CLEANUP_PASSED
apk del --no-network cowplay
test ! -e /usr/bin/cowplay
test ! -e /etc/init.d/cowplay
after=$(sha256sum "$state/config.json" "$state/signing-seed" "$state/accessory.pk8")
test "$before" = "$after"
echo UNINSTALL_RETAINS_IMPORTED_STATE_PASSED
trap - EXIT INT TERM
apk add --no-network --allow-untrusted "$package"
/usr/bin/cowplay verify-image /usr/bin/cowplay
test -x /etc/init.d/cowplay
echo PACKAGE_REINSTALL_PASSED
