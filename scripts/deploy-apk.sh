#!/bin/sh
# Upload the core and LuCI packages to the F50 over one SSH session and upgrade both.
# Changes the device: run only with the current task's authorization. The core package
# scripts preserve upgrades' run/boot state; first install enables next boot only.
set -eu
device=${1:?Usage: deploy-apk.sh USER@HOST VERSION CORE_APK LUCI_APK}
version=${2:?version required}
core=${3:?core APK required}
luci=${4:?LuCI APK required}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT HUP INT TERM
cp "$core" "$work/cowplay.apk"
cp "$luci" "$work/luci-app-cowplay.apk"
printf '%s\n' "$version" > "$work/VERSION"
if command -v sha256sum >/dev/null 2>&1; then sum='sha256sum'; else sum='shasum -a 256'; fi
(cd "$work" && $sum cowplay.apk luci-app-cowplay.apk > SHA256SUMS)
cat "$work/SHA256SUMS"
cat > "$work/install.sh" <<'REMOTE'
#!/bin/sh
set -eu
umask 077
dir=$1
version=$(cat "$dir/VERSION")
(cd "$dir" && sha256sum -c SHA256SUMS)
# Private state copy stays on the device, as in earlier authorized upgrades.
if [ -d /etc/cowplay/state ]; then
    mkdir -p /etc/cowplay/backups
    backup=/etc/cowplay/backups/pre-$version-$(date +%Y%m%d-%H%M%S).tgz
    tar -czf "$backup" -C /etc/cowplay state
    echo "STATE_BACKUP=$backup"
fi
if apk --no-network info --exists cowplay >/dev/null 2>&1; then upgrading=yes; else upgrading=no; fi
if /etc/init.d/cowplay enabled 2>/dev/null; then before=enabled; else before=disabled; fi
apk --no-network --allow-untrusted add --upgrade "$dir/cowplay.apk" "$dir/luci-app-cowplay.apk"
apk --no-network info --exists "cowplay=$version" "luci-app-cowplay=$version"
if /etc/init.d/cowplay enabled 2>/dev/null; then after=enabled; else after=disabled; fi
if [ "$upgrading" = yes ]; then
    [ "$before" = "$after" ] || { echo "AUTOSTART_CHANGED $before -> $after" >&2; exit 1; }
else
    [ "$after" = enabled ] || { echo FIRST_INSTALL_AUTOSTART_NOT_ENABLED >&2; exit 1; }
fi
echo "INSTALLED cowplay=$version luci-app-cowplay=$version AUTOSTART=$after"
REMOTE
# One connection, so a password login prompts once. ustar keeps busybox tar happy.
COPYFILE_DISABLE=1 tar --format ustar -cf - -C "$work" VERSION SHA256SUMS install.sh cowplay.apk luci-app-cowplay.apk |
    ssh "$device" 'd=$(mktemp -d /tmp/cowplay-deploy.XXXXXX) && tar -xf - -C "$d" && sh "$d/install.sh" "$d"; rc=$?; rm -rf "$d"; exit $rc'
