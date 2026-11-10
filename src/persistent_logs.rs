//! Bounded export from the configured log directory, never an arbitrary path supplied by a viewer.
use serde_json::{Value, json};
use std::{fs, io::{self, Read, Seek, SeekFrom}, path::Path, time::{Duration, UNIX_EPOCH}};

const EXPORT_BYTES: usize = 256 * 1024;
static EXPORTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

fn mount_path(raw: &str) -> String {
    raw.replace("\\040", " ").replace("\\011", "\t").replace("\\012", "\n").replace("\\134", "\\")
}

pub(crate) fn mounted_storage(table: &str, mount: &Path) -> bool {
    table.lines().any(|line| {
        let Some((before, after)) = line.split_once(" - ") else { return false; };
        let fields: Vec<_> = before.split_whitespace().collect();
        let fs: Vec<_> = after.split_whitespace().collect();
        fields.len() >= 6 && fs.len() >= 3
            && Path::new(&mount_path(fields[4])) == mount
            && fields[5].split(',').any(|o| o == "rw")
            && fs[2].split(',').any(|o| o == "rw")
            && matches!(fs[0], "ext2"|"ext3"|"ext4"|"f2fs"|"btrfs"|"xfs"|"vfat"|"exfat"|"ntfs3")
    })
}

pub(crate) fn open_log(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let file = fs::OpenOptions::new().create(true).append(true).mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path)?;
    if !file.metadata()?.is_file() { return Err(io::Error::other("日志目标不是普通文件")); }
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

fn read_tail(dir: &Path, budget: usize) -> io::Result<Value> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut files = Vec::new();
    let mut truncated = false;
    let mut rejected = 0usize;
    for (n, entry) in fs::read_dir(dir)?.enumerate() {
        if n >= 4096 { truncated = true; break; }
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let order = if name == "cowplay.log" { (u64::MAX, u16::MAX) }
            else if let Some(key) = f50_server::log_archive_key("cowplay.log", &name) { key }
            else { continue; };
        if !entry.file_type()?.is_file() { rejected += 1; truncated = true; continue; }
        files.push((order, name, entry.path()));
    }
    files.sort_by_key(|(order, _, _)| *order);
    let mut remaining = budget;
    let mut chunks = Vec::new();
    let mut metadata = Vec::new();
    for (_, name, path) in files.into_iter().rev() {
        if remaining == 0 { truncated = true; break; }
        // Reject symlinks and non-files after opening too, including a racing replacement.
        let mut file = fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(&path)?;
        let info = file.metadata()?;
        if !info.is_file() { return Err(io::Error::other("日志路径不是普通文件")); }
        let n = info.len().min(remaining as u64) as usize;
        truncated |= info.len() > n as u64;
        file.seek(SeekFrom::Start(info.len().saturating_sub(n as u64)))?;
        let mut bytes = Vec::with_capacity(n);
        file.take(n as u64).read_to_end(&mut bytes)?;
        let mut from = 0;
        while from < bytes.len() && bytes[from] & 0xc0 == 0x80 { from += 1; }
        chunks.push(String::from_utf8_lossy(&bytes[from..]).into_owned());
        metadata.push(json!({"name":name,"stored_bytes":info.len(),"included_bytes":bytes.len()-from,
            "modified_ms":info.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64)}));
        remaining -= bytes.len();
    }
    chunks.reverse(); metadata.reverse();
    Ok(json!({"enabled":true,"captured_ms":f50_server::diag::now_ms(),"limit_bytes":budget,
        "truncated":truncated,"rejected_nonregular":rejected,"files":metadata,"tail":f50_server::diag::mask_macs(&chunks.join(""))}))
}

pub(crate) async fn export(directory: String) -> Value {
    let Ok(permit) = EXPORTS.try_acquire() else {
        return json!({"enabled":true,"error":"持久日志读取繁忙，请稍后重试","tail":""});
    };
    let (done, result) = tokio::sync::oneshot::channel();
    let thread = std::thread::Builder::new().name("log-export".into()).spawn(move || {
        let _permit = permit;
        let value = crate::admin::external_log_dir(&directory, false)
            .map_err(io::Error::other).and_then(|path| read_tail(path.parent().unwrap(), EXPORT_BYTES));
        let _ = done.send(value);
    });
    if let Err(error) = thread { return json!({"enabled":true,"error":error.to_string(),"tail":""}); }
    match tokio::time::timeout(Duration::from_secs(2), result).await {
        Ok(Ok(Ok(value))) => value,
        Ok(Ok(Err(error))) => json!({"enabled":true,"error":f50_server::diag::mask_macs(&error.to_string()),"tail":""}),
        _ => json!({"enabled":true,"error":"持久日志读取未在两秒内完成；内存日志仍可下载","tail":""}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn writer_refuses_symlink_and_fifo_without_touching_target_or_waiting() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("private.key");
        fs::write(&target, "SYNTHETIC_SECRET").unwrap();
        let path = dir.path().join("cowplay.log");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(open_log(&path).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "SYNTHETIC_SECRET");
        fs::remove_file(&path).unwrap();
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(open_log(&path).is_err(), "nonblocking open must reject FIFO without a reader");
    }
    #[test]
    fn exact_rw_persistent_mount_accepts_same_device_and_decodes_paths() {
        let table = "30 1 8:1 / / rw - ext4 /dev/mmcblk1 rw\n31 30 8:1 / /mnt/mu300-disk rw - ext4 /dev/mmcblk1 rw\n32 30 8:2 / /mnt/test\\040disk rw - ext4 /dev/sda1 rw\n";
        assert!(mounted_storage(table, Path::new("/mnt/mu300-disk")));
        assert!(mounted_storage(table, Path::new("/mnt/test disk")));
        for bad in ["/mnt/mu300-disk/sub", "/mnt/empty", "/mnt/mu300"] { assert!(!mounted_storage(table, Path::new(bad))); }
        for fs in ["tmpfs", "ramfs", "overlay", "squashfs"] {
            assert!(!mounted_storage(&table.replace("ext4", fs), Path::new("/mnt/mu300-disk")));
        }
        assert!(!mounted_storage(&table.replace("rw -", "ro -"), Path::new("/mnt/mu300-disk")));
        assert!(!mounted_storage(&table.replace("mmcblk1 rw", "mmcblk1 ro"), Path::new("/mnt/mu300-disk")));
    }
    #[test]
    fn disk_tail_survives_reopening_is_bounded_and_ignores_other_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("cowplay.log.day-100-0"), "previous-process\n").unwrap();
        fs::write(dir.path().join("cowplay.log"), "中文\nsynthetic aa:bb:cc:dd:ee:ff\n").unwrap();
        fs::write(dir.path().join("private.key"), "SYNTHETIC_SECRET").unwrap();
        std::os::unix::fs::symlink(dir.path().join("private.key"), dir.path().join("cowplay.log.day-101-0")).unwrap();
        let first = read_tail(dir.path(), 512).unwrap();
        assert!(first["tail"].as_str().unwrap().contains("previous-process"));
        assert!(!first.to_string().contains("SYNTHETIC_SECRET"));
        assert_eq!(first["rejected_nonregular"], 1);
        assert!(first["tail"].as_str().unwrap().contains("aa:bb:cc:••:••:ff"));
        assert_eq!(read_tail(dir.path(), 512).unwrap()["tail"], first["tail"]);
        let cut = read_tail(dir.path(), 31).unwrap();
        assert_eq!(cut["truncated"], true);
        assert!(!cut["tail"].as_str().unwrap().starts_with('�'));
        assert_eq!(cut["files"].as_array().unwrap().len(), 1);
    }
}
