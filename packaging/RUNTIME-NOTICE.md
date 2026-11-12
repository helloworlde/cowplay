# Private userspace runtime

The CowPlay APK includes only dbus-daemon, btattach, bluetoothd, the musl loader,
and the transitive shared libraries reported by the loader. They are selected
from signature-verified official OpenWrt 25.12.6 packages, without modification.
No kernel module, firmware, identity, pairing, system D-Bus configuration or
system BlueZ service is installed by this payload.

`/usr/lib/cowplay/bluez/BUILD_PACKAGES.json` records build input versions, origins
and their package-declared licenses. It describes the disposable dependency
resolution root; kernel packages listed there are excluded from the APK.
`REPOSITORIES` records official package index URLs; `SHA256SUMS` covers all actual
runtime files. The build script pins the OpenWrt keyring commit and key checksum.

Corresponding sources and license notices are available from the exact OpenWrt
package sources: https://downloads.openwrt.org/sources/ and
https://git.openwrt.org/feed/packages/tree/?h=openwrt-25.12 .
Upstream projects: https://www.bluez.org/ (BlueZ), https://dbus.freedesktop.org/
(D-Bus), https://gitlab.gnome.org/GNOME/glib (GLib), https://musl.libc.org/
(musl), https://libexpat.github.io/ (Expat), https://github.com/PCRE2Project/pcre2
(PCRE2), and https://gcc.gnu.org/ (libgcc, including the GCC Runtime Library Exception).
These components retain their respective licenses; CowPlay does not relicense them.
Official release packages are signed; verify the pinned release key and signed
checksums before installation. Local build output is not a signed release.
