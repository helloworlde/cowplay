#!/bin/sh
# Offline restoration after a configuration-preserving sysupgrade. Never flashes firmware.
set -eu
umask 077
base=/etc/cowplay
cache=$base/firmware-restore
init=/etc/init.d/cowplay
[ -f "$cache/current" ] || exit 0
fail() {
    printf '%s\n' "$1" > "$cache/result"
    logger -t cowplay-restore "$1" 2>/dev/null || :
    exit 1
}
[ ! -e "$base/updates/active" ] && [ ! -e /var/run/cowplay-network/active ] || fail 'RESTORE_PENDING: another app or port transaction is active'
[ ! -e "$cache/blocked" ] || fail 'RESTORE_PENDING: recovery cache is locked; inspect app state and prepare a verified current cache'
for tool in apk jsonfilter openssl sha256sum; do command -v "$tool" >/dev/null 2>&1 || fail "RESTORE_PENDING: missing $tool; signed cache and private state retained"; done
id=$(cat "$cache/current")
case "$id" in ''|*[!0-9a-f]*) fail 'RESTORE_PENDING: invalid cache id';; esac
[ "${#id}" -eq 32 ] || fail 'RESTORE_PENDING: invalid cache id'
slot=$cache/$id
manifest=$slot/cowplay-release.json
openssl dgst -sha256 -verify "$base/release-key.pem" -signature "$slot/cowplay-release.sig" "$manifest" >/dev/null 2>&1 || fail 'RESTORE_PENDING: manifest signature rejected'
get() { jsonfilter -i "$manifest" -e "$1"; }
[ "$(get '@.schema')" = 1 ] && [ "$(get '@.platform')" = openwrt-apk ] || fail 'RESTORE_PENDING: unsupported manifest'
repo=$(jsonfilter -i "$base/releases.json" -e '@.repository')
[ "$(get '@.repository')" = "$repo" ] || fail 'RESTORE_PENDING: repository mismatch'
arch=$(cat /etc/apk/arch)
[ "$arch" = aarch64_generic ] && [ "$(get '@.arch')" = "$arch" ] || fail 'RESTORE_PENDING: unsupported architecture'
release=$(sed -n "s/^DISTRIB_RELEASE=['\"]\{0,1\}\([^'\"]*\)['\"]\{0,1\}$/\1/p" /etc/openwrt_release)
[ -n "$release" ] && get '@.openwrt_releases[*]' | grep -Fqx "$release" || fail 'RESTORE_PENDING: firmware release was not declared compatible; install a reviewed compatible release manually'
version=$(get '@.version')
case "$version" in ''|*[!0-9.r-]*) fail 'RESTORE_PENDING: invalid package version';; esac
[ "$(get '@.tag')" = "v${version%-r0}" ] || fail 'RESTORE_PENDING: tag mismatch'
restore_boot() {
    case "$1" in
        enabled) "$init" enable >/dev/null 2>&1 || fail 'RESTORE_PENDING: cannot restore boot preference';;
        disabled) "$init" disable >/dev/null 2>&1 || fail 'RESTORE_PENDING: cannot restore boot preference';;
        *) fail 'RESTORE_PENDING: invalid original boot preference';;
    esac
}
if apk --no-network info --exists "cowplay=$version" "luci-app-cowplay=$version" >/dev/null 2>&1; then
    printf '%s\n' 'READY: both packages are installed; no service changes' > "$cache/result"
    exit 0
fi
for name in cowplay luci-app-cowplay; do
    if apk --no-network info --exists "$name" >/dev/null 2>&1 && ! apk --no-network info --exists "$name=$version" >/dev/null 2>&1; then
        fail 'RESTORE_PENDING: a different package version is installed; use the managed updater'
    fi
done
[ ! -x "$init" ] || ! "$init" running >/dev/null 2>&1 || fail 'RESTORE_PENDING: CowPlay is running; use the managed updater'
prior_boot=disabled
[ ! -L /etc/rc.d/S95cowplay ] || prior_boot=enabled
keys=$(mktemp -d /tmp/cowplay-restore-keys.XXXXXX)
trap 'rm -rf "$keys"' EXIT HUP INT TERM
cp "$base/release-key.pem" "$keys/release.pem"
for index in 0 1; do
    if [ "$index" = 0 ]; then name=cowplay; pkgarch=$arch; else name=luci-app-cowplay; pkgarch=noarch; fi
    file=$name-$version-$pkgarch.apk
    [ "$(get "@.packages[$index].name")" = "$name" ] && [ "$(get "@.packages[$index].arch")" = "$pkgarch" ] && [ "$(get "@.packages[$index].file")" = "$file" ] || fail 'RESTORE_PENDING: package identity mismatch'
    sum=$(get "@.packages[$index].sha256")
    case "$sum" in ''|*[!0-9a-f]*) fail 'RESTORE_PENDING: invalid hash';; esac
    [ "${#sum}" -eq 64 ] || fail 'RESTORE_PENDING: invalid hash'
    printf '%s  %s\n' "$sum" "$slot/$file" | sha256sum -c - >/dev/null 2>&1 || fail 'RESTORE_PENDING: cached package hash rejected'
    apk --keys-dir "$keys" --no-network verify "$slot/$file" >/dev/null 2>&1 || fail 'RESTORE_PENDING: cached APK signature rejected'
done
core=$slot/cowplay-$version-$arch.apk
luci=$slot/luci-app-cowplay-$version-noarch.apk
apk --keys-dir "$keys" --no-network --no-cache --repositories-file /dev/null add --simulate "$core" "$luci" "cowplay=$version" "luci-app-cowplay=$version" >/dev/null 2>&1 || fail 'RESTORE_PENDING: offline dependencies unavailable; install compatible firmware dependencies before retrying'
apk --keys-dir "$keys" --no-network --no-cache --repositories-file /dev/null add "$core" "$luci" "cowplay=$version" "luci-app-cowplay=$version" >/dev/null 2>&1 || fail 'RESTORE_PENDING: APK restore failed; inspect package state before retrying'
apk --no-network info --exists "cowplay=$version" "luci-app-cowplay=$version" >/dev/null 2>&1 || fail 'RESTORE_PENDING: installed package versions do not match'
restore_boot "$prior_boot"
printf '%s\n' 'RESTORED: core and LuCI installed; original boot preference restored; private state retained' > "$cache/result"
