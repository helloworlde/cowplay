#!/bin/sh
# Native apk install/upgrade/remove checks in a new disposable root. No services run.
set -eu
previous=$(realpath "${1:?Usage: test-apk.sh PREVIOUS_APK NEW_APK NEW_INSTALL_ROOT}")
package=$(realpath "${2:?new APK required}")
root=${3:?new install root required}
[ ! -e "$root" ] || { echo 'Install root already exists' >&2; exit 1; }
mkdir -p "$root/etc/apk/keys" "$root/etc/cowplay/state"
root=$(CDPATH='' cd -- "$root" && pwd)
key=$root/etc/apk/keys/openwrt-25.12.pem
curl --fail --location --connect-timeout 15 --max-time 60 \
 'https://git.openwrt.org/keyring/plain/apk/openwrt-25.12.pem?id=f0670054fd3aa9616bcd8d0d0357a1a15c65b758' -o "$key"
printf '%s  %s\n' 27d61d827d64ccf2f46dce5f52aec6279404d4a00686dafb22f52f3b4ee675f8 "$key" | sha256sum -c -
# Standard OpenWrt package dependencies are verified separately from the local unsigned APK.
cat > "$root/etc/apk/repositories" <<'REPOS'
https://downloads.openwrt.org/releases/25.12.6/targets/armsr/armv8/packages/packages.adb
https://downloads.openwrt.org/releases/25.12.6/packages/aarch64_generic/base/packages.adb
https://downloads.openwrt.org/releases/25.12.6/packages/aarch64_generic/packages/packages.adb
REPOS
apk --root "$root" --arch aarch64_generic --no-scripts add --initdb procd ubus jsonfilter ca-bundle openssl-util
printf 'synthetic user state\n' > "$root/etc/cowplay/state/retained-test-data"
before=$(sha256sum "$root/etc/cowplay/state/retained-test-data")
apk --root "$root" --arch aarch64_generic --no-scripts --no-network --allow-untrusted add "$previous"
apk --root "$root" --arch aarch64_generic --no-scripts --no-network --allow-untrusted add --upgrade "$package"
apk --root "$root" info --exists 'cowplay=0.0.2-r6'
"$root/usr/bin/cowplay" verify-image "$root/usr/bin/cowplay"
runtime=$root/usr/lib/cowplay/bluez
(cd "$runtime" && sha256sum -c SHA256SUMS)
"$runtime/lib/ld-musl-aarch64.so.1" --library-path "$runtime/usr/lib:$runtime/lib" "$runtime/usr/bin/bluetoothd" --version
"$runtime/lib/ld-musl-aarch64.so.1" --library-path "$runtime/usr/lib:$runtime/lib" "$runtime/usr/bin/dbus-daemon" --version
[ ! -e "$root/opt/dashplay-bluez" ]
[ ! -e "$root/lib/modules" ]
apk --root "$root" info --contents cowplay > "$root/cowplay-contents.txt"
apk --root "$root" --no-scripts --no-network del cowplay
[ ! -e "$root/usr/bin/cowplay" ]
[ ! -e "$root/etc/init.d/cowplay" ]
[ ! -e "$root/usr/lib/cowplay/bluez" ]
[ "$before" = "$(sha256sum "$root/etc/cowplay/state/retained-test-data")" ]
apk --root "$root" --arch aarch64_generic --no-scripts --no-network --allow-untrusted add "$package"
echo APK_NATIVE_INSTALL_UPGRADE_REMOVE_REINSTALL_AND_STATE_RETENTION_PASSED
