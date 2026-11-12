#!/bin/sh
set -eu
umask 077
version=${1:?Usage: install-linux-from-github.sh VERSION PUBLIC_KEY_SHA256 [--no-systemd]}
fingerprint=${2:?Public release key SHA256 required}
mode=${3:-}
[ "$#" -le 3 ] && { [ -z "$mode" ] || [ "$mode" = --no-systemd ]; } || exit 1
printf '%s\n' "$version" | grep -Eq '^(0|[1-9][0-9]{0,8})(\.(0|[1-9][0-9]{0,8})){2}-r(0|[1-9][0-9]{0,8})$' || { echo 'Invalid release version' >&2; exit 1; }
printf '%s\n' "$fingerprint" | grep -Eq '^[0-9a-f]{64}$' || { echo 'Invalid public-key fingerprint' >&2; exit 1; }
[ "$(uname -s)" = Linux ] && [ ! -f /etc/openwrt_release ] || { echo 'Use the signed APK packages on OpenWrt' >&2; exit 1; }
case "$(uname -m)" in x86_64) arch=x86_64;; aarch64|arm64) arch=arm64;; *) echo 'Unsupported architecture' >&2; exit 1;; esac
for tool in curl openssl sha256sum tar sort cmp awk; do command -v "$tool" >/dev/null 2>&1 || { echo "Missing download tool: $tool" >&2; exit 1; }; done
work=$(mktemp -d /tmp/cowplay-download.XXXXXX)
trap 'rm -rf "$work"' EXIT HUP INT TERM
base="https://github.com/helloworlde/cowplay/releases/download/v${version%-r0}"
archive="cowplay-$version-linux-$arch.tar.gz"
for file in release-key.pem SHA256SUMS SHA256SUMS.sig "$archive"; do
    case "$file" in "$archive") limit=134217728;; *) limit=16384;; esac
    curl --fail --location --proto '=https' --proto-redir '=https' --connect-timeout 15 --max-time 180 --max-filesize "$limit" "$base/$file" -o "$work/$file"
done
printf '%s  %s\n' "$fingerprint" "$work/release-key.pem" | sha256sum -c -
openssl dgst -sha256 -verify "$work/release-key.pem" -signature "$work/SHA256SUMS.sig" "$work/SHA256SUMS"
hash=$(awk -v name="$archive" '
    NF != 2 || length($1) != 64 || $1 ~ /[^0-9a-f]/ || $2 !~ /^[A-Za-z0-9][A-Za-z0-9._-]*$/ {bad=1}
    {if (seen[$2]++) bad=1}
    $2 == name {found++; value=$1}
    END {if (bad || found != 1) exit 1; print value}
' "$work/SHA256SUMS")
(cd "$work" && printf '%s  %s\n' "$hash" "$archive" | sha256sum -c -)
package="cowplay-$version-linux-$arch"
{
    printf '%s/\n' "$package"
    for name in cowplay install.sh cowplay.service config.example.json VERSION ARCH UPGRADE_FROM LICENSE NOTICE.md TESLA-PROTOCOL-LICENSE LIVI-NOTICE README.md; do printf '%s/%s\n' "$package" "$name"; done
} | LC_ALL=C sort > "$work/expected"
tar -tzf "$work/$archive" > "$work/members"
LC_ALL=C sort "$work/members" > "$work/actual"
cmp -s "$work/expected" "$work/actual" || { echo 'Unexpected or duplicate archive members' >&2; exit 1; }
tar -tvzf "$work/$archive" > "$work/types"
awk 'substr($0,1,1) != "-" && substr($0,1,1) != "d" {bad=1} END {exit bad}' "$work/types" || { echo 'Archive links or special files are refused' >&2; exit 1; }
tar -xzf "$work/$archive" -C "$work"
if [ -n "$mode" ]; then sh "$work/$package/install.sh" "$mode"; else sh "$work/$package/install.sh"; fi
