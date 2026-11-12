#!/usr/bin/env python3
"""Exercise the actual lifecycle scripts against isolated service/PID/HCI fixtures."""
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import threading

project = Path(__file__).resolve().parent.parent
with tempfile.TemporaryDirectory(prefix="lifecycle-", dir=project / "target") as raw:
    root = Path(raw)
    marker = root / "upgrade-running"
    init = root / "init"
    events = root / "events"
    running = root / "running"
    state = root / "private-state"
    state.write_text("retained identity and pairings")
    release_dir = root / "cowplay-config"
    release_dir.mkdir()
    release_source = release_dir / "releases.json"
    release_key = release_dir / "release-key.pem"
    bootstrap_events = root / "bootstrap-events"
    cowplay = root / "cowplay"
    cowplay.write_text('''#!/bin/sh
[ "$1" = update-bootstrap ] || exit 1
printf 'bootstrap\\n' >> "$TEST_DIR/bootstrap-events"
printf '{"repository":"example/cowplay"}\\n' > "$TEST_DIR/cowplay-config/releases.json"
printf 'synthetic-public-key\\n' > "$TEST_DIR/cowplay-config/release-key.pem"
''')
    cowplay.chmod(0o755)
    init.write_text('''#!/bin/sh
case "$1" in
running) test -f "$TEST_DIR/running";;
start) touch "$TEST_DIR/running"; echo start >> "$TEST_DIR/events";;
enable) echo enable >> "$TEST_DIR/events";;
stop) rm -f "$TEST_DIR/running"; echo stop >> "$TEST_DIR/events";;
disable) echo disable >> "$TEST_DIR/events";;
esac
''')
    init.chmod(0o755)
    for name, body in {"ubus": "printf '{}\\n'", "jsonfilter": 'printf "%s\\n" "$TEST_PID"'}.items():
        f = root / name
        f.write_text("#!/bin/sh\n" + body + "\n")
        f.chmod(0o755)
    hooks = {}
    for name in ["post-install", "pre-upgrade", "post-upgrade", "pre-deinstall"]:
        source = (project / "packaging" / f"cowplay.{name}").read_text()
        source = source.replace("/etc/init.d/cowplay", shlex.quote(str(init)))
        source = source.replace("/usr/bin/cowplay", shlex.quote(str(cowplay)))
        source = source.replace("/etc/cowplay/", str(release_dir) + "/")
        source = source.replace("/var/run/cowplay-upgrade-running", shlex.quote(str(marker)))
        source = source.replace("/sys/class/bluetooth/hci0", shlex.quote(str(root / "hci0")))
        hooks[name] = root / name
        hooks[name].write_text(source)
    env = dict(os.environ, TEST_DIR=str(root), TEST_PID="99999999", PATH=str(root) + ":/usr/bin:/bin")
    env.pop("IPKG_INSTROOT", None)
    def run(name, **extra):
        return subprocess.run(["/bin/sh", str(hooks[name])], env=dict(env, **extra), check=True, capture_output=True, text=True, timeout=25)
    run("post-install")
    assert release_source.exists() and release_key.exists(), "direct APK install must initialize the update source and public key"
    assert bootstrap_events.read_text().splitlines() == ["bootstrap"]
    assert events.read_text().splitlines() == ["enable"], "first install enables next boot without starting"
    assert not running.exists()
    before = events.read_bytes()
    release_source.unlink()
    release_key.unlink()
    run("post-install", IPKG_INSTROOT=str(root))
    assert not release_source.exists() and not release_key.exists(), "offline image assembly must not initialize the host update source"
    assert bootstrap_events.read_text().splitlines() == ["bootstrap"]
    assert events.read_bytes() == before, "offline install must not change the host boot setting"
    run("pre-upgrade")
    run("post-upgrade")
    assert release_source.exists() and release_key.exists(), "upgrading a direct APK installation must initialize its missing update source"
    assert bootstrap_events.read_text().splitlines() == ["bootstrap", "bootstrap"]
    release_source.write_text('{"repository":"example/custom-source"}')
    release_key.write_text('custom-public-key')
    assert not running.exists(), "a stopped service must stay stopped during upgrade"
    assert not marker.exists()
    running.touch()
    child = subprocess.Popen(["/bin/sleep", "0.2"])
    reap = threading.Thread(target=child.wait)
    reap.start()
    run("pre-upgrade", TEST_PID=str(child.pid))
    assert child.poll() == 0, "pre-upgrade must wait for the old process, not just its stop request"
    reap.join()
    assert marker.exists() and not running.exists()
    run("post-upgrade")
    assert release_source.read_text() == '{"repository":"example/custom-source"}'
    assert release_key.read_text() == 'custom-public-key'
    assert bootstrap_events.read_text().splitlines() == ["bootstrap", "bootstrap"], "upgrades must preserve administrator-selected trust"
    assert running.exists() and not marker.exists()
    run("pre-deinstall")
    assert not running.exists()
    assert events.read_text().splitlines() == ["enable", "stop", "start", "stop", "disable"]
    assert state.read_text() == "retained identity and pairings"
    # An HCI that never disappears must suppress post-upgrade startup.
    (root / "hci0").touch()
    running.touch()
    fake_sleep = root / "sleep"
    fake_sleep.write_text("#!/bin/sh\nexit 0\n")
    fake_sleep.chmod(0o755)
    timeout = run("pre-upgrade")
    assert "automatic restart skipped" in timeout.stderr
    run("post-upgrade")
    assert not running.exists() and not marker.exists()
    before = events.read_bytes()
    run("pre-deinstall", IPKG_INSTROOT=str(root))
    assert events.read_bytes() == before, "offline image assembly must not operate services"
    # A partial administrator setup is preserved for explicit repair, not replaced.
    release_key.unlink()
    run("post-upgrade")
    assert release_source.read_text() == '{"repository":"example/custom-source"}' and not release_key.exists()
    assert bootstrap_events.read_text().splitlines() == ["bootstrap", "bootstrap"]
print("LIFECYCLE_STOP_WAIT_RESUME_TIMEOUT_AND_STATE_RETENTION_PASSED")
