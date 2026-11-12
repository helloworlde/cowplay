#!/bin/sh
set -eu
umask 077
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
root=${DESTDIR:-}
manual=no
case "${1:-}" in '') ;; --no-systemd) manual=yes;; *) echo 'Usage: install.sh [--no-systemd]' >&2; exit 1;; esac
[ "$#" -le 1 ] || exit 1
case "$root" in ''|/*) ;; *) echo 'DESTDIR must be an absolute staging root' >&2; exit 1;; esac
if [ -n "$root" ]; then
    mkdir -p "$root"
    root=$(CDPATH='' cd -- "$root" && pwd -P)
    [ "$root" != / ] || { echo 'DESTDIR must not resolve to the live root filesystem' >&2; exit 1; }
fi
staging=no
if [ -n "$root" ]; then staging=yes; manual=yes; else [ "$(id -u)" -eq 0 ] || { echo 'Run as root, or set DESTDIR for a file-only staging install' >&2; exit 1; }; fi
[ "$(uname -s)" = Linux ] || { echo 'This archive requires Linux' >&2; exit 1; }
[ ! -f "$root/etc/openwrt_release" ] || { echo 'OpenWrt requires the signed APK packages' >&2; exit 1; }
case "$(uname -m)" in x86_64) host_arch=x86_64;; aarch64|arm64) host_arch=arm64;; *) echo 'Unsupported CPU architecture' >&2; exit 1;; esac
version=$(cat "$here/VERSION")
arch=$(cat "$here/ARCH")
valid_version() { printf '%s\n' "$1" | grep -Eq '^(0|[1-9][0-9]{0,8})(\.(0|[1-9][0-9]{0,8})){2}-r(0|[1-9][0-9]{0,8})$'; }
valid_version "$version" && [ "$arch" = "$host_arch" ] || { echo 'Invalid version or archive architecture mismatch' >&2; exit 1; }
for tool in flock timeout tar cmp sha256sum sort; do command -v "$tool" >/dev/null 2>&1 || { echo "Missing installer tool: $tool" >&2; exit 1; }; done
"$here/cowplay" verify-image "$here/cowplay" "$arch"
prefix=$root/usr/local
base=$prefix/lib/cowplay
meta=$root/var/lib/cowplay-installer
unit=$root/etc/systemd/system/cowplay.service
files='cowplay install.sh cowplay.service config.example.json VERSION ARCH UPGRADE_FROM LICENSE NOTICE.md TESLA-PROTOCOL-LICENSE LIVI-NOTICE README.md'
for file in $files; do [ -f "$here/$file" ] && [ ! -L "$here/$file" ] || { echo "Invalid package file: $file" >&2; exit 1; }; done
mkdir -p "$meta" "$meta/transactions"
chmod 0700 "$meta" "$meta/transactions"
exec 9> "$meta/lock"
flock -n 9 || { echo 'Another CowPlay installation is running' >&2; exit 1; }
if [ -e "$meta/recovery-required" ]; then echo 'Previous installation needs inspection; see /var/lib/cowplay-installer before retrying' >&2; exit 1; fi
old=
if [ -e "$base/current" ] || [ -L "$base/current" ]; then
    [ -L "$base/current" ] || { echo 'Unmanaged current path' >&2; exit 1; }
    target=$(readlink "$base/current")
    old=${target#releases/}
    valid_version "$old" && [ "$target" = "releases/$old" ] && [ -f "$base/releases/$old/VERSION" ] && [ "$(cat "$base/releases/$old/VERSION")" = "$old" ] || { echo 'Unmanaged existing release' >&2; exit 1; }
    [ "$(cat "$base/releases/$old/ARCH")" = "$arch" ] || { echo 'Installed architecture mismatch' >&2; exit 1; }
    newest=$(printf '%s\n%s\n' "$old" "$version" | LC_ALL=C sort -V | tail -n 1)
    [ "$newest" = "$version" ] || { echo 'Downgrades are refused; old state may not be compatible' >&2; exit 1; }
    [ "$old" = "$version" ] || grep -Fqx "$old" "$here/UPGRADE_FROM" || { echo 'Release does not declare compatibility with the installed version' >&2; exit 1; }
fi
if [ -e "$prefix/bin/cowplay" ] || [ -L "$prefix/bin/cowplay" ]; then
    [ -n "$old" ] && [ -L "$prefix/bin/cowplay" ] && [ "$(readlink "$prefix/bin/cowplay")" = '../lib/cowplay/current/cowplay' ] || { echo 'Unmanaged existing binary; review migration before installation' >&2; exit 1; }
fi
if [ -e "$prefix/share/cowplay" ] || [ -L "$prefix/share/cowplay" ]; then
    [ -n "$old" ] && [ -L "$prefix/share/cowplay" ] && [ "$(readlink "$prefix/share/cowplay")" = '../lib/cowplay/current' ] || { echo 'Unmanaged existing share directory' >&2; exit 1; }
fi
use_systemd=no
if [ "$manual" = no ] && [ -d "$root/run/systemd/system" ]; then
    command -v systemctl >/dev/null 2>&1 || { echo 'systemctl is unavailable' >&2; exit 1; }
    use_systemd=yes
fi
was_running=no
old_pid=0
boot_before=
if [ "$use_systemd" = yes ]; then
    load_code=0
    load_state=$(timeout 10 systemctl show cowplay.service --property=LoadState --value) || load_code=$?
    case "$load_state:$load_code" in
        not-found:0|not-found:4) fragment=; drops=;;
        loaded:0)
            fragment=$(timeout 10 systemctl show cowplay.service --property=FragmentPath --value)
            drops=$(timeout 10 systemctl show cowplay.service --property=DropInPaths --value);;
        *) echo 'Cannot safely query the systemd unit' >&2; exit 1;;
    esac
    [ -z "$drops" ] || { echo 'Custom systemd drop-ins exist; use reviewed manual service management' >&2; exit 1; }
    if [ -n "$fragment" ] || [ -e "$unit" ] || [ -L "$unit" ]; then
        [ -n "$old" ] && [ "$fragment" = "$unit" ] && [ ! -L "$unit" ] && cmp -s "$unit" "$base/releases/$old/cowplay.service" || { echo 'Unmanaged or modified systemd unit; review it before updating' >&2; exit 1; }
        state=$(timeout 10 systemctl show cowplay.service --property=ActiveState --value)
        case "$state" in active) was_running=yes;; inactive|failed) ;; *) echo 'Service is transitioning; retry after it settles' >&2; exit 1;; esac
        old_pid=$(timeout 10 systemctl show cowplay.service --property=MainPID --value)
        case "$old_pid" in ''|*[!0-9]*) echo 'Cannot identify old service PID' >&2; exit 1;; esac
        [ "$was_running" = no ] || [ "$old_pid" -gt 1 ] || { echo 'Running service has no valid PID' >&2; exit 1; }
        boot_before=$(timeout 10 systemctl is-enabled cowplay.service 2>/dev/null || true)
        case "$boot_before" in enabled|disabled|enabled-runtime|static|indirect) ;; *) echo 'Cannot determine boot preference' >&2; exit 1;; esac
    fi
fi
if [ -d "$base/releases/$version" ]; then
    for file in $files; do cmp -s "$here/$file" "$base/releases/$version/$file" || { echo 'Same version has different contents; publish a new version' >&2; exit 1; }; done
    if [ "$old" = "$version" ]; then echo "Already installed: $version; no service changes"; exit 0; fi
fi
job=$(mktemp -d "$meta/transactions/install.XXXXXX")
printf '%s\n' "$version" > "$job/version"
printf '%s\n' "$old" > "$job/previous-version"
printf '%s\n' "$was_running" > "$job/was-running"
printf '%s\n' "$boot_before" > "$job/boot-preference"
printf '%s\n' preparing > "$job/status"
stopped=no
stopping=no
switched=no
stage=
finish() {
    rc=$?
    trap - EXIT HUP INT TERM
    [ -z "$stage" ] || rm -rf "$stage"
    if [ "$rc" -ne 0 ]; then
        printf '%s\n' failed > "$job/status"
        if [ "$switched" = yes ]; then
            if [ "$use_systemd" = yes ] && [ "$was_running" = yes ]; then timeout 35 systemctl stop cowplay.service || true; fi
            printf '%s\n' "$job" > "$meta/recovery-required"
            echo "Program switch occurred; inspect $job and system logs. No automatic downgrade or state restore." >&2
        elif [ "$stopped" = yes ] && [ "$was_running" = yes ]; then
            if timeout 35 systemctl start cowplay.service && timeout 10 systemctl is-active --quiet cowplay.service; then
                rm -f "$meta/recovery-required"
            else echo 'Old service did not resume; inspect system logs' >&2; fi
        elif [ "$stopping" = no ]; then
            rm -f "$meta/recovery-required"
        else echo 'Old service stop was uncertain; inspect transaction before retrying' >&2
        fi
        sync
    fi
    exit "$rc"
}
trap finish EXIT
trap 'exit 130' HUP INT TERM
for directory in "$root/usr" "$prefix" "$prefix/lib" "$base" "$base/releases" "$prefix/bin" "$prefix/share"; do
    if [ ! -d "$directory" ]; then mkdir "$directory"; chmod 0755 "$directory"; fi
done
needed=$(du -sk "$here" | awk '{printf "%.0f\n",$1*2+16384}')
for directory in "$root/etc/cowplay" "$root/var/lib/cowplay"; do
    if [ -d "$directory" ]; then needed=$((needed + $(du -sk "$directory" | awk '{printf "%.0f\n",$1*2}'))); fi
done
available=$(df -Pk "$base" | awk 'END {print $4}')
backup_available=$(df -Pk "$meta" | awk 'END {print $4}')
[ "$available" -ge "$needed" ] && [ "$backup_available" -ge "$needed" ] || { echo 'Insufficient space for package staging and private backup' >&2; exit 1; }
stage=$(mktemp -d "$base/.staging.XXXXXX")
chmod 0755 "$stage"
for file in $files; do cp "$here/$file" "$stage/$file"; chmod 0644 "$stage/$file"; done
chmod 0755 "$stage/cowplay" "$stage/install.sh"
printf '%s\n' "$job" > "$meta/recovery-required"
sync
if [ "$was_running" = yes ]; then
    stopping=yes
    printf '%s\n' stopping > "$job/status"
    timeout 35 systemctl stop cowplay.service
    n=0
    while :; do
        state=$(timeout 10 systemctl show cowplay.service --property=ActiveState --value)
        pid=$(timeout 10 systemctl show cowplay.service --property=MainPID --value)
        if [ "$state" = inactive ] && [ "$pid" = 0 ] && ! kill -0 "$old_pid" 2>/dev/null; then break; fi
        [ "$n" -lt 20 ] || { echo 'Old process did not stop; program files were not changed' >&2; exit 1; }
        n=$((n+1)); sleep 1
    done
    stopped=yes
fi
printf '%s\n' backing_up > "$job/status"
set --
for path in etc/cowplay var/lib/cowplay; do [ ! -e "$root/$path" ] || set -- "$@" "$path"; done
if [ "$#" -gt 0 ]; then timeout 120 tar -czf "$job/private-state.tgz" -C "$root/" "$@"; chmod 0600 "$job/private-state.tgz"; fi
printf '%s\n' installing > "$job/status"
if [ ! -d "$base/releases/$version" ]; then mv "$stage" "$base/releases/$version"; stage=; fi
ln -s "releases/$version" "$base/.current.$$"
switched=yes
mv -fT "$base/.current.$$" "$base/current"
ln -s '../lib/cowplay/current/cowplay' "$prefix/bin/.cowplay.$$"
mv -fT "$prefix/bin/.cowplay.$$" "$prefix/bin/cowplay"
ln -s '../lib/cowplay/current' "$prefix/share/.cowplay.$$"
mv -fT "$prefix/share/.cowplay.$$" "$prefix/share/cowplay"
if [ ! -e "$root/var/lib/cowplay" ]; then
    mkdir -p "$root/var/lib/cowplay"
    chmod 0700 "$root/var/lib/cowplay"
    "$prefix/bin/cowplay" init-access "$root/var/lib/cowplay"
fi
if [ "$use_systemd" = yes ]; then
    for directory in "$root/etc" "$root/etc/systemd" "$root/etc/systemd/system"; do
        if [ ! -d "$directory" ]; then mkdir "$directory"; chmod 0755 "$directory"; fi
    done
    cp "$here/cowplay.service" "$unit.new.$$"
    chmod 0644 "$unit.new.$$"
    mv -fT "$unit.new.$$" "$unit"
    timeout 15 systemctl daemon-reload
    if [ -n "$boot_before" ]; then
        boot_after=$(timeout 10 systemctl is-enabled cowplay.service 2>/dev/null || true)
        [ "$boot_before" = "$boot_after" ] || { echo 'Boot preference changed unexpectedly; inspect service before continuing' >&2; exit 1; }
    fi
    if [ "$was_running" = yes ]; then
        printf '%s\n' starting > "$job/status"
        timeout 35 systemctl start cowplay.service
        n=0
        while ! timeout 10 systemctl is-active --quiet cowplay.service; do
            [ "$n" -lt 15 ] || { echo 'New service did not become active' >&2; exit 1; }
            n=$((n+1)); sleep 1
        done
        sleep 1
        timeout 10 systemctl is-active --quiet cowplay.service
    fi
fi
sync
printf '%s\n' complete > "$job/status"
sync
rm -f "$meta/recovery-required"
sync
echo "Installed $version ($arch). Configuration, pairings and certificates retained."
if [ "$use_systemd" = no ]; then echo 'Files installed only; manage the service explicitly.';
elif [ "$was_running" = no ]; then echo 'Service remains stopped; no boot preference was enabled.'; fi
