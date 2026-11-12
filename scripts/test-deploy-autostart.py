#!/usr/bin/env python3
"""Run deploy-apk.sh's device-side installer in a disposable synthetic root."""
import hashlib
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile

project = Path(__file__).resolve().parent.parent
source = (project / "scripts/deploy-apk.sh").read_text().split("<<'REMOTE'\n", 1)[1].split("\nREMOTE", 1)[0]
with tempfile.TemporaryDirectory(prefix="deploy-autostart-", dir=project / "target") as raw:
    root = Path(raw)
    init = root / "init"
    init.write_text('''#!/bin/sh
case "$1" in
enabled) test -f "$TEST_DIR/enabled";;
enable) touch "$TEST_DIR/enabled";;
*) exit 1;;
esac
''')
    init.chmod(0o755)
    checksum = root / "sha256sum"
    checksum.write_text("#!" + sys.executable + "\n" + "import hashlib,sys\nfrom pathlib import Path\nassert sys.argv[1]=='-c'\nfor line in Path(sys.argv[2]).read_text().splitlines():\n digest,name=line.split('  ',1)\n assert hashlib.sha256(Path(name).read_bytes()).hexdigest()==digest\n")
    checksum.chmod(0o755)
    apk = root / "apk"
    apk.write_text('''#!/bin/sh
case "$*" in
*" add --upgrade "*)
    [ -e "$TEST_DIR/installed" ] || "$TEST_INIT" enable
    touch "$TEST_DIR/installed";;
*" info --exists cowplay"*) test -f "$TEST_DIR/installed";;
*) exit 1;;
esac
''')
    apk.chmod(0o755)
    state = root / "cowplay/state"
    state.mkdir(parents=True)
    retained = state / "retained-test-data"
    retained.write_text("synthetic identity and pairings")
    installer = root / "install.sh"
    installer.write_text(source.replace("/etc/init.d/cowplay", shlex.quote(str(init))).replace("/etc/cowplay", str(root / "cowplay")))
    payload = root / "payload"
    payload.mkdir()
    (payload / "VERSION").write_text("synthetic-r0\n")
    hashes = []
    for name in ["cowplay.apk", "luci-app-cowplay.apk"]:
        content = b"synthetic package: installer contract only"
        (payload / name).write_bytes(content)
        hashes.append(hashlib.sha256(content).hexdigest() + "  " + name)
    (payload / "SHA256SUMS").write_text("\n".join(hashes) + "\n")
    env = dict(os.environ, TEST_DIR=str(root), TEST_INIT=str(init), PATH=str(root) + ":/usr/bin:/bin")
    for installed, enabled in [(False, False), (True, False), (True, True)]:
        for name, exists in [("installed", installed), ("enabled", enabled)]:
            p = root / name
            if exists:
                p.touch()
            elif p.exists():
                p.unlink()
        result = subprocess.run(["/bin/sh", str(installer), str(payload)], env=env, text=True, capture_output=True, timeout=10)
        assert result.returncode == 0, (installed, enabled, result.stdout, result.stderr)
        assert (root / "enabled").exists() == (enabled if installed else True)
        assert retained.read_text() == "synthetic identity and pairings"
print("DEPLOY_FRESH_INSTALL_ENABLES_AND_UPGRADE_PRESERVES_AUTOSTART_PASSED")
