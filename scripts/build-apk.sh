#!/bin/sh
# Run on Linux with apk-tools v3 (mkpkg enabled), against a staged aarch64 musl release binary.
set -eu
apk_tool=${APK_TOOL:-apk}
binary=${1:?Usage: build-apk.sh AARCH64_MUSL_BINARY OUTPUT_APK}
output=${2:?output required}
runtime=${3:?signature-verified private runtime directory required}
# Check before mkpkg labels the payload architecture. The checker must run on the build host.
magic=$(dd if="$binary" bs=1 count=6 2>/dev/null | od -An -tx1 | tr -d ' \n')
machine=$(dd if="$binary" bs=1 skip=18 count=2 2>/dev/null | od -An -tx1 | tr -d ' \n')
[ "$magic" = 7f454c460201 ] && [ "$machine" = b700 ] || { echo 'Requires an AArch64 ELF64 executable' >&2; exit 1; }
checker=${IMAGE_CHECKER:-$binary}
"$checker" verify-image "$binary"
here=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT HUP INT TERM
chmod 755 "$stage"
mkdir -p "$stage/usr/bin" "$stage/usr/libexec" "$stage/lib/upgrade/keep.d" "$stage/etc/init.d" "$stage/usr/share/cowplay" "$stage/usr/lib/cowplay/bluez"
(cd "$runtime" && sha256sum -c SHA256SUMS)
cp -a "$runtime/." "$stage/usr/lib/cowplay/bluez/"
cp "$binary" "$stage/usr/bin/cowplay"
cp "$here/packaging/cowplay.init" "$stage/etc/init.d/cowplay"
cp "$here/packaging/cowplay-restore.init" "$stage/etc/init.d/cowplay-restore"
cp "$here/packaging/cowplay-restore.sh" "$stage/usr/libexec/cowplay-restore"
cp "$here/packaging/cowplay.keep" "$stage/lib/upgrade/keep.d/cowplay"
cp "$here/packaging/cowplay.sysupgrade" "$stage/lib/upgrade/cowplay.sh"
cp "$here/packaging/releases.example.json" "$stage/usr/share/cowplay/"
cp "$here/packaging/config.example.json" "$stage/usr/share/cowplay/"
cp "$here/LICENSE" "$here/NOTICE.md" "$here/packaging/RUNTIME-NOTICE.md" "$here/packaging/TESLA-PROTOCOL-LICENSE" "$stage/usr/share/cowplay/"
chmod 755 "$stage/usr/bin/cowplay" "$stage/etc/init.d/cowplay" "$stage/etc/init.d/cowplay-restore" "$stage/usr/libexec/cowplay-restore"
"$apk_tool" mkpkg --info name:cowplay --info version:0.3.14-r17 \
    --info arch:aarch64_generic --info license:GPL-3.0-only \
    --info 'description:CowPlay wireless CarPlay receiver with private BlueZ/D-Bus userspace' \
    --info 'depends:procd ubus jsonfilter ca-bundle openssl-util' \
    --script "post-install:$here/packaging/cowplay.post-install" \
    --script "pre-upgrade:$here/packaging/cowplay.pre-upgrade" \
    --script "post-upgrade:$here/packaging/cowplay.post-upgrade" \
    --script "pre-deinstall:$here/packaging/cowplay.pre-deinstall" \
    --files "$stage" --output "$output"
