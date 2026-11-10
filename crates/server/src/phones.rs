//! Selected-phone admission and per-phone AirPlay records. Bluetooth is owned by the host.
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::watch;
pub type Controller = (Vec<u8>, [u8; 32]);
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub epoch: u64,
    pub device: Option<String>,
    pub admission: bool,
    store: Option<PathBuf>,
}
pub struct Candidate {
    device: String,
    record: Option<Controller>,
    store: Option<PathBuf>,
}
struct State {
    current: Snapshot,
    record: Option<Controller>,
    base: Option<PathBuf>,
    memory: BTreeMap<String, Option<Controller>>,
}
pub struct Phones {
    state: Mutex<State>,
    changes: watch::Sender<u64>,
    pub operation: tokio::sync::Mutex<()>,
}
pub fn device_component(device: &str) -> io::Result<&str> {
    let (adapter, value) = device.strip_prefix("/org/bluez/hci")
        .and_then(|p| p.split_once("/dev_"))
        .ok_or_else(|| io::Error::other("invalid phone device"))?;
    if adapter.parse::<u16>().ok().is_none_or(|n| n.to_string() != adapter) {
        return Err(io::Error::other("invalid phone adapter"));
    }
    if value.len() != 17
        || value.split('_').count() != 6
        || !value
            .split('_')
            .all(|s| s.len() == 2 && s.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(io::Error::other("invalid phone device"));
    }
    Ok(value)
}
fn slot(base: &Path, device: &str) -> io::Result<PathBuf> {
    Ok(base
        .with_extension("phones")
        .join(format!("{}.json", device_component(device)?)))
}
fn private_dir(parent: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    if std::fs::symlink_metadata(parent).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(io::Error::other("pairing directory is a symlink"));
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}
fn save(path: &Path, id: &[u8], key: &[u8; 32]) -> io::Result<()> {
    private_dir(
        path.parent()
            .ok_or_else(|| io::Error::other("pairing path"))?,
    )?;
    crate::pairing_store::write(path, id, key)
}
impl Phones {
    pub fn new(record: Option<Controller>, base: Option<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                current: Snapshot {
                    epoch: 0,
                    device: None,
                    admission: true,
                    store: base.clone(),
                },
                record,
                base,
                memory: BTreeMap::new(),
            }),
            changes: watch::channel(0).0,
            operation: Default::default(),
        })
    }
    /// Bind the legacy record only to the previously saved Bluetooth phone, before switching.
    pub fn initialize(&self, device: Option<&str>) -> io::Result<()> {
        if let Some(device) = device {
            device_component(device)?;
        }
        let mut s = self.state.lock().unwrap();
        let directory = s.base.as_ref().map(|p| p.with_extension("phones"));
        let marker = directory.as_ref().map(|p| p.join(".legacy-migrated"));
        let migrated = marker
            .as_ref()
            .map(|p| p.try_exists())
            .transpose()?
            .unwrap_or(false);
        if let Some(directory) = &directory {
            private_dir(directory)?;
        }
        let path = match (s.base.as_ref(), device) {
            (Some(base), Some(device)) => Some(slot(base, device)?),
            _ => None,
        };
        let record = match &path {
            Some(p) if p.try_exists()? => crate::pairing_store::read(p)?,
            Some(p) if !migrated => {
                if let Some((id, key)) = &s.record {
                    save(p, id, key)?;
                }
                s.record.clone()
            }
            Some(_) => None,
            None if device.is_some() => s.record.clone(),
            None => None,
        };
        // Finish one-time migration even with no known legacy owner. Never infer ownership
        // again from a newly selected Bluetooth phone after an interrupted PairSetup.
        if let Some(marker) = marker.filter(|_| !migrated) {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&marker)?
                .sync_all()?;
            std::fs::File::open(marker.parent().unwrap())?.sync_all()?;
        }
        s.current.device = device.map(str::to_owned);
        s.current.store = path;
        s.current.admission = device.is_some() && record.is_some();
        s.record = record.clone();
        if let Some(device) = device {
            s.memory.insert(device.into(), record);
        }
        Ok(())
    }
    /// Drop a forgotten phone's AirPlay record. The admitted current phone is refused.
    pub fn forget(&self, device: &str) -> io::Result<()> {
        device_component(device)?;
        let mut s = self.state.lock().unwrap();
        if s.current.device.as_deref() == Some(device) && s.current.admission {
            return Err(io::Error::other("current phone"));
        }
        if let Some(base) = &s.base {
            match std::fs::remove_file(slot(base, device)?) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        s.memory.remove(device);
        if s.current.device.as_deref() == Some(device) {
            s.current.device = None;
            s.current.store = None;
            s.record = None;
        }
        Ok(())
    }
    pub fn snapshot(&self) -> Snapshot {
        self.state.lock().unwrap().current.clone()
    }
    pub fn epoch(&self) -> u64 {
        self.state.lock().unwrap().current.epoch
    }
    pub fn is_current(&self, epoch: u64) -> bool {
        self.epoch() == epoch
    }
    /// Run a short synchronous commit while the admitted selection cannot change.
    /// The callback must not call back into `Phones` or block on external work.
    pub(crate) fn while_current<R>(&self, epoch: u64, commit: impl FnOnce() -> R) -> Option<R> {
        let s = self.state.lock().unwrap();
        (s.current.epoch == epoch && s.current.admission).then(commit)
    }
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }
    pub async fn changed_since(&self, epoch: u64) {
        let mut rx = self.subscribe();
        while self.is_current(epoch) {
            if rx.changed().await.is_err() {
                break;
            }
        }
    }
    pub fn pause(&self) -> io::Result<u64> {
        self.pause_expected(None)
    }
    pub fn pause_if_current(&self, epoch:u64) -> io::Result<u64> {
        self.pause_expected(Some(epoch))
    }
    fn pause_expected(&self, expected:Option<u64>) -> io::Result<u64> {
        let mut s = self.state.lock().unwrap();
        if expected.is_some_and(|epoch|epoch!=s.current.epoch) {
            return Err(io::Error::other("此操作已被新的手机选择替代"));
        }
        s.current.epoch = s
            .current
            .epoch
            .checked_add(1)
            .ok_or_else(|| io::Error::other("phone generation exhausted"))?;
        s.current.admission = false;
        let epoch = s.current.epoch;
        self.changes.send_replace(epoch);
        Ok(epoch)
    }
    pub fn prepare(&self, device: &str) -> io::Result<Candidate> {
        device_component(device)?;
        let s = self.state.lock().unwrap();
        let store = s.base.as_ref().map(|p| slot(p, device)).transpose()?;
        let record = match &store {
            Some(p) => crate::pairing_store::read(p)?,
            None => s.memory.get(device).cloned().flatten(),
        };
        Ok(Candidate {
            device: device.into(),
            record,
            store,
        })
    }
    /// Persist host selection while holding the same lock used by late PairSetup commits.
    pub fn select(
        &self,
        c: Candidate,
        epoch: u64,
        persist: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        let mut s = self.state.lock().unwrap();
        if s.current.epoch != epoch || s.current.admission {
            return Err(io::Error::other("phone selection changed"));
        }
        persist()?;
        s.current.device = Some(c.device.clone());
        s.current.store = c.store;
        s.record = c.record.clone();
        s.memory.insert(c.device, c.record);
        Ok(())
    }
    /// Only the selected phone's current iAP2 handoff may admit network sessions.
    pub fn handoff(&self, epoch: u64, device: &str) -> bool {
        let mut s = self.state.lock().unwrap();
        if s.current.epoch != epoch || s.current.device.as_deref() != Some(device) {
            return false;
        }
        s.current.admission = true;
        true
    }
    pub fn lookup(&self, snapshot: &Snapshot, id: &[u8]) -> Option<[u8; 32]> {
        let s = self.state.lock().unwrap();
        if s.current.epoch != snapshot.epoch
            || !s.current.admission
            || s.current.device != snapshot.device
        {
            return None;
        }
        s.record
            .as_ref()
            .filter(|(known, _)| known == id)
            .map(|(_, key)| *key)
    }
    pub fn save(&self, snapshot: &Snapshot, id: &[u8], key: &[u8; 32]) -> io::Result<()> {
        let mut s = self.state.lock().unwrap();
        if s.current.epoch != snapshot.epoch
            || !s.current.admission
            || s.current.device != snapshot.device
            || s.current.store != snapshot.store
        {
            return Err(io::Error::other("phone selection changed"));
        }
        if s.memory.iter().any(|(device, r)| {
            Some(device) != snapshot.device.as_ref()
                && r.as_ref().is_some_and(|(known, _)| known == id)
        }) {
            return Err(io::Error::other("controller belongs to another phone"));
        }
        if let Some(path) = &snapshot.store {
            save(path, id, key)?;
        }
        let record = Some((id.to_vec(), *key));
        s.record = record.clone();
        if let Some(device) = &snapshot.device {
            s.memory.insert(device.clone(), record);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const A: &str = "/org/bluez/hci0/dev_00_11_22_33_44_55";
    const B: &str = "/org/bluez/hci0/dev_00_11_22_33_44_66";
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "cowplay-phone-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn base(&self) -> PathBuf {
            self.0.join("controller.json")
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn selected(phones: &Phones, device: &str) -> Snapshot {
        let epoch = phones.pause().unwrap();
        phones
            .select(phones.prepare(device).unwrap(), epoch, || Ok(()))
            .unwrap();
        assert!(!phones.snapshot().admission);
        assert!(phones.handoff(epoch, device));
        phones.snapshot()
    }
    #[test]
    fn stale_auto_connect_cannot_advance_a_newer_phone_selection() {
        let phones=Phones::new(None,None);
        let old=phones.epoch();
        let latest=phones.pause().unwrap();
        assert!(phones.pause_if_current(old).is_err());
        assert_eq!(phones.epoch(),latest);
        assert_eq!(phones.pause_if_current(latest).unwrap(),latest+1);
    }
    #[test]
    fn legacy_a_and_new_b_survive_switching_back_and_restart() {
        use std::os::unix::fs::PermissionsExt;
        let dir = Directory::new();
        let base = dir.base();
        crate::pairing_store::write(&base, b"a", &[1; 32]).unwrap();
        let original = std::fs::read(&base).unwrap();
        let phones = Phones::new(
            crate::pairing_store::read(&base).unwrap(),
            Some(base.clone()),
        );
        phones.initialize(Some(A)).unwrap();
        assert_eq!(phones.lookup(&phones.snapshot(), b"a"), Some([1; 32]));
        let b = selected(&phones, B);
        phones.save(&b, b"b", &[2; 32]).unwrap();
        assert_eq!(phones.lookup(&b, b"a"), None);
        assert_eq!(
            std::fs::metadata(base.with_extension("phones"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let restarted = Phones::new(
            crate::pairing_store::read(&base).unwrap(),
            Some(base.clone()),
        );
        restarted.initialize(Some(B)).unwrap();
        assert_eq!(restarted.lookup(&restarted.snapshot(), b"b"), Some([2; 32]));
        let a = selected(&restarted, A);
        assert_eq!(restarted.lookup(&a, b"a"), Some([1; 32]));
        assert_eq!(restarted.lookup(&a, b"b"), None);
        assert_eq!(
            std::fs::read(&base).unwrap(),
            original,
            "legacy backup remains intact"
        );
    }
    #[test]
    fn bluetooth_selection_without_pair_setup_does_not_remigrate_legacy_a_to_b() {
        let dir = Directory::new();
        let base = dir.base();
        crate::pairing_store::write(&base, b"a", &[1; 32]).unwrap();
        let phones = Phones::new(
            crate::pairing_store::read(&base).unwrap(),
            Some(base.clone()),
        );
        phones.initialize(Some(A)).unwrap();
        selected(&phones, B); // Bluetooth selection persisted; no AirPlay PairSetup yet.
        let restarted = Phones::new(
            crate::pairing_store::read(&base).unwrap(),
            Some(base.clone()),
        );
        restarted.initialize(Some(B)).unwrap();
        assert!(!restarted.snapshot().admission);
        assert!(restarted
            .save(&restarted.snapshot(), b"b", &[2; 32])
            .is_err());
        assert_eq!(
            restarted.lookup(&restarted.snapshot(), b"a"),
            None,
            "legacy A must never be inferred to belong to B at restart"
        );
        let a = selected(&restarted, A);
        assert_eq!(restarted.lookup(&a, b"a"), Some([1; 32]));
    }
    #[test]
    fn stale_handoff_and_pair_setup_cannot_overwrite_current_phone() {
        let phones = Phones::new(None, None);
        phones.initialize(Some(A)).unwrap();
        assert!(phones.handoff(phones.epoch(), A));
        let a = phones.snapshot();
        phones.save(&a, b"a", &[1; 32]).unwrap();
        let epoch = phones.pause().unwrap();
        assert!(!phones.handoff(a.epoch, A));
        assert!(!phones.handoff(epoch, B));
        assert!(phones.save(&a, b"late-a", &[9; 32]).is_err());
        phones
            .select(phones.prepare(B).unwrap(), epoch, || Ok(()))
            .unwrap();
        assert!(!phones.handoff(epoch, A));
        assert!(phones.handoff(epoch, B));
        let b = phones.snapshot();
        phones.save(&b, b"b", &[2; 32]).unwrap();
        assert!(phones.save(&a, b"late-a", &[9; 32]).is_err());
        assert_eq!(phones.lookup(&b, b"b"), Some([2; 32]));
        let a = selected(&phones, A);
        assert_eq!(phones.lookup(&a, b"a"), Some([1; 32]));
    }
    #[test]
    fn failed_selection_or_pairing_write_does_not_publish_unsaved_state() {
        let dir = Directory::new();
        let phones = Phones::new(None, Some(dir.base()));
        phones.initialize(Some(A)).unwrap();
        let epoch = phones.pause().unwrap();
        assert!(phones
            .select(phones.prepare(B).unwrap(), epoch, || Err(io::Error::other(
                "synthetic disk failure"
            )))
            .is_err());
        assert_eq!(phones.snapshot().device.as_deref(), Some(A));
        let b = selected(&phones, B);
        let path = slot(&dir.base(), B).unwrap();
        // A directory occupying the record path makes the atomic replacement fail.
        std::fs::create_dir_all(&path).unwrap();
        assert!(phones.save(&b, b"b", &[2; 32]).is_err());
        assert_eq!(phones.lookup(&b, b"b"), None);
        assert!(path.is_dir(), "no record was written");
        std::fs::remove_dir(&path).unwrap();
        phones.save(&b, b"b", &[2; 32]).unwrap();
        assert_eq!(phones.lookup(&b, b"b"), Some([2; 32]));
    }
    #[test]
    fn unknown_legacy_owner_is_never_assigned_to_the_first_new_phone() {
        let dir = Directory::new();
        let base = dir.base();
        crate::pairing_store::write(&base, b"unknown", &[8; 32]).unwrap();
        let phones = Phones::new(
            crate::pairing_store::read(&base).unwrap(),
            Some(base.clone()),
        );
        phones.initialize(None).unwrap();
        assert!(!phones.snapshot().admission);
        let restarted = Phones::new(crate::pairing_store::read(&base).unwrap(), Some(base));
        restarted.initialize(Some(B)).unwrap();
        assert_eq!(restarted.lookup(&restarted.snapshot(), b"unknown"), None);
    }
    #[test]
    fn only_a_valid_local_bluez_phone_can_name_a_pairing_slot() {
        assert_eq!(device_component(A).unwrap(), "00_11_22_33_44_55");
        assert_eq!(device_component("/org/bluez/hci1/dev_00_11_22_33_44_55").unwrap(), "00_11_22_33_44_55");
        for device in [
            "/tmp/file",
            "/org/bluez/hci01/dev_00_11_22_33_44_55",
            "/org/bluez/hci65536/dev_00_11_22_33_44_55",
            "/org/bluez/hci0/dev_../../secret",
            "/org/bluez/hci0/dev_00_11_22_33_44_ZZ",
        ] {
            assert!(device_component(device).is_err());
        }
    }
}
