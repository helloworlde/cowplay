#!/bin/sh
# Install the core and LuCI packages into a new disposable OpenWrt root (aarch64 Linux, root user).
# Only the LuCI post-install runs (chrooted); the core service scripts are covered elsewhere.
set -eu
core=$(realpath "${1:?Usage: test-luci-apk.sh CORE_APK LUCI_APK NEW_INSTALL_ROOT}")
luci=$(realpath "${2:?LuCI APK required}")
root=${3:?new install root required}
[ ! -e "$root" ] || { echo 'Install root already exists' >&2; exit 1; }
mkdir -p "$root/etc/apk/keys" "$root/tmp"
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
 base-files busybox uci luci-base rpcd jsonfilter procd ubus ca-bundle openssl-util
apk --root "$root" --arch aarch64_generic --no-scripts --no-network --allow-untrusted add "$core"
mkdir -p "$root/tmp/luci-modulecache"
touch "$root/tmp/luci-indexcache.test"
apk --root "$root" --arch aarch64_generic --no-network --allow-untrusted add "$luci"
apk --root "$root" info --exists luci-app-cowplay
for notice in LICENSE NOTICE.md RUNTIME-NOTICE.md TESLA-PROTOCOL-LICENSE LIVI-NOTICE; do
    [ -s "$root/usr/share/cowplay/$notice" ]
done
for f in usr/share/luci/menu.d/luci-app-cowplay.json usr/share/rpcd/acl.d/luci-app-cowplay.json \
 www/luci-static/resources/view/cowplay/main.js www/luci-static/resources/view/cowplay/tesla.js \
 www/luci-static/resources/cowplay/tesla.css www/luci-static/resources/cowplay/rpc.js \
 www/luci-static/resources/cowplay/style.css; do
    [ -f "$root/$f" ]
done
[ -x "$root/usr/libexec/rpcd/cowplay" ]
chroot "$root" /bin/sh -c '/usr/libexec/rpcd/cowplay list | jsonfilter -e "@.status" >/dev/null'
chroot "$root" /bin/sh -c '/usr/libexec/rpcd/cowplay list | jsonfilter -e "@.update_check.channel" | grep -q "^$"'
chroot "$root" /bin/sh -c '/usr/libexec/rpcd/cowplay list | jsonfilter -e "@.tesla_status" -e "@.tesla_climate_command" >/dev/null'
# post-install ran uci-defaults once and dropped LuCI caches; it did not enable or start anything.
grep -q "option port '18443'" "$root/etc/config/cowplay"
[ ! -e "$root/etc/uci-defaults/90-cowplay" ]
[ ! -e "$root/tmp/luci-indexcache.test" ] && [ ! -e "$root/tmp/luci-modulecache" ]
[ -z "$(find "$root/etc/rc.d" -name '*cowplay*' 2>/dev/null)" ]
printf "config cowplay 'main'\n\toption port '19443'\n" > "$root/etc/config/cowplay"
before=$(sha256sum "$root/etc/config/cowplay")
apk --root "$root" --no-network del luci-app-cowplay
[ ! -e "$root/usr/share/luci/menu.d/luci-app-cowplay.json" ]
[ ! -e "$root/www/luci-static/resources/view/cowplay/main.js" ]
[ ! -e "$root/usr/libexec/rpcd/cowplay" ]
[ -x "$root/usr/bin/cowplay" ]
[ "$before" = "$(sha256sum "$root/etc/config/cowplay")" ]
apk --root "$root" --arch aarch64_generic --no-network --allow-untrusted add "$luci"
[ "$before" = "$(sha256sum "$root/etc/config/cowplay")" ]
[ ! -e "$root/etc/uci-defaults/90-cowplay" ]
echo LUCI_APK_INSTALL_POSTINSTALL_REMOVE_REINSTALL_AND_POLICY_RETENTION_PASSED
