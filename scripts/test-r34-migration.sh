#!/bin/sh
# Install the core and LuCI packages into a new disposable OpenWrt root (aarch64 Linux, root user).
# Only the LuCI post-install runs (chrooted); the core service scripts are covered elsewhere.
set -eu
core=$(realpath "${1:?Usage: test-luci-apk.sh CORE_APK LUCI_APK NEW_INSTALL_ROOT}")
luci=$(realpath "${2:?LuCI APK required}")
oldcore=$(realpath "${4:?r34 core required}")
oldluci=$(realpath "${5:?r34 LuCI required}")
root=${3:?new install root required}
[ ! -e "$root" ] || { echo 'Install root already exists' >&2; exit 1; }
mkdir -p "$root/etc/apk/keys" "$root/tmp/lock" "$root/tmp/run"
# OpenWrt base-files ships /var -> /tmp; create it first so apk's cache/log under var/ fit it.
ln -s tmp "$root/var"
root=$(CDPATH='' cd -- "$root" && pwd)
key=$root/etc/apk/keys/openwrt-25.12.pem
curl --fail --location --connect-timeout 15 --max-time 60 \
 'https://git.openwrt.org/keyring/plain/apk/openwrt-25.12.pem?id=f0670054fd3aa9616bcd8d0d0357a1a15c65b758' -o "$key"
printf '%s  %s\n' 27d61d827d64ccf2f46dce5f52aec6279404d4a00686dafb22f52f3b4ee675f8 "$key" | sha256sum -c -
cat > "$root/etc/apk/repositories" <<'REPOS'
https://downloads.openwrt.org/releases/25.12.6/targets/armsr/armv8/packages/packages.adb
https://downloads.openwrt.org/releases/25.12.6/packages/aarch64_generic/base/packages.adb
https://downloads.openwrt.org/releases/25.12.6/packages/aarch64_generic/luci/packages.adb
https://downloads.openwrt.org/releases/25.12.6/packages/aarch64_generic/packages/packages.adb
REPOS
apk --root "$root" --arch aarch64_generic --no-scripts add --initdb \
 base-files busybox uci luci-base rpcd jsonfilter procd ubus ca-bundle openssl-util curl
# The old development APKs are unsigned; only fixture setup allows them.
apk --root "$root" --arch aarch64_generic --no-scripts --no-network --allow-untrusted add "$oldcore" "$oldluci"
keydir=$(mktemp -d)
trap 'rm -rf "$keydir"' EXIT HUP INT TERM
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "$keydir/test.key" 2>/dev/null
openssl pkey -in "$keydir/test.key" -pubout -out "$keydir/test.pem"
cp "$core" "$keydir/core.apk"
cp "$luci" "$keydir/luci.apk"
for file in core luci; do
    apk --allow-untrusted adbsign --reset-signatures --sign-key "$keydir/test.key" "$keydir/$file.apk"
    apk --no-network --keys-dir "$keydir" verify "$keydir/$file.apk"
done
mkdir -p "$root/etc/cowplay/state" "$root/etc/config" "$root/etc/rc.d"
chmod 700 "$root/etc/cowplay/state"
printf '%s\n' 'SYNTHETIC IDENTITY - NOT A PRIVATE KEY' > "$root/etc/cowplay/state/accessory.pk8"
printf '%s\n' 'SYNTHETIC PAIRING' > "$root/etc/cowplay/state/pairing.json"
printf '%s\n' 'SYNTHETIC CERTIFICATE' > "$root/etc/cowplay/state/tls.pem"
printf '%s\n' 'config cowplay main' ' option port 19443' > "$root/etc/config/cowplay"
(cd "$root" && sha256sum etc/cowplay/state/* etc/config/cowplay) > "$keydir/state.sha256"
apk --root "$root" info --exists cowplay=0.3.14-r34 luci-app-cowplay=0.3.14-r34
# Retain an explicit enabled preference; the package scripts must not add restore autostart.
chroot "$root" /etc/init.d/cowplay enable
find "$root/etc/rc.d" -name '*cowplay*' -exec basename '{}' \; | sort > "$keydir/boot.before"
# Identical package constraints to the updater, with no force/untrusted bypass.
for simulate in --simulate ''; do
    apk --root "$root" --arch aarch64_generic --no-network --no-cache --repositories-file /dev/null --keys-dir "$keydir" add --upgrade "$keydir/core.apk" "$keydir/luci.apk" cowplay=0.0.1-r0 luci-app-cowplay=0.0.1-r0 $simulate
done
apk --root "$root" info --exists cowplay=0.0.1-r0 luci-app-cowplay=0.0.1-r0
(cd "$root" && sha256sum -c "$keydir/state.sha256")
find "$root/etc/rc.d" -name '*cowplay*' -exec basename '{}' \; | sort > "$keydir/boot.after"
cmp "$keydir/boot.before" "$keydir/boot.after"
chroot "$root" /bin/sh -c '/usr/libexec/rpcd/cowplay list | jsonfilter -e "@.update_check" -e "@.tesla_status" -e "@.tesla_climate_command" >/dev/null'
[ -f "$root/www/luci-static/resources/cowplay/updater.js" ]
[ ! -e "$root/etc/rc.d/S18cowplay-restore" ]
echo R34_SIGNED_DOUBLE_PACKAGE_MIGRATION_AND_PRIVATE_STATE_BOOT_PREFERENCE_PASSED
