//! External CowPlay logs: daily/size rotation and retention of this writer's archives.
use std::os::unix::fs::{OpenOptionsExt, MetadataExt};
use std::sync::{Arc, atomic::{AtomicBool, AtomicU64, Ordering}};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::PathBuf,
};

pub(crate) const FILE_LIMIT: u64 = 32 * 1024 * 1024;
pub(crate) const ARCHIVE_LIMIT: u64 = 128 * 1024 * 1024;

/// Strictly recognize only archives produced by this writer.
pub fn archive_key(base: &str, name: &str) -> Option<(u64, u16)> {
    let prefix = format!("{base}.day-");
    let (day, part) = name.strip_prefix(&prefix)?.split_once('-')?;
    let (d, p) = (day.parse::<u64>().ok()?, part.parse::<u16>().ok()?);
    (d.to_string() == day && p.to_string() == part && p < 1024).then_some((d, p))
}

pub(crate) struct LogFile {
    path: PathBuf,
    file: File,
    day: u64,
    written: u64,
    pruned: Option<(u64, u8)>,
    directory_dirty: bool,
    identity: (u64,u64),
    inode: Arc<AtomicU64>,
    rotating: Arc<AtomicBool>,
}

impl LogFile {
    pub(crate) fn new(path: PathBuf, file: File, day: u64) -> io::Result<Self> {
        let metadata = file.metadata()?;
        let written = metadata.len();
        let day = if written > 0 {
            metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|time| time.as_secs() / 86_400)
                .unwrap_or(day)
        } else {
            day
        };
        Ok(Self {
            path,
            file,
            day,
            written,
            pruned: None,
            directory_dirty: true,
            identity: (metadata.dev(),metadata.ino()),
            inode: Arc::new(AtomicU64::new(metadata.ino())),
            rotating: Arc::new(AtomicBool::new(false)),
        })
    }

    pub(crate) fn identity(&self) -> (u64,u64) { self.identity }
    pub(crate) fn identity_state(&self) -> (Arc<AtomicU64>, Arc<AtomicBool>) { (self.inode.clone(),self.rotating.clone()) }

    pub(crate) fn sync(&mut self) -> io::Result<()> {
        self.file.sync_data()?;
        if self.directory_dirty {
            if let Some(dir) = self.path.parent() { File::open(dir)?.sync_all()?; }
            self.directory_dirty = false;
        }
        Ok(())
    }

    fn archive(&mut self) -> io::Result<()> {
        struct Rotation(Arc<AtomicBool>);
        impl Drop for Rotation { fn drop(&mut self) {self.0.store(false,Ordering::Release);} }
        self.rotating.store(true,Ordering::Release);
        let _rotation=Rotation(self.rotating.clone());
        self.sync()?;
        let name = self
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid log filename"))?;
        // Reserve our destination so neither a restart nor two size rotations overwrite history.
        for part in 0..1024 {
            let archive = self
                .path
                .with_file_name(format!("{name}.day-{}-{part}", self.day));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&archive)
            {
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
                Ok(reservation) => {
                    drop(reservation);
                    if let Err(e) = fs::rename(&self.path, &archive) {
                        let _ = fs::remove_file(&archive);
                        return Err(e);
                    }
                    self.file = OpenOptions::new()
                        .create_new(true)
                        .append(true)
                        .mode(0o600)
                        .open(&self.path)?;
                    let metadata = self.file.metadata()?;
                    self.identity = (metadata.dev(),metadata.ino());
                    self.inode.store(metadata.ino(),Ordering::Release);
                    self.written = 0;
                    self.directory_dirty = true;
                    return Ok(());
                }
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "too many log archives for one day",
        ))
    }

    fn prune(&mut self, day: u64, keep: u8) -> io::Result<()> {
        let dir = self
            .path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing log directory"))?;
        let base = self.path.file_name().unwrap().to_string_lossy().into_owned();
        let mut retained = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() { continue; }
            let name = entry.file_name();
            let Some((parsed_day, parsed_part)) = name.to_str().and_then(|n| archive_key(&base, n)) else { continue; };
            if keep > 0 && day.saturating_sub(parsed_day) >= u64::from(keep) {
                fs::remove_file(entry.path())?;
                self.directory_dirty = true;
            } else {
                retained.push(((parsed_day, parsed_part), entry.metadata()?.len(), entry.path()));
            }
        }
        retained.sort_by_key(|(order, _, _)| *order);
        let mut bytes: u64 = retained.iter().map(|(_, n, _)| *n).sum();
        for (_, n, path) in retained {
            if bytes <= ARCHIVE_LIMIT { break; }
            fs::remove_file(path)?;
            self.directory_dirty = true;
            bytes = bytes.saturating_sub(n);
        }
        Ok(())
    }

    pub(crate) fn write(&mut self, line: &str, day: u64, keep: u8) -> io::Result<()> {
        let rotate = self.written > 0
            && ((keep > 0 && day != self.day)
                || self.written.saturating_add(line.len() as u64) > FILE_LIMIT);
        if rotate {
            self.archive()?;
        }
        self.day = day;
        self.file.write_all(line.as_bytes())?;
        self.written += line.len() as u64;
        if rotate || self.pruned != Some((day, keep)) {
            self.pruned = Some((day, keep));
            if let Err(e) = self.prune(day, keep) {
                // Stop this disk writer visibly instead of exceeding the retention budget.
                // The bounded in-memory ring continues recording diagnostic evidence.
                return Err(e);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn archive_budget_prunes_only_the_oldest_owned_files() {
        let dir = Directory::new();
        for name in ["cowplay.log.day-99-0", "cowplay.log.day-100-0", "cowplay.log.day-100-1"] {
            File::create(dir.0.join(name)).unwrap().set_len(80*1024*1024).unwrap();
        }
        fs::write(dir.0.join("other.log"), "user data").unwrap();
        let mut log = dir.writer(100);
        log.write("current\n", 100, 7).unwrap();
        assert!(!dir.0.join("cowplay.log.day-99-0").exists());
        assert!(!dir.0.join("cowplay.log.day-100-0").exists());
        assert!(dir.0.join("cowplay.log.day-100-1").exists());
        assert_eq!(dir.read("other.log"), "user data");
    }
    use std::sync::atomic::{AtomicU64, Ordering};
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "cowplay-retention-{}-{}-{}",
                std::process::id(),
                crate::diag::now_ms(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn writer(&self, day: u64) -> LogFile {
            let path = self.0.join("cowplay.log");
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(&path)
                .unwrap();
            LogFile::new(path, file, day).unwrap()
        }
        fn read(&self, name: &str) -> String {
            fs::read_to_string(self.0.join(name)).unwrap()
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn daily_rotation_keeps_days_and_does_not_overwrite_an_existing_archive() {
        let dir = Directory::new();
        fs::write(dir.0.join("cowplay.log.day-100-0"), "earlier process\n").unwrap();
        let mut log = dir.writer(100);
        log.write("first day\n", 100, 7).unwrap();
        log.write("second day\n", 101, 7).unwrap();
        assert_eq!(dir.read("cowplay.log.day-100-0"), "earlier process\n");
        assert_eq!(dir.read("cowplay.log.day-100-1"), "first day\n");
        assert_eq!(dir.read("cowplay.log"), "second day\n");
    }

    #[test]
    fn retention_boundary_preserves_other_files_directories_symlinks_and_future_dates() {
        let dir = Directory::new();
        for (name, text) in [
            ("cowplay.log.day-93-0", "expired"),
            ("cowplay.log.day-94-0", "within seven days"),
            ("cowplay.log.day-101-0", "future clock"),
            ("cowplay.log.1", "legacy log"),
            ("notes.txt", "user data"),
            ("cowplay.log.day-1-0.extra", "unrelated suffix"),
            ("cowplay.log.day-01-0", "noncanonical day"),
            ("cowplay.log.day-1-9999", "outside writer namespace"),
        ] {
            fs::write(dir.0.join(name), text).unwrap();
        }
        fs::create_dir(dir.0.join("cowplay.log.day-1-9")).unwrap();
        std::os::unix::fs::symlink(dir.0.join("notes.txt"), dir.0.join("cowplay.log.day-1-8"))
            .unwrap();
        let mut log = dir.writer(100);
        log.write("current\n", 100, 7).unwrap();
        assert!(!dir.0.join("cowplay.log.day-93-0").exists());
        assert_eq!(dir.read("cowplay.log.day-94-0"), "within seven days");
        assert_eq!(dir.read("cowplay.log.day-101-0"), "future clock");
        assert_eq!(dir.read("cowplay.log.1"), "legacy log");
        assert_eq!(dir.read("notes.txt"), "user data");
        assert_eq!(dir.read("cowplay.log.day-1-0.extra"), "unrelated suffix");
        assert_eq!(dir.read("cowplay.log.day-01-0"), "noncanonical day");
        assert_eq!(
            dir.read("cowplay.log.day-1-9999"),
            "outside writer namespace"
        );
        assert!(dir.0.join("cowplay.log.day-1-9").is_dir());
        assert!(dir
            .0
            .join("cowplay.log.day-1-8")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
        log.write("shorter retention\n", 100, 1).unwrap();
        assert!(
            !dir.0.join("cowplay.log.day-94-0").exists(),
            "changing retention applies without waiting another day"
        );
        assert!(dir.read("cowplay.log").contains("shorter retention"));
    }

    #[test]
    fn size_rotation_preserves_all_chunks_and_private_permissions() {
        let dir = Directory::new();
        let mut log = dir.writer(100);
        log.file.set_len(FILE_LIMIT).unwrap();
        log.written = FILE_LIMIT;
        log.write("new chunk\n", 100, 7).unwrap();
        log.file.set_len(FILE_LIMIT).unwrap();
        log.written = FILE_LIMIT;
        log.write("latest chunk\n", 100, 7).unwrap();
        assert_eq!(
            fs::metadata(dir.0.join("cowplay.log.day-100-0"))
                .unwrap()
                .len(),
            FILE_LIMIT
        );
        assert_eq!(
            fs::metadata(dir.0.join("cowplay.log.day-100-1"))
                .unwrap()
                .len(),
            FILE_LIMIT
        );
        assert_eq!(dir.read("cowplay.log"), "latest chunk\n");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(dir.0.join("cowplay.log"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn restarting_after_a_day_boundary_expires_old_content_instead_of_relabeling_it() {
        let dir = Directory::new();
        let mut before = dir.writer(93);
        before.write("before restart\n", 93, 7).unwrap();
        before
            .file
            .set_times(
                std::fs::FileTimes::new().set_modified(
                    std::time::UNIX_EPOCH + std::time::Duration::from_secs(93 * 86_400),
                ),
            )
            .unwrap();
        drop(before);
        let mut after = dir.writer(100);
        after.write("after restart\n", 100, 7).unwrap();
        assert_eq!(dir.read("cowplay.log"), "after restart\n");
        assert!(
            !dir.0.join("cowplay.log.day-93-0").exists(),
            "seven-day-old archive expires after restarting"
        );
    }
}
