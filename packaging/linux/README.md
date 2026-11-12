# CowPlay Linux distribution

This archive targets the architecture in ARCH and embeds the web UI in `cowplay`.
It contains no private identity, pairing, Wi-Fi password, certificate or access token.
It uses existing system BlueZ, D-Bus and a separately configured Wi-Fi access point.
Do not use this tarball on OpenWrt; use the matching signed APK packages instead.

Verify the release public-key fingerprint, the detached SHA256SUMS signature and
this archive's exact SHA-256 before extracting or running any included script.
The release's `install-linux-from-github.sh` automates the authenticated archive
download; that script must itself be verified before execution.

Run `sudo sh ./install.sh` from the extracted directory. It validates architecture,
static ELF format, package version and supported upgrade source before installation.
First installation does not enable or start CowPlay. Upgrades of its managed systemd
service stop and wait for the old process, keep configuration/state and boot preference,
then restore only the previous running state. It refuses unmanaged installations,
downgrades, unknown state compatibility and administrator-modified systemd units.
For a manually managed service, explicitly use `--no-systemd` after stopping it yourself.
`DESTDIR=/absolute/staging/root` performs a file-only staged install without host services.

Installed paths:
- /usr/local/bin/cowplay: stable program link
- /usr/local/lib/cowplay/releases/VERSION: retained immutable release files
- /usr/local/share/cowplay: current examples, notices and installer
- /etc/cowplay/config.json: user-owned configuration; never created or overwritten
- /var/lib/cowplay: default private state; never replaced or removed
- /var/lib/cowplay-installer: private transaction records and pre-upgrade backups

Copy /usr/local/share/cowplay/config.example.json to /etc/cowplay/config.json and
configure the explicit system Bluetooth backend, your existing accessory identity,
selected adapter, access-point details and trusted HTTPS certificates. Protect config
and private files with mode 0600, private directories with 0700. Prepare the optional
management token described in the example. Then start CowPlay manually, or explicitly
run `systemctl enable --now cowplay` when systemd and the required hardware are ready.
The installer never configures the access point, Bluetooth service, drivers or firmware.

Installer tools: POSIX sh, coreutils (including timeout and mv -T), util-linux flock,
tar, and systemctl only for systemd mode. Downloads additionally need curl, OpenSSL
and a valid system CA bundle. No Rust, Node, Python, or compiler is needed at runtime.

Upgrade backups cover the default /etc/cowplay and /var/lib/cowplay locations.
Referenced private files outside them need an administrator's independent backup.
A durable recovery gate is written before stopping the service. Power loss, forced
exit, or a post-switch installation/start failure leaves the recorded version
and backups for inspection; it never automatically runs an old binary on newer state.
Do not install in parallel with other package/service/state changes. Inspect the
transaction status and journal before recovery. Systemd running status does not prove
Bluetooth, Wi-Fi, CarPlay media, audio or browser compatibility; these need device tests.

First-use access code: a new default /var/lib/cowplay directory is initialized by
this installer. On first opening CowPlay, choose and confirm a six-digit code,
or explicitly choose no access code. With a code enabled, every new browser must
verify it. Without one, devices that can reach the page can enter directly.
QR links only open the page. The receiver preserves the chosen access mode.
For an explicitly new custom state directory, run `sudo cowplay init-access DIR`
once before starting. It refuses an existing access-token file. Do not use it to
repair missing/corrupt access state: preserve and inspect that state instead.
Changing or resetting the code requires the protected management page or local
control socket. Restarting/upgrading keeps the code and remembered browsers.
