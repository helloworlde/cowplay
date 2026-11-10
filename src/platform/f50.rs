//! F50 vendor Bluetooth bring-up. Owns only its children and storage link.
//! Based on DashPlay board_zte_f50.py (GPL-3.0); no package/kernel/network changes.
use std::{
    fs, io,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub type Result<T> = crate::RuntimeResult<T>;
pub const RUNTIME: &str = "/usr/lib/cowplay/bluez";
pub const RUN: &str = "/var/run/cowplay";
pub const BUS: &str = "unix:path=/var/run/cowplay/system_bus_socket";

pub fn private_dir(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("refusing a symlink for a private directory".into());
    }
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

type Stop = tokio::sync::watch::Receiver<bool>;
fn pause(duration: Duration, stop: &Stop) -> Result<()> {
    let until = Instant::now() + duration;
    loop {
        if *stop.borrow() {
            return Err("Bluetooth start cancelled".into());
        }
        let remaining = until.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        std::thread::sleep(remaining.min(Duration::from_millis(50)));
    }
}
fn own(command: &mut Command) {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        let parent = std::process::id() as libc::pid_t;
        // SAFETY: only async-signal-safe system calls run between fork and exec.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    libc::_exit(125)
                }
                Ok(())
            });
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = command;
}
fn run(mut command: Command, seconds: u64, stop: &Stop) -> Result<()> {
    pause(Duration::ZERO, stop)?;
    let operation = command.get_program().to_owned();
    command.stdin(Stdio::null());
    own(&mut command);
    let mut child = command.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        if *stop.borrow() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{} cancelled", operation.to_string_lossy()).into());
        }
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(format!("{} exited: {status}", operation.to_string_lossy()).into())
            };
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{} timed out", operation.to_string_lossy()).into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn valid_address(text: &str) -> bool {
    let parts: Vec<_> = text.trim().split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && u8::from_str_radix(p, 16).is_ok())
        && u8::from_str_radix(parts[0], 16).is_ok_and(|b| b & 1 == 0)
        && parts.iter().any(|p| *p != "00")
}
fn radios() -> Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    for entry in fs::read_dir("/sys/class/rfkill")? {
        let path = entry?.path();
        if fs::read_to_string(path.join("type"))?.trim() == "bluetooth" {
            result.push(path)
        }
    }
    Ok(result)
}
fn hci_present() -> bool {
    fs::read_dir("/sys/class/bluetooth").is_ok_and(|entries| {
        entries
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with("hci"))
    })
}

pub struct Board {
    children: Vec<Child>,
    storage: Option<PathBuf>,
    lock: Option<fs::File>,
    stop: Stop,
}
impl Board {
    pub fn start(state: &Path, address_file: &Path, stop: Stop) -> Result<Self> {
        pause(Duration::ZERO, &stop)?;
        if !Path::new("/etc/openwrt_release").is_file() || std::env::consts::ARCH != "aarch64" {
            return Err("F50 backend requires aarch64 OpenWrt".into());
        }
        for file in [
            "usr/bin/dbus-daemon",
            "usr/bin/btattach",
            "usr/bin/bluetoothd",
        ] {
            if !Path::new(RUNTIME).join(file).is_file() {
                return Err(format!("missing private BlueZ component: {file}").into());
            }
        }
        if hci_present() {
            return Err("Bluetooth controller already owned; stop its owner first".into());
        }
        let address = fs::read_to_string(address_file)?;
        if !valid_address(&address) {
            return Err("invalid existing Bluetooth address".into());
        }
        private_dir(Path::new(RUN))?;
        let lock = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(Path::new(RUN).join("owner.lock"))?;
        use std::os::fd::AsRawFd;
        // SAFETY: lock owns a valid descriptor for this lifetime. No other files are touched.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("another cowplay owns Bluetooth".into());
        }
        let mut board = Self {
            children: Vec::new(),
            storage: None,
            lock: Some(lock),
            stop,
        };
        private_dir(state)?;
        let storage = state.join("bluetooth");
        private_dir(&storage)?;
        let storage = fs::canonicalize(storage)?;
        let link = Path::new("/var/lib/bluetooth");
        match fs::symlink_metadata(link) {
            Ok(_) => {
                if !fs::symlink_metadata(link)?.file_type().is_symlink()
                    || fs::canonicalize(link)? != storage
                {
                    return Err("/var/lib/bluetooth belongs to another service; existing pairings preserved".into());
                }
                // An earlier crash may leave our own link; adopt its cleanup without changing stored pairings.
                board.storage = Some(storage.clone());
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all("/var/lib")?;
                symlink(&storage, link)?;
                board.storage = Some(storage);
            }
            Err(e) => return Err(e.into()),
        }
        let mut command = Command::new("modprobe");
        command.arg("sprdbt_tty");
        run(command, 10, &board.stop)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !Path::new("/dev/ttyBT0").exists() && Instant::now() < deadline {
            pause(Duration::from_millis(50), &board.stop)?;
        }
        if !Path::new("/dev/ttyBT0").exists() {
            return Err("vendor Bluetooth tty did not appear".into());
        }
        let radio_list = radios()?;
        if radio_list.len() != 1 {
            return Err("expected one vendor Bluetooth rfkill".into());
        }
        let radio = &radio_list[0];
        if fs::read_to_string(radio.join("hard"))?.trim() != "0" {
            return Err("Bluetooth is hardware blocked".into());
        }
        let initialise = || {
            let mut cmd = Command::new("/opt/mu300/bin/mu300-bt-init");
            cmd.args([
                "-d",
                "/dev/ttyBT0",
                "-p",
                "/lib/firmware/bt_configure_pskey.ini",
                "-r",
                "/lib/firmware/bt_configure_rf.ini",
                "-a",
                address.trim(),
            ]);
            // Vendor output may contain the private device address.
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
            run(cmd, 30, &board.stop)
        };
        fs::write(radio.join("state"), "0\n")?;
        pause(Duration::from_millis(200), &board.stop)?;
        fs::write(radio.join("state"), "1\n")?;
        if initialise().is_err() {
            pause(Duration::ZERO, &board.stop)?;
            fs::write(radio.join("state"), "0\n")?;
            pause(Duration::from_secs(1), &board.stop)?;
            fs::write(radio.join("state"), "1\n")?;
            pause(Duration::from_secs(2), &board.stop)?;
            initialise()?;
        }
        fs::write(
            Path::new(RUN).join("dbus.conf"),
            format!(
                "<busconfig><type>system</type><listen>{BUS}</listen><auth>EXTERNAL</auth><policy user=\"root\"><allow own=\"*\"/><allow send_destination=\"*\"/><allow receive_sender=\"*\"/></policy></busconfig>\n"
            ),
        )?;
        fs::write(
            Path::new(RUN).join("bluez.conf"),
            "[General]\nName=cowplay\nControllerMode=dual\nDiscoverableTimeout=180\nPairableTimeout=180\n[Policy]\nAutoEnable=false\n",
        )?;
        board.spawn(
            "dbus-daemon",
            &["--nofork", &format!("--config-file={RUN}/dbus.conf")],
        )?;
        board.wait_for(|| Path::new(RUN).join("system_bus_socket").exists(), 5)?;
        board.spawn("btattach", &["-B", "/dev/ttyBT0", "-P", "h4"])?;
        board.spawn("bluetoothd", &["-n", "-f", &format!("{RUN}/bluez.conf")])?;
        board.wait_for(hci_present, 10)?;
        for radio in radios()? {
            if fs::read_to_string(radio.join("hard"))?.trim() != "0" {
                return Err("Bluetooth became hardware blocked".into());
            }
            if fs::read_to_string(radio.join("soft"))?.trim() != "0" {
                fs::write(radio.join("soft"), "0\n")?;
            }
        }
        Ok(board)
    }
    fn spawn(&mut self, name: &str, args: &[&str]) -> Result<()> {
        pause(Duration::ZERO, &self.stop)?;
        let mut cmd = Command::new(Path::new(RUNTIME).join("lib/ld-musl-aarch64.so.1"));
        cmd.arg("--library-path")
            .arg(format!("{RUNTIME}/usr/lib:{RUNTIME}/lib"))
            .arg(Path::new(RUNTIME).join("usr/bin").join(name))
            .args(args)
            .env("DBUS_SYSTEM_BUS_ADDRESS", BUS)
            .stdin(Stdio::null());
        // Each child is directly owned; never kill processes by name or scan another runtime.
        own(&mut cmd);
        self.children.push(cmd.spawn()?);
        Ok(())
    }
    fn wait_for(&mut self, predicate: impl Fn() -> bool, seconds: u64) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        while !predicate() {
            pause(Duration::ZERO, &self.stop)?;
            self.check()?;
            if Instant::now() >= deadline {
                return Err("Bluetooth start timed out".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        pause(Duration::ZERO, &self.stop)?;
        self.check()
    }
    pub fn check(&mut self) -> Result<()> {
        for child in &mut self.children {
            if let Some(status) = child.try_wait()? {
                return Err(format!("Bluetooth child exited: {status}").into());
            }
        }
        Ok(())
    }
}
use std::os::unix::fs::OpenOptionsExt;
impl Drop for Board {
    fn drop(&mut self) {
        let until = Instant::now() + Duration::from_secs(3);
        for (index, child) in self.children.iter_mut().enumerate().rev() {
            // BlueZ must finish while its HCI transport and D-Bus are still alive.
            // The shared deadline also bounds partial-startup cleanup.
            // SAFETY: id is the positive PID of a child that has not been reaped/reused.
            if child.try_wait().is_ok_and(|s| s.is_none()) {
                unsafe {
                    libc::kill(child.id() as i32, libc::SIGTERM);
                }
            }
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Err(_) => break,
                    Ok(None) => {}
                }
                if Instant::now() >= until {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            let role = match index {
                0 => "DBUS",
                1 => "HCI_TRANSPORT",
                2 => "BLUEZ_DAEMON",
                _ => "OWNED_CHILD",
            };
            f50_server::diag_log!(Info, "bt", "stop_stage_complete={role}");
        }
        // Do not power the shared wireless chip again after its HCI transport has exited.
        if let Some(storage) = &self.storage {
            let link = Path::new("/var/lib/bluetooth");
            if fs::read_link(link).is_ok_and(|p| &p == storage) {
                let _ = fs::remove_file(link);
            }
        }
        self.lock.take();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn child_fixture(script: &str) -> (Board, PathBuf) {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "cowplay-owned-children-{}-{nonce}",
            std::process::id()
        ));
        private_dir(&path).unwrap();
        let (_, stop) = tokio::sync::watch::channel(false);
        let mut board = Board {
            children: Vec::new(),
            storage: None,
            lock: None,
            stop,
        };
        for role in ["DBUS", "HCI", "BLUEZ"] {
            board.children.push(
                Command::new("/bin/sh")
                    .args(["-c", script])
                    .env("F50_TEST_DIR", &path)
                    .env("F50_TEST_ROLE", role)
                    .spawn()
                    .unwrap(),
            );
        }
        let ready = Instant::now() + Duration::from_secs(2);
        while ["DBUS", "HCI", "BLUEZ"]
            .iter()
            .any(|role| !path.join(format!("{role}.ready")).exists())
        {
            assert!(
                Instant::now() < ready,
                "synthetic child did not become ready"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        (board, path)
    }
    #[test]
    fn child_cleanup_keeps_dependencies_until_their_users_exit() {
        let script = r#"
trap '
case "$F50_TEST_ROLE" in
BLUEZ) /bin/sleep 0.1; touch "$F50_TEST_DIR/BLUEZ.done" ;;
HCI) test -f "$F50_TEST_DIR/BLUEZ.done" || printf "EARLY_HCI\n" >> "$F50_TEST_DIR/events"; touch "$F50_TEST_DIR/HCI.done" ;;
DBUS) test -f "$F50_TEST_DIR/HCI.done" || printf "EARLY_DBUS\n" >> "$F50_TEST_DIR/events" ;;
esac
printf "%s\n" "$F50_TEST_ROLE" >> "$F50_TEST_DIR/events"
exit 0
' TERM
touch "$F50_TEST_DIR/$F50_TEST_ROLE.ready"
while :; do /bin/sleep 0.02; done
"#;
        let (board, path) = child_fixture(script);
        drop(board);
        assert_eq!(
            fs::read_to_string(path.join("events")).unwrap(),
            "BLUEZ\nHCI\nDBUS\n"
        );
        fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn child_cleanup_shares_one_wait_budget() {
        let (board, path) = child_fixture(
            "trap '' TERM; touch \"$F50_TEST_DIR/$F50_TEST_ROLE.ready\"; exec /bin/sleep 30",
        );
        let began = Instant::now();
        drop(board);
        // Three individual 3s budgets would take 9s; the production wait budget is shared.
        assert!(began.elapsed() < Duration::from_secs(6));
        fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn address_validation_rejects_multicast_and_malformed() {
        assert!(valid_address("02:01:02:03:04:05\n"));
        for a in [
            "01:01:02:03:04:05",
            "00:00:00:00:00:00",
            "02:01:02:03:04",
            "xx:01:02:03:04:05",
        ] {
            assert!(!valid_address(a));
        }
    }
    #[test]
    fn stopping_cancels_a_running_owned_initialiser() {
        let (stop, receiver) = tokio::sync::watch::channel(false);
        let cancel = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            stop.send(true).unwrap();
        });
        let mut command = Command::new("/bin/sleep");
        command.arg("30");
        let began = Instant::now();
        let error = run(command, 30, &receiver).unwrap_err();
        cancel.join().unwrap();
        assert!(error.to_string().contains("cancelled"));
        assert!(began.elapsed() < Duration::from_secs(2));
    }
}
