#!/bin/sh
# Build a private userspace runtime from signature-verified OpenWrt 25.12.6 packages.
# Run inside an aarch64 Linux container with apk-tools v3, curl and binutils.
set -eu
out=${1:?Usage: prepare-bluez.sh NEW_OUTPUT_DIRECTORY}
[ "$(uname -m)" = aarch64 ] || { echo 'Requires aarch64 Linux' >&2; exit 1; }
[ ! -e "$out" ] || { echo 'Output already exists; choose a new build directory' >&2; exit 1; }
mkdir -p "$(dirname "$out")"
parent=$(CDPATH='' cd -- "$(dirname "$out")" && pwd)
work=$(mktemp -d "$parent/.cowplay-bluez.XXXXXX")
trap 'rm -rf "$work"' EXIT HUP INT TERM
root=$work/root
stage=$work/runtime
mkdir -p "$root/etc/apk/keys" "$stage/usr/bin" "$stage/usr/lib" "$stage/lib"
key=$root/etc/apk/keys/openwrt-25.12.pem
curl --fail --location --connect-timeout 15 --max-time 60 \
  'https://git.openwrt.org/keyring/plain/apk/openwrt-25.12.pem?id=f0670054fd3aa9616bcd8d0d0357a1a15c65b758' -o "$key"
printf '%s  %s\n' 27d61d827d64ccf2f46dce5f52aec6279404d4a00686dafb22f52f3b4ee675f8 "$key" | sha256sum -c -
cat > "$root/etc/apk/repositories" <<'REPOS'
https://downloads.openwrt.org/releases/25.12.6/targets/armsr/armv8/packages/packages.adb
https://downloads.openwrt.org/releases/25.12.6/packages/aarch64_generic/base/packages.adb
https://downloads.openwrt.org/releases/25.12.6/packages/aarch64_generic/packages/packages.adb
https://downloads.openwrt.org/releases/25.12.6/targets/armsr/armv8/kmods/6.12.112-1-67e205f26d0458c4d12f1fd315216398/packages.adb
REPOS
# Kernel dependencies are resolved only in this disposable root. No scripts run;
# the payload whitelist below cannot include modules, firmware or system services.
apk --root "$root" --arch aarch64_generic --no-scripts add --initdb \
  bluez-daemon=5.83-r1 bluez-utils=5.83-r1 dbus=1.16.2-r4
for name in dbus-daemon btattach bluetoothd; do
  test -x "$root/usr/bin/$name"
  cp -p "$root/usr/bin/$name" "$stage/usr/bin/$name"
done
# The private loader resolves transitive dependencies; copy only those SONAMEs.
cp -p "$root/lib/libc.so" "$stage/lib/libc.so"
ln -s libc.so "$stage/lib/ld-musl-aarch64.so.1"
: > "$work/needed"
for name in dbus-daemon btattach bluetoothd; do
  "$root/lib/ld-musl-aarch64.so.1" --library-path "$root/usr/lib:$root/lib" --list "$root/usr/bin/$name" > "$work/deps"
  awk '$2 == "=>" && $1 != "libc.so" {print $1}' "$work/deps" >> "$work/needed"
done
sort -u "$work/needed" | while IFS= read -r name; do
  case "$name" in */*) echo 'Invalid shared library name' >&2; exit 1;; esac
  if [ -e "$root/usr/lib/$name" ]; then dir=usr/lib; else dir=lib; fi
  cp -pL "$root/$dir/$name" "$stage/$dir/$name"
done
loader=$stage/lib/ld-musl-aarch64.so.1
# Resolve every dependency with the private loader, before packaging.
for name in dbus-daemon btattach bluetoothd; do
  "$loader" --library-path "$stage/usr/lib:$stage/lib" --list "$stage/usr/bin/$name"
done
"$loader" --library-path "$stage/usr/lib:$stage/lib" "$stage/usr/bin/bluetoothd" --version
"$loader" --library-path "$stage/usr/lib:$stage/lib" "$stage/usr/bin/dbus-daemon" --version
"$loader" --library-path "$stage/usr/lib:$stage/lib" "$stage/usr/bin/btattach" --help
cp "$root/etc/apk/repositories" "$stage/REPOSITORIES"
apk --root "$root" query --installed --format json --fields name,version,license,origin '*' > "$stage/BUILD_PACKAGES.json"
grep -q '"name".*"bluez-daemon"' "$stage/BUILD_PACKAGES.json"
grep -q '"name".*"dbus"' "$stage/BUILD_PACKAGES.json"
# A reproducible component inventory; no identity or device files are read.
(cd "$stage" && find lib usr -type f -exec sha256sum '{}' ';' | sort > SHA256SUMS)
mv "$stage" "$out"
