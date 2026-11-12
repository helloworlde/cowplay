# Sources and attribution

Based on LIVI by Lasse Heitgres (https://github.com/f-io/LIVI).
LIVI's synthetic iAP2 CSM test vectors are retained unchanged under GPL-3.0-or-later
and the same attribution term. Their source revision and SHA-256 are recorded in
research/livi-csm-lock.json; no additional LIVI runtime or dependency is included.
The temporary Ultra feasibility probe reuses LIVI protocol code under GPL-3.0-or-later,
with the additional attribution term in crates/server/src/ultra/livi/NOTICE.
The pinned revision is recorded in research/ultra-livi-lock.json. The initial probe used synthetic vehicle data; the current runtime uses the
authenticated Tesla read-only feed described below, with unavailable values omitted.

This work is GPL-3.0-only; see LICENSE. Rust pairing-window timing and confirmation rules are adapted from DashPlay `scripts/onecloud/pairing_window.py`; Wi-Fi security mapping from `platform_openwrt.py`. Original work: DashPlay contributors / yizhixiaoheigou, GPL-3.0. Exact commits are recorded in research/upstream-lock.json. Upstream copyright notices remain in the checked-out source.

OpenWrt packaging and F50 board integration are being researched from openwrt-dashplay and DashPlay. Their Java/Python runtime is not part of the Rust binary. No accessory identities, certificates, private keys, signing keys or vendor firmware are included. CarPlay and iPhone are Apple trademarks; this project is independent.

Platform-independent Rust core/server and the browser media assets are reused from the owner local f50-carplay-rust project. Their original PlayPort/DiPlay/DashPlay attribution and GPL-3.0 source notices are retained. The exact public source revisions are in research/reused-upstream-lock.json; imported local Rust source hashes are in research/reused-rust.json. Android/JNI, Java and Python runtimes are not included.

OpenWrt support adds Rust F50 child-process ownership, BlueZ Agent1/Profile1, bounded pairing, Unix FD transport, local-only import and procd packaging. zbus is used for D-Bus; Rust dependency versions and checksums are locked in Cargo.lock. Browser HTML/JS management assets are built directly into the executable.

OpenWrt 0.3.1 replaces handwritten HTTP, ACME/JWS/CSR and certificate parsing with reqwest (MIT/Apache-2.0), instant-acme (Apache-2.0), rcgen (MIT/Apache-2.0 through instant-acme), and RustCrypto x509-cert/der (MIT/Apache-2.0); tempfile (MIT/Apache-2.0) supplies private atomic-file staging. Exact versions/checksums are in Cargo.lock. The APK private runtime has its own component inventory and notices; see packaging/RUNTIME-NOTICE.md.
# Tesla vehicle-command protocol

The Tesla BLE protocol mapping and worked cryptographic test vectors follow Tesla,
Inc.'s `vehicle-command` at `0a582da430c4aca6d397fb6cbed5b4d6bb380125`, under
Apache-2.0. CowPlay implements bounded authenticated state reads and whitelisted
HVAC power/temperature actions on its existing BlueZ transport. Vehicle Monitor
enrollment remains the default; control uses a separate explicitly enrolled Driver
identity. It does not embed the upstream Go Bluetooth backend.
The source pin is `research/tesla-ble-lock.json`; the license is included as
`TESLA-PROTOCOL-LICENSE` in the installed notices directory.
