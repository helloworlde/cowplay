#!/bin/sh
# Package luci-app-cowplay the way OpenWrt's luci.mk does (PKGARCH all -> apk arch noarch).
# Run where apk-tools v3 with mkpkg is available, e.g. the Alpine build container.
set -eu
apk_tool=${APK_TOOL:-apk}
output=${1:?Usage: build-luci-apk.sh OUTPUT_APK}
here=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
app=$here/luci-app-cowplay
version=$(cat "$here/packaging/VERSION")
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT HUP INT TERM
files=$stage/files
mkdir -p "$files/www" "$files/lib/apk/packages"
cp -R "$app/htdocs/." "$files/www/"
cp -R "$app/root/." "$files/"
find "$files" -type d -exec chmod 755 '{}' +
find "$files" -type f -exec chmod 644 '{}' +
# uhttpd CGI handlers and rpcd plugins must remain executable after normalization.
chmod 755 "$files/www/cgi-bin/cowplay-key-download" "$files/usr/libexec/rpcd/cowplay" "$files/etc/uci-defaults/90-cowplay"
# default_postinst finds this package's uci-defaults through its file list.
(cd "$files" && find . \( -type f -o -type l \) | sed 's|^\.||' | sort) > "$stage/list"
mv "$stage/list" "$files/lib/apk/packages/luci-app-cowplay.list"
{ echo '#!/bin/sh'; echo 'export PKG_UPGRADE=1'; sed '/^#!/d' "$here/packaging/luci-app-cowplay.post-install"; } > "$stage/post-upgrade"
"$apk_tool" mkpkg --info name:luci-app-cowplay --info "version:$version" \
    --info arch:noarch --info license:GPL-3.0-only \
    --info 'description:CowPlay wireless CarPlay receiver management' \
    --info 'depends:luci-base rpcd jsonfilter cowplay' \
    --script "post-install:$here/packaging/luci-app-cowplay.post-install" \
    --script "post-upgrade:$stage/post-upgrade" \
    --files "$files" --output "$output"
