#!/bin/sh
# Run locally as administrator on the F50, after checking this script's SHA-256.
set -eu
umask 077
version=${1:?Usage: install-openwrt-from-github.sh VERSION PUBLIC_KEY_SHA256}
fingerprint=${2:?Public release key SHA256 required}
[ "$#" -eq 2 ] || exit 1
printf '%s\n' "$version" | grep -Eq '^(0|[1-9][0-9]{0,8})(\.(0|[1-9][0-9]{0,8})){2}(-r(0|[1-9][0-9]{0,8}))?$' || { echo 'Invalid version' >&2; exit 1; }
printf '%s\n' "$fingerprint" | grep -Eq '^[0-9a-f]{64}$' || { echo 'Invalid public-key fingerprint' >&2; exit 1; }
[ "$(id -u)" -eq 0 ] && [ -f /etc/openwrt_release ] && [ "$(cat /etc/apk/arch)" = aarch64_generic ] || { echo 'Requires an OpenWrt aarch64_generic administrator session' >&2; exit 1; }
for tool in curl openssl sha256sum jsonfilter awk; do command -v "$tool" >/dev/null 2>&1 || { echo "Install missing official package/tool first: $tool" >&2; exit 1; }; done
work=$(mktemp -d /tmp/cowplay-bootstrap.XXXXXX)
trap 'rm -rf "$work"' EXIT HUP INT TERM
tag="v${version%-r0}"
base="https://github.com/helloworlde/cowplay/releases/download/$tag"
for file in release-key.pem SHA256SUMS SHA256SUMS.sig cowplay-updater-aarch64; do
    case "$file" in cowplay-updater-aarch64) limit=33554432;; *) limit=16384;; esac
    curl --fail --location --proto '=https' --proto-redir '=https' --connect-timeout 15 --max-time 180 --max-filesize "$limit" "$base/$file" -o "$work/$file"
done
printf '%s  %s\n' "$fingerprint" "$work/release-key.pem" | sha256sum -c -
openssl dgst -sha256 -verify "$work/release-key.pem" -signature "$work/SHA256SUMS.sig" "$work/SHA256SUMS"
hash=$(awk '
    NF != 2 || length($1) != 64 || $1 ~ /[^0-9a-f]/ || $2 !~ /^[A-Za-z0-9][A-Za-z0-9._-]*$/ {bad=1}
    {if (seen[$2]++) bad=1}
    $2 == "cowplay-updater-aarch64" {found++; value=$1}
    END {if (bad || found != 1) exit 1; print value}
' "$work/SHA256SUMS")
(cd "$work" && printf '%s  cowplay-updater-aarch64\n' "$hash" | sha256sum -c -)
chmod 700 "$work/cowplay-updater-aarch64"
updater=$work/cowplay-updater-aarch64
"$updater" update-bootstrap
result=$(printf '{"tag":"%s"}' "$tag" | "$updater" update check)
job=$(printf '%s' "$result" | jsonfilter -e '@.id')
[ -n "$job" ] || { echo "$result" >&2; exit 1; }
applied=no
# Workers are independent of this terminal. A disconnect never kills an installation.
for tick in $(seq 1 240); do
    result=$("$updater" update status)
    current=$(printf '%s' "$result" | jsonfilter -e '@.id')
    [ "$current" = "$job" ] || { echo 'Update task changed; inspect CowPlay System page' >&2; exit 1; }
    phase=$(printf '%s' "$result" | jsonfilter -e '@.phase')
    printf '%s\n' "$result" | jsonfilter -e '@.message'
    case "$phase" in
        ready)
            [ "$applied" = no ] || { echo 'Unexpected task state; inspect System page' >&2; exit 1; }
            printf '{"id":"%s"}' "$job" | "$updater" update apply
            applied=yes ;;
        complete|current) echo 'CowPlay is ready. Reload LuCI → Services → CowPlay.'; exit 0 ;;
        failed|interrupted|superseded|acknowledged) echo 'Update did not complete; inspect CowPlay System page and retained backup.' >&2; exit 1 ;;
    esac
    sleep 3
done
echo 'Update is still running. Check CowPlay System page; do not launch a second installation.' >&2
exit 1
