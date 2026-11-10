//! Explicit, recoverable HTTPS listener takeover. No command lines or keys are exposed.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};

type Result<T> = std::result::Result<T, String>;

#[derive(Clone)]
pub(crate) struct Paths {
    pub proc: PathBuf,
    pub uhttpd: PathBuf,
    pub cowplay: PathBuf,
    pub config: PathBuf,
    pub init: PathBuf,
    pub jobs: PathBuf,
}
impl Paths {
    pub fn live() -> Self {
        Self {
            proc: "/proc".into(),
            uhttpd: "/etc/config/uhttpd".into(),
            cowplay: "/etc/config/cowplay".into(),
            config: "/etc/cowplay/state/config.json".into(),
            init: "/etc/init.d".into(),
            jobs: "/var/run/cowplay-network".into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Owner {
    pub pid: Option<u32>,
    pub process: String,
    pub service: Option<String>,
    pub addresses: Vec<SocketAddr>,
    pub(crate) start: u64,
    pub(crate) inodes: Vec<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Check {
    pub ok: bool,
    pub port: u16,
    pub bind: SocketAddr,
    pub running_port: Option<u16>,
    pub available: bool,
    pub owners: Vec<Owner>,
    pub can_takeover: bool,
    pub management_port: Option<u16>,
    pub fingerprint: String,
    pub issue: Option<String>,
}

fn valid_service(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}
fn overlaps(a: SocketAddr, b: SocketAddr) -> bool {
    a.port() == b.port()
        && a.is_ipv4() == b.is_ipv4()
        && (a.ip() == b.ip() || a.ip().is_unspecified() || b.ip().is_unspecified())
}
fn proc_address(s: &str) -> Option<SocketAddr> {
    let (address, port) = s.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let ip = if address.len() == 8 {
        IpAddr::from(u32::from_str_radix(address, 16).ok()?.to_ne_bytes())
    } else if address.len() == 32 {
        let mut out = [0u8; 16];
        for i in 0..4 {
            out[i * 4..i * 4 + 4].copy_from_slice(
                &u32::from_str_radix(&address[i * 8..i * 8 + 8], 16)
                    .ok()?
                    .to_ne_bytes(),
            );
        }
        IpAddr::from(out)
    } else {
        return None;
    };
    Some(SocketAddr::new(ip, port))
}
fn proc_listeners(text: &str, target: SocketAddr) -> Vec<(u64, SocketAddr)> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let columns: Vec<_> = line.split_whitespace().collect();
            if columns.get(3) != Some(&"0A") {
                return None;
            }
            let addr = proc_address(columns.get(1)?)?;
            overlaps(addr, target).then(|| Some((columns.get(9)?.parse().ok()?, addr)))?
        })
        .collect()
}
fn process_start(text: &str) -> Option<u64> {
    text.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}
fn owners(paths: &Paths, target: SocketAddr) -> Vec<Owner> {
    let mut sockets = BTreeMap::new();
    for file in ["net/tcp", "net/tcp6"] {
        if let Ok(text) = std::fs::read_to_string(paths.proc.join(file)) {
            sockets.extend(proc_listeners(&text, target));
        }
    }
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&paths.proc) {
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
                continue;
            };
            let mut inodes = BTreeSet::new();
            for fd in fds.flatten() {
                let Ok(link) = std::fs::read_link(fd.path()) else {
                    continue;
                };
                let Some(inode) = link
                    .to_str()
                    .and_then(|s| s.strip_prefix("socket:["))
                    .and_then(|s| s.strip_suffix(']'))
                    .and_then(|s| s.parse::<u64>().ok())
                else {
                    continue;
                };
                if sockets.contains_key(&inode) {
                    inodes.insert(inode);
                }
            }
            if inodes.is_empty() {
                continue;
            }
            let process = std::fs::read_to_string(entry.path().join("comm"))
                .unwrap_or_else(|_| "未知".into())
                .trim()
                .to_owned();
            let start = std::fs::read_to_string(entry.path().join("stat"))
                .ok()
                .and_then(|t| process_start(&t))
                .unwrap_or(0);
            found.push(Owner {
                pid: Some(pid),
                process,
                service: None,
                addresses: inodes
                    .iter()
                    .filter_map(|i| sockets.get(i).copied())
                    .collect(),
                start,
                inodes: inodes.into_iter().collect(),
            });
        }
    }
    let known: BTreeSet<_> = found
        .iter()
        .flat_map(|o| o.inodes.iter().copied())
        .collect();
    for (inode, address) in sockets {
        if !known.contains(&inode) {
            found.push(Owner {
                pid: None,
                process: "未知".into(),
                service: None,
                addresses: vec![address],
                start: 0,
                inodes: vec![inode],
            });
        }
    }
    found.sort_by_key(|o| (o.pid, o.addresses.clone()));
    found
}

pub(crate) async fn command(program: &Path, args: &[String], seconds: u64) -> Result<String> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(seconds), command.output())
        .await
        .map_err(|_| "系统操作超时".to_owned())?
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!("系统操作失败（{}）", output.status));
    }
    if output.stdout.len() > 1024 * 1024 {
        return Err("系统返回数据过大".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
async fn service_for(paths: &Paths, owner: &Owner) -> Option<String> {
    if !valid_service(&owner.process)
        || owner.start == 0
        || !paths.init.join(&owner.process).is_file()
    {
        return None;
    }
    let pid = owner.pid?;
    procd_pids(&owner.process)
        .await
        .ok()?
        .contains(&pid)
        .then(|| owner.process.clone())
}
async fn procd_pids(service: &str) -> Result<BTreeSet<u32>> {
    if !valid_service(service) {
        return Err("服务名无效".into());
    }
    let data = command(
        Path::new("/bin/ubus"),
        &[
            "call".into(),
            "service".into(),
            "list".into(),
            serde_json::json!({"name":service}).to_string(),
        ],
        2,
    )
    .await?;
    let data: serde_json::Value = serde_json::from_str(&data).map_err(|_| "服务状态无效")?;
    Ok(data
        .get(service)
        .and_then(|v| v.get("instances"))
        .and_then(|v| v.as_object())
        .into_iter()
        .flat_map(|instances| instances.values())
        .filter_map(|i| {
            i["pid"]
                .as_u64()
                .and_then(|p| u32::try_from(p).ok())
                .filter(|p| *p > 0)
        })
        .collect())
}
fn move_https(data: &[u8], target: SocketAddr, port: u16, keep_old: bool) -> Result<Vec<u8>> {
    let mut pkg =
        crate::uci::Package::parse(std::str::from_utf8(data).map_err(|_| "uhttpd 配置编码无效")?)?;
    let mut changed = false;
    for section in &mut pkg.sections {
        if section.kind != "uhttpd" {
            continue;
        }
        for (key, value) in &mut section.options {
            if key != "listen_https" {
                continue;
            }
            let values = match value {
                crate::uci::Value::Single(v) => vec![v.clone()],
                crate::uci::Value::List(v) => v.clone(),
            };
            let mut next = Vec::new();
            for value in values {
                let address: SocketAddr =
                    value.parse().map_err(|_| "uhttpd HTTPS 监听格式无法识别")?;
                if overlaps(address, target) {
                    changed = true;
                    if keep_old {
                        next.push(value);
                    }
                    let mut alternate = address;
                    alternate.set_port(port);
                    next.push(alternate.to_string());
                } else {
                    next.push(value);
                }
            }
            next.sort();
            next.dedup();
            *value = crate::uci::Value::List(next);
        }
    }
    if !changed {
        return Err("未找到与占用者对应的 uhttpd HTTPS 监听，不能猜测覆盖".into());
    }
    Ok(pkg.serialize().into_bytes())
}
fn alternative(paths: &Paths, target: SocketAddr, data: &[u8]) -> Option<u16> {
    for port in [8443, 9443, 10443] {
        if port == target.port() {
            continue;
        }
        let staged = move_https(data, target, port, true).ok()?;
        let pkg = crate::uci::Package::parse(std::str::from_utf8(&staged).ok()?).ok()?;
        let mut listeners = Vec::new();
        let mut available = true;
        for section in pkg.sections {
            for (key, value) in section.options {
                if key != "listen_https" {
                    continue;
                }
                let values = match value {
                    crate::uci::Value::Single(v) => vec![v],
                    crate::uci::Value::List(v) => v,
                };
                for value in values {
                    let Ok(address) = value.parse::<SocketAddr>() else {
                        return None;
                    };
                    if address.port() != port {
                        continue;
                    }
                    // Probe separately: wildcard IPv6 can also reserve IPv4 on Linux.
                    match std::net::TcpListener::bind(address) {
                        Ok(listener) => {
                            drop(listener);
                            listeners.push(address);
                        }
                        Err(_) => {
                            available = false;
                        }
                    }
                }
            }
        }
        if available
            && !listeners.is_empty()
            && owners(paths, SocketAddr::new(target.ip(), port)).is_empty()
        {
            return Some(port);
        }
    }
    None
}
fn fingerprint(check: &Check, paths: &Paths) -> String {
    let mut hash = Sha256::new();
    hash.update(
        serde_json::to_vec(&(
            check.bind,
            check.running_port,
            &check.owners,
            check.management_port,
        ))
        .unwrap(),
    );
    for path in [&paths.uhttpd, &paths.cowplay] {
        if let Ok(bytes) = std::fs::read(path) {
            hash.update(Sha256::digest(bytes));
        }
    }
    format!("{:x}", hash.finalize())
}
pub(crate) async fn inspect(
    paths: &Paths,
    bind: SocketAddr,
    running_port: Option<u16>,
    port: u16,
) -> Result<Check> {
    let target = SocketAddr::new(bind.ip(), port);
    let (available, issue) = if running_port == Some(port) {
        (true, None)
    } else {
        match std::net::TcpListener::bind(target) {
            Ok(listener) => {
                drop(listener);
                (true, None)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => (false, None),
            Err(e) => (false, Some(format!("无法检查端口：{e}"))),
        }
    };
    let mut found = if available {
        Vec::new()
    } else {
        owners(paths, target)
    };
    for owner in &mut found {
        owner.service = service_for(paths, owner).await;
    }
    let has_uhttpd = found.iter().any(|o| o.service.as_deref() == Some("uhttpd"));
    let management_port = if has_uhttpd {
        std::fs::read(&paths.uhttpd)
            .ok()
            .and_then(|data| alternative(paths, target, &data))
    } else {
        None
    };
    let protected = [
        "cowplay", "procd", "ubus", "rpcd", "network", "firewall", "dnsmasq", "dropbear",
    ];
    let can_takeover = !available
        && issue.is_none()
        && !found.is_empty()
        && found.iter().all(|o| {
            o.service
                .as_deref()
                .is_some_and(|s| !protected.contains(&s))
        })
        && (!has_uhttpd || management_port.is_some());
    let mut check = Check {
        ok: true,
        port,
        bind: target,
        running_port,
        available,
        owners: found,
        can_takeover,
        management_port,
        fingerprint: String::new(),
        issue,
    };
    check.fingerprint = fingerprint(&check, paths);
    Ok(check)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Job {
    pub id: String,
    pub phase: String,
    pub message: String,
    pub check: Check,
    pub management_scheme: String,
    pub management_port: u16,
    pub original_management_port: u16,
    pub confirmed: bool,
    pub created: u64,
    pub pid: u32,
    pub original_cowplay: Vec<u8>,
    pub saved_cowplay: Vec<u8>,
    pub applied_cowplay: Option<Vec<u8>>,
    pub original_uhttpd: Option<Vec<u8>>,
    pub written_uhttpd: Option<Vec<u8>>,
    pub stopped: Vec<String>,
    pub cowplay_touched: bool,
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn job_path(paths: &Paths, id: &str) -> Result<PathBuf> {
    if id.len() < 6
        || id.len() > 64
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err("接管任务编号无效".into());
    }
    Ok(paths.jobs.join(id).join("job.json"))
}
fn read_job(paths: &Paths, id: &str) -> Result<Job> {
    serde_json::from_slice(&std::fs::read(job_path(paths, id)?).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
fn write_job(paths: &Paths, job: &Job) -> Result<()> {
    f50_server::settings::write_private(
        &job_path(paths, &job.id)?,
        &serde_json::to_vec(job).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
fn write_confirmation(paths: &Paths, job: &Job) -> Result<()> {
    f50_server::settings::write_private(
        &job_path(paths, &job.id)?.with_file_name("confirmed"),
        b"confirmed",
    )
    .map_err(|e| e.to_string())
}
fn restore_prepared_port(paths: &Paths, job: &Job) -> Result<()> {
    let current = std::fs::read(&paths.cowplay).map_err(|e| e.to_string())?;
    if current == job.original_cowplay {
        return Ok(());
    }
    let mut pkg =
        crate::uci::Package::parse(std::str::from_utf8(&current).map_err(|_| "配置编码无效")?)?;
    let original = crate::uci::Package::parse(
        std::str::from_utf8(&job.original_cowplay).map_err(|_| "恢复配置编码无效")?,
    )?;
    let port = original.get("main", "port");
    if pkg.get("main", "port") == port {
        return Ok(());
    }
    if pkg.get("main", "port").and_then(|v| v.parse::<u16>().ok()) != Some(job.check.port) {
        return Err("端口配置已由其他操作修改，未覆盖".into());
    }
    if let Some(port) = port {
        pkg.set("cowplay", "main", "port", port);
    } else if let Some(main) = pkg
        .sections
        .iter_mut()
        .find(|s| s.name.as_deref() == Some("main"))
    {
        main.options.retain(|(key, _)| key != "port");
    }
    replace_if(&paths.cowplay, &current, pkg.serialize().as_bytes())
}
fn public_job(job: &Job) -> serde_json::Value {
    serde_json::json!({"id":job.id,"phase":job.phase,"message":job.message,"port":job.check.port,"management_scheme":job.management_scheme,"management_port":job.management_port,"original_management_port":job.original_management_port,"owners":job.check.owners,"can_takeover":job.check.can_takeover})
}
fn replace_if(path: &Path, expected: &[u8], next: &[u8]) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if std::fs::read(path).map_err(|e| e.to_string())? != expected {
        return Err("配置已被其他操作修改，未覆盖其更改".into());
    }
    let mode = std::fs::metadata(path)
        .map_err(|e| e.to_string())?
        .permissions()
        .mode()
        & 0o777;
    f50_server::settings::replace(path, next, mode).map_err(|e| e.to_string())
}

// The independent rpcd CLI is also available while the receiver is stopped.
async fn live_check(paths: &Paths, port: u16) -> Result<Check> {
    if let Ok(value) = crate::ctl::request(
        "network_context",
        &serde_json::json!({}),
        Path::new(crate::ctl::SOCKET),
    ) {
        if let Some(bind) = value["bind"]
            .as_str()
            .and_then(|s| s.parse::<SocketAddr>().ok())
        {
            return inspect(paths, bind, Some(bind.port()), port).await;
        }
    }
    let bytes = f50_server::settings::read(paths.config.to_str().ok_or("配置路径无效")?, 32768)
        .map_err(|_| "无法读取 CowPlay 监听地址")?;
    let config: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "CowPlay 配置无效")?;
    let bind = config["receiver"]["web"]["bind"]
        .as_str()
        .ok_or("CowPlay 配置缺少监听地址")?
        .parse()
        .map_err(|_| "CowPlay 监听地址无效")?;
    let stopped = procd_pids("cowplay")
        .await
        .is_ok_and(|pids| pids.is_empty());
    let mut report = inspect(paths, bind, None, port).await?;
    if !stopped {
        report.can_takeover = false;
        report.issue = Some("无法确认 CowPlay 原运行状态，暂不执行服务接管".into());
    }
    Ok(report)
}

pub async fn cli(action: &str, args: &serde_json::Value) -> Result<serde_json::Value> {
    if !Path::new("/etc/openwrt_release").is_file() { return Err("network migration requires OpenWrt".into()); }
    let paths = Paths::live();
    let port = || {
        args["port"]
            .as_u64()
            .filter(|p| (1..=65535).contains(p))
            .map(|p| p as u16)
            .ok_or_else(|| "HTTPS 端口应为 1–65535 的整数".to_owned())
    };
    match action {
        "check" => Ok(serde_json::to_value(live_check(&paths, port()?).await?).unwrap()),
        "prepare" => {
            use std::os::unix::fs::PermissionsExt;
            let check = live_check(&paths, port()?).await?;
            if check.fingerprint != args["fingerprint"].as_str().unwrap_or("") {
                return Err("占用情况已变化，请重新确认".into());
            }
            if !check.can_takeover {
                return Err("无法确认可恢复的接管方式；配置仍可保存".into());
            }
            let scheme = args["management_scheme"]
                .as_str()
                .filter(|s| ["http", "https"].contains(s))
                .ok_or("管理协议无效")?;
            let current = args["management_port"]
                .as_u64()
                .filter(|p| (1..=65535).contains(p))
                .ok_or("管理端口无效")? as u16;
            let management_port = if scheme == "https" && current == check.port {
                check.management_port.ok_or("未找到备用 HTTPS 管理端口")?
            } else {
                current
            };
            if check
                .owners
                .iter()
                .any(|o| o.service.as_deref() != Some("uhttpd"))
            {
                let management = inspect(
                    &paths,
                    SocketAddr::new(check.bind.ip(), current),
                    None,
                    current,
                )
                .await?;
                if management.owners.iter().any(|o| {
                    check
                        .owners
                        .iter()
                        .any(|target| target.service == o.service && target.service.is_some())
                }) {
                    return Err(
                        "占用服务同时承载当前管理入口，请从独立的 LuCI 入口执行接管；配置仍可保存"
                            .into(),
                    );
                }
            }
            std::fs::create_dir_all(&paths.jobs).map_err(|e| e.to_string())?;
            std::fs::set_permissions(&paths.jobs, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
            let directory = tempfile::Builder::new()
                .prefix("takeover-")
                .tempdir_in(&paths.jobs)
                .map_err(|e| e.to_string())?;
            let id = directory
                .path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let _ = directory.keep();
            let saved_cowplay = std::fs::read(&paths.cowplay).map_err(|e| e.to_string())?;
            let mut original_cowplay = saved_cowplay.clone();
            if let Some(running) = check.running_port {
                let mut pkg = crate::uci::Package::parse(
                    std::str::from_utf8(&saved_cowplay).map_err(|_| "配置编码无效")?,
                )?;
                if pkg.get("main", "port").and_then(|s| s.parse().ok()) != Some(running) {
                    pkg.set("cowplay", "main", "port", &running.to_string());
                    original_cowplay = pkg.serialize().into_bytes();
                }
            }
            let job = Job {
                id,
                phase: "prepared".into(),
                message: "等待应用配置".into(),
                check,
                management_scheme: scheme.into(),
                management_port,
                original_management_port: current,
                confirmed: false,
                created: now(),
                pid: 0,
                original_cowplay,
                saved_cowplay,
                applied_cowplay: None,
                original_uhttpd: std::fs::read(&paths.uhttpd).ok(),
                written_uhttpd: None,
                stopped: Vec::new(),
                cowplay_touched: false,
            };
            let _lock = job_lock(&paths)?;
            write_job(&paths, &job)?;
            reserve(&paths, &job)?;
            Ok(public_job(&job))
        }
        "status" => {
            let id = args["id"].as_str().unwrap_or("");
            let active = if id.is_empty() {
                std::fs::read_to_string(paths.jobs.join("active")).ok()
            } else {
                Some(id.to_owned())
            };
            match active {
                Some(id) => Ok(public_job(&read_job(&paths, id.trim())?)),
                None => Ok(serde_json::json!({"phase":"idle"})),
            }
        }
        "confirm" => {
            let job = read_job(&paths, args["id"].as_str().ok_or("缺少任务编号")?)?;
            if job.phase != "await_management" || now().saturating_sub(job.created) > 180 {
                return Err("任务不在管理入口确认阶段".into());
            }
            if args["management_scheme"].as_str() != Some(&job.management_scheme)
                || args["management_port"].as_u64() != Some(u64::from(job.management_port))
            {
                return Err("请从新的管理入口确认".into());
            }
            write_confirmation(&paths, &job)?;
            Ok(serde_json::json!({"ok":true}))
        }
        "commit" => {
            let mut job = read_job(&paths, args["id"].as_str().ok_or("缺少任务编号")?)?;
            if job.phase != "prepared" {
                return Ok(public_job(&job));
            }
            let lock = job_lock(&paths)?;
            job = read_job(&paths, &job.id)?;
            if job.phase != "prepared" {
                return Ok(public_job(&job));
            }
            if std::fs::read_to_string(paths.jobs.join("active"))
                .ok()
                .as_deref()
                != Some(&job.id)
            {
                return Err("接管任务已失效，请重新确认".into());
            }
            if now().saturating_sub(job.created) > 120 {
                return Err("接管确认已过期，请重新确认".into());
            }
            let current = std::fs::read(&paths.cowplay).map_err(|e| e.to_string())?;
            let pkg = crate::uci::Package::parse(
                std::str::from_utf8(&current).map_err(|_| "配置编码无效")?,
            )?;
            if pkg.get("main", "port").and_then(|p| p.parse::<u16>().ok()) != Some(job.check.port) {
                return Err("目标端口尚未应用".into());
            }
            job.applied_cowplay = Some(current);
            job.phase = "queued".into();
            job.message = "正在准备接管".into();
            write_job(&paths, &job)?;
            drop(lock);
            if let Err(e) = spawn_worker(&job.id, false) {
                job.phase = "failed".into();
                job.message = format!("无法启动接管任务：{e}");
                if let Some(applied) = &job.applied_cowplay {
                    replace_if(&paths.cowplay, applied, &job.original_cowplay)?;
                }
                write_job(&paths, &job)?;
                release(&paths, &job);
                return Err(job.message);
            }
            Ok(public_job(&job))
        }
        "cancel" => {
            let _lock = job_lock(&paths)?;
            let mut job = read_job(&paths, args["id"].as_str().ok_or("缺少任务编号")?)?;
            if job.phase != "prepared" {
                return Err("接管已开始，不能取消已执行的操作".into());
            }
            restore_prepared_port(&paths, &job)?;
            job.phase = "cancelled".into();
            job.message = "已取消接管".into();
            write_job(&paths, &job)?;
            release(&paths, &job);
            Ok(public_job(&job))
        }
        "recover" => {
            let lock = job_lock(&paths)?;
            let mut job = read_job(&paths, args["id"].as_str().ok_or("缺少任务编号")?)?;
            if ["complete", "cancelled", "rolled_back"].contains(&job.phase.as_str()) {
                return Ok(public_job(&job));
            }
            if job.phase == "prepared" {
                restore_prepared_port(&paths, &job)?;
                job.phase = "rolled_back".into();
                job.message = "已撤销未完成的端口配置，其他编辑保留".into();
                write_job(&paths, &job)?;
                release(&paths, &job);
                return Ok(public_job(&job));
            }
            if std::fs::read_to_string(paths.jobs.join("active")).is_ok_and(|id| id != job.id) {
                return Err("另一接管任务尚未完成".into());
            }
            job.phase = "recover_queued".into();
            job.message = "正在恢复原配置与服务".into();
            write_job(&paths, &job)?;
            drop(lock);
            spawn_worker(&job.id, true)?;
            Ok(public_job(&job))
        }
        _ => Err("未知网络操作".into()),
    }
}
fn spawn_worker(id: &str, recovery: bool) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let mut child = std::process::Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
    child
        .args([
            "network-worker",
            id,
            if recovery { "recover" } else { "apply" },
        ])
        .process_group(0)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    child.spawn().map(|_| ()).map_err(|e| e.to_string())
}

fn job_lock(paths: &Paths) -> Result<std::fs::File> {
    if paths.jobs == Path::new("/var/run/cowplay-network") && Path::new("/etc/cowplay/updates/active").exists() {
        return Err("软件更新任务尚未结束，请先完成或恢复".into());
    }
    use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(paths.jobs.join("lock"))
        .map_err(|e| e.to_string())?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("另一端口接管正在进行".into());
    }
    Ok(file)
}
fn reserve(paths: &Paths, job: &Job) -> Result<()> {
    let active = paths.jobs.join("active");
    if let Ok(id) = std::fs::read_to_string(&active) {
        let existing = read_job(paths, id.trim())?;
        if !["failed", "cancelled", "complete", "rolled_back"].contains(&existing.phase.as_str())
            && !(existing.phase == "prepared" && now().saturating_sub(existing.created) > 120)
        {
            return Err("已有端口接管任务，请先完成或恢复".into());
        }
    }
    f50_server::settings::write_private(&active, job.id.as_bytes()).map_err(|e| e.to_string())
}
fn release(paths: &Paths, job: &Job) {
    let active = paths.jobs.join("active");
    if std::fs::read_to_string(&active).ok().as_deref() == Some(&job.id) {
        let _ = std::fs::remove_file(active);
    }
}

trait Backend {
    async fn inspect(&self, paths: &Paths, job: &Job) -> Result<Check>;
    async fn service(&self, paths: &Paths, name: &str, action: &str) -> Result<()>;
    async fn management(&self, paths: &Paths, job: &Job) -> Result<Job>;
    async fn health(&self, paths: &Paths, job: &Job) -> Result<()>;
    async fn restored(&self, paths: &Paths, job: &Job) -> Result<()>;
}
struct Live;
impl Backend for Live {
    async fn inspect(&self, paths: &Paths, job: &Job) -> Result<Check> {
        inspect(
            paths,
            job.check.bind,
            job.check.running_port,
            job.check.port,
        )
        .await
    }
    async fn service(&self, paths: &Paths, name: &str, action: &str) -> Result<()> {
        if !valid_service(name) || !["start", "stop", "restart"].contains(&action) {
            return Err("服务操作无效".into());
        }
        command(&paths.init.join(name), &[action.into()], 20)
            .await
            .map(|_| ())
            .map_err(|_| format!("{name} {action} 失败或超时"))
    }
    async fn management(&self, paths: &Paths, job: &Job) -> Result<Job> {
        let until = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let current = read_job(paths, &job.id)?;
            if std::fs::read(job_path(paths, &job.id)?.with_file_name("confirmed"))
                .ok()
                .as_deref()
                == Some(b"confirmed")
            {
                let mut current = current;
                current.confirmed = true;
                return Ok(current);
            }
            if tokio::time::Instant::now() >= until {
                return Err("未确认新管理入口可用，保留原端口并恢复配置".into());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    async fn health(&self, paths: &Paths, job: &Job) -> Result<()> {
        let until = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            if owned_receiver(paths, job.check.bind).await
                && verify_https(&paths.config, &paths.cowplay, job.check.bind)
                    .await
                    .is_ok()
            {
                return Ok(());
            }
            if tokio::time::Instant::now() >= until {
                return Err("CowPlay 未在目标端口通过可信 HTTPS 验证".into());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    async fn restored(&self, paths: &Paths, job: &Job) -> Result<()> {
        let until = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let current = inspect(paths, job.check.bind, None, job.check.port).await?;
            let services_ok = job
                .check
                .owners
                .iter()
                .filter_map(|o| o.service.as_ref())
                .all(|service| {
                    current
                        .owners
                        .iter()
                        .any(|o| o.service.as_ref() == Some(service))
                });
            let cowplay_ok = if let Some(port) = job.check.running_port {
                let target = SocketAddr::new(job.check.bind.ip(), port);
                owned_receiver(paths, target).await
                    && verify_https(&paths.config, &paths.cowplay, target)
                        .await
                        .is_ok()
            } else {
                crate::ctl::request(
                    "network_context",
                    &serde_json::json!({}),
                    Path::new(crate::ctl::SOCKET),
                )
                .is_err()
            };
            if services_ok && cowplay_ok {
                return Ok(());
            }
            if tokio::time::Instant::now() >= until {
                return Err("原监听或 CowPlay HTTPS 尚未恢复，未报告恢复成功".into());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}
async fn owned_receiver(paths: &Paths, target: SocketAddr) -> bool {
    let Ok(context) = crate::ctl::request(
        "network_context",
        &serde_json::json!({}),
        Path::new(crate::ctl::SOCKET),
    ) else {
        return false;
    };
    if context["bind"]
        .as_str()
        .and_then(|s| s.parse::<SocketAddr>().ok())
        != Some(target)
    {
        return false;
    }
    let Some(pid) = context["pid"].as_u64().and_then(|p| u32::try_from(p).ok()) else {
        return false;
    };
    let report = match inspect(paths, target, None, target.port()).await {
        Ok(report) => report,
        Err(_) => return false,
    };
    !report.owners.is_empty()
        && report
            .owners
            .iter()
            .all(|o| o.pid == Some(pid) && o.service.as_deref() == Some("cowplay"))
}

pub(crate) async fn verify_https(config: &Path, uci: &Path, target: SocketAddr) -> Result<()> {
    use rustls_pki_types::pem::PemObject;
    let bytes = f50_server::settings::read(config.to_str().ok_or("配置路径无效")?, 32768)
        .map_err(|_| "无法读取 HTTPS 配置")?;
    let config: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| "HTTPS 配置无效")?;
    let web = &config["receiver"]["web"];
    let mut origin = reqwest::Url::parse(web["origin"].as_str().ok_or("缺少 HTTPS 地址")?)
        .map_err(|_| "HTTPS 地址无效")?;
    if origin.scheme() != "https" {
        return Err("仅允许可信 HTTPS 验证".into());
    }
    let policy = crate::policy::Policy::from_uci(&crate::uci::Package::read(uci)?);
    if !policy.domain.is_empty() {
        origin
            .set_host(Some(&policy.domain))
            .map_err(|_| "域名无效")?;
    }
    origin
        .set_port(Some(target.port()))
        .map_err(|_| "HTTPS 端口无效")?;
    origin.set_path("/health");
    let host = origin.host_str().ok_or("缺少 HTTPS 域名")?;
    let address = SocketAddr::new(
        if target.ip().is_unspecified() {
            if target.is_ipv4() {
                IpAddr::from([127, 0, 0, 1])
            } else {
                IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
            }
        } else {
            target.ip()
        },
        target.port(),
    );
    let cert = f50_server::settings::read(
        web["tls_certificate"].as_str().ok_or("缺少证书路径")?,
        256 * 1024,
    )
    .map_err(|_| "无法读取 HTTPS 证书")?;
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .https_only(true)
        .timeout(Duration::from_secs(2))
        .pool_max_idle_per_host(0)
        .resolve(host, address);
    for certificate in rustls_pki_types::CertificateDer::pem_slice_iter(&cert) {
        builder = builder.add_root_certificate(
            reqwest::Certificate::from_der(&certificate.map_err(|_| "证书编码无效")?)
                .map_err(|_| "证书无效")?,
        );
    }
    let mut response = builder
        .build()
        .map_err(|_| "HTTPS 客户端初始化失败")?
        .get(origin)
        .send()
        .await
        .map_err(|_| "可信 HTTPS 握手失败")?;
    if !response.status().is_success() {
        return Err("HTTPS 健康检查未通过".into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "HTTPS 响应读取失败")? {
        if body.len() + chunk.len() > 256 {
            return Err("目标端口未返回 CowPlay 健康响应".into());
        }
        body.extend_from_slice(&chunk);
    }
    if body != b"bridge available; query authenticated websocket for session status" {
        return Err("目标端口未返回 CowPlay 健康响应".into());
    }
    Ok(())
}
fn same_owners(expected: &[Owner], actual: &[Owner], skip_uhttpd: bool) -> bool {
    expected
        .iter()
        .filter(|o| !skip_uhttpd || o.service.as_deref() != Some("uhttpd"))
        .eq(actual
            .iter()
            .filter(|o| !skip_uhttpd || o.service.as_deref() != Some("uhttpd")))
}
async fn execute<B: Backend>(paths: &Paths, job: &mut Job, backend: &B) -> Result<()> {
    let current = backend.inspect(paths, job).await?;
    if current.available || !same_owners(&job.check.owners, &current.owners, false) {
        return Err("占用者已变化，请重新确认覆盖".into());
    }
    if std::fs::read(&paths.cowplay).ok().as_ref() != job.applied_cowplay.as_ref() {
        return Err("CowPlay 配置已变化，未执行覆盖".into());
    }
    let has_uhttpd = job
        .check
        .owners
        .iter()
        .any(|o| o.service.as_deref() == Some("uhttpd"));
    if has_uhttpd {
        let original = job.original_uhttpd.as_ref().ok_or("缺少原管理配置")?;
        let staged = move_https(
            original,
            job.check.bind,
            job.check.management_port.ok_or("缺少备用管理端口")?,
            true,
        )?;
        // Write-ahead recovery information, before changing any listener.
        job.written_uhttpd = Some(staged.clone());
        job.phase = "preparing_management".into();
        job.message = "正在建立备用管理入口，原入口保留".into();
        write_job(paths, job)?;
        replace_if(&paths.uhttpd, original, &staged)?;
        backend.service(paths, "uhttpd", "restart").await?;
        job.phase = "await_management".into();
        job.message = "等待浏览器确认管理入口".into();
        write_job(paths, job)?;
        *job = backend.management(paths, job).await?;
        let current = backend.inspect(paths, job).await?;
        if !same_owners(&job.check.owners, &current.owners, true)
            || current.owners.iter().any(|o| o.service.is_none())
        {
            return Err("等待确认时占用者发生变化，未释放原端口".into());
        }
    }
    let services: BTreeSet<_> = job
        .check
        .owners
        .iter()
        .filter_map(|o| o.service.clone())
        .filter(|s| s != "uhttpd")
        .collect();
    for service in services {
        let current = backend.inspect(paths, job).await?;
        let expected: Vec<_> = job
            .check
            .owners
            .iter()
            .filter(|o| !job.stopped.iter().any(|s| o.service.as_ref() == Some(s)))
            .cloned()
            .collect();
        if !same_owners(&expected, &current.owners, has_uhttpd) {
            return Err("占用者已变化，未停止新的服务".into());
        }
        job.stopped.push(service.clone());
        job.phase = "releasing".into();
        job.message = format!("正在停止 {service} 并释放端口");
        write_job(paths, job)?;
        backend.service(paths, &service, "stop").await?;
    }
    if has_uhttpd {
        let final_config = move_https(
            job.original_uhttpd.as_ref().unwrap(),
            job.check.bind,
            job.check.management_port.unwrap(),
            false,
        )?;
        let staged = job.written_uhttpd.clone().unwrap();
        job.written_uhttpd = Some(final_config.clone());
        job.phase = "releasing".into();
        job.message = "管理入口已确认，正在释放原 HTTPS 端口".into();
        write_job(paths, job)?;
        replace_if(&paths.uhttpd, &staged, &final_config)?;
        backend.service(paths, "uhttpd", "restart").await?;
    }
    let until = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if backend.inspect(paths, job).await?.available {
            break;
        }
        if tokio::time::Instant::now() >= until {
            return Err("目标端口仍被占用，接管未完成".into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if std::fs::read(&paths.cowplay).ok().as_ref() != job.applied_cowplay.as_ref() {
        return Err("CowPlay 配置已被其他操作修改".into());
    }
    job.cowplay_touched = true;
    job.phase = "starting".into();
    job.message = "正在启动 CowPlay 并验证 HTTPS".into();
    write_job(paths, job)?;
    backend
        .service(
            paths,
            "cowplay",
            if job.check.running_port.is_some() {
                "restart"
            } else {
                "start"
            },
        )
        .await?;
    backend.health(paths, job).await?;
    job.phase = "complete".into();
    job.message = format!("CowPlay 已接管 {}，HTTPS 验证通过", job.check.port);
    write_job(paths, job)?;
    Ok(())
}
async fn recover<B: Backend>(paths: &Paths, job: &mut Job, backend: &B) -> Result<()> {
    if job.applied_cowplay.is_none() {
        return restore_prepared_port(paths, job);
    }
    let mut errors = Vec::new();
    let mut cowplay_safe = true;
    if let Some(applied) = &job.applied_cowplay {
        match std::fs::read(&paths.cowplay) {
            Err(e) => {
                errors.push(format!("无法读取 CowPlay 配置：{e}"));
                cowplay_safe = false;
            }
            Ok(current) => {
                if current != *applied && current != job.original_cowplay {
                    errors.push("CowPlay 配置已被其他操作修改，未覆盖".into());
                    cowplay_safe = false;
                }
                if cowplay_safe {
                    if job.cowplay_touched {
                        if let Err(e) = backend.service(paths, "cowplay", "stop").await {
                            errors.push(e);
                        }
                    }
                    if current == *applied {
                        if let Err(e) = replace_if(&paths.cowplay, applied, &job.original_cowplay) {
                            errors.push(e);
                            cowplay_safe = false;
                        }
                    }
                }
            }
        }
    }
    if let (Some(original), Some(written)) = (&job.original_uhttpd, &job.written_uhttpd) {
        let original = original.clone();
        let written = written.clone();
        let restore = async {
            let staged = move_https(
                &original,
                job.check.bind,
                job.check.management_port.unwrap(),
                true,
            )?;
            let current = std::fs::read(&paths.uhttpd).map_err(|e| e.to_string());
            match current {
                Ok(current) if current == original || current == written || current == staged => {
                    let safe = backend
                        .inspect(paths, job)
                        .await
                        .map(|c| {
                            c.available
                                || !c.owners.is_empty()
                                    && c.owners
                                        .iter()
                                        .all(|o| o.service.as_deref() == Some("uhttpd"))
                        })
                        .unwrap_or(false);
                    let target = if safe {
                        original.clone()
                    } else {
                        errors.push("原端口仍被占用，保留备用管理入口".into());
                        staged.clone()
                    };
                    job.written_uhttpd = Some(target.clone());
                    write_job(paths, job)?;
                    if current == target {
                        Ok(())
                    } else {
                        replace_if(&paths.uhttpd, &current, &target)
                    }
                }
                Ok(_) => Err("管理配置已被其他操作修改，未覆盖".into()),
                Err(e) => Err(e.to_string()),
            }
        }
        .await;
        match restore {
            Ok(()) => {
                if let Err(e) = backend.service(paths, "uhttpd", "restart").await {
                    errors.push(e)
                }
            }
            Err(e) => errors.push(e),
        }
    }
    for service in job.stopped.iter().rev() {
        if let Err(e) = backend.service(paths, service, "start").await {
            errors.push(e);
        }
    }
    if cowplay_safe && job.cowplay_touched && job.check.running_port.is_some() {
        if let Err(e) = backend.service(paths, "cowplay", "start").await {
            errors.push(e);
        }
    }
    if errors.is_empty() {
        backend.restored(paths, job).await
    } else {
        Err(errors.join("；"))
    }
}
pub async fn worker(id: &str, recovery: bool) -> Result<()> {
    if !Path::new("/etc/openwrt_release").is_file() { return Err("network migration requires OpenWrt".into()); }
    let paths = Paths::live();
    let _lock = job_lock(&paths)?;
    let mut job = read_job(&paths, id)?;
    if recovery {
        if job.phase != "recover_queued" {
            return Ok(());
        }
        let result =
            tokio::time::timeout(Duration::from_secs(60), recover(&paths, &mut job, &Live))
                .await
                .unwrap_or_else(|_| Err("恢复操作超时".into()));
        job.phase = if result.is_ok() {
            "rolled_back"
        } else {
            "recovery_required"
        }
        .into();
        job.message = match result {
            Ok(()) => "已恢复原配置与服务".into(),
            Err(e) => format!("恢复未完成：{e}"),
        };
        write_job(&paths, &job)?;
        if job.phase == "rolled_back" {
            release(&paths, &job);
        }
        return Ok(());
    }
    if job.phase != "queued" {
        return Ok(());
    }
    job.pid = std::process::id();
    job.created = now();
    write_job(&paths, &job)?;
    let result = tokio::time::timeout(Duration::from_secs(120), execute(&paths, &mut job, &Live))
        .await
        .unwrap_or_else(|_| Err("接管任务超时".into()));
    if let Err(error) = result {
        let recovery =
            tokio::time::timeout(Duration::from_secs(60), recover(&paths, &mut job, &Live))
                .await
                .unwrap_or_else(|_| Err("恢复操作超时".into()));
        job.phase = if recovery.is_ok() {
            "rolled_back"
        } else {
            "recovery_required"
        }
        .into();
        job.message = match recovery {
            Ok(()) => format!("{error}；已恢复原配置与服务"),
            Err(e) => format!("{error}；恢复未完成：{e}"),
        };
        write_job(&paths, &job)?;
    }
    if job.phase != "recovery_required" {
        release(&paths, &job);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };
    struct Mock {
        calls: Mutex<Vec<(String, String)>>,
        confirmed: bool,
        health_ok: bool,
        restored_ok: bool,
        changed_owner: bool,
        concurrent_config: bool,
        fail: Option<(&'static str, &'static str)>,
        failed: AtomicBool,
        cow_running: AtomicBool,
        cow_target: AtomicBool,
        outsider_after_release: bool,
        outsider_active: AtomicBool,
    }
    impl Mock {
        fn good() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                confirmed: true,
                health_ok: true,
                restored_ok: true,
                changed_owner: false,
                concurrent_config: false,
                fail: None,
                failed: AtomicBool::new(false),
                cow_running: AtomicBool::new(true),
                cow_target: AtomicBool::new(false),
                outsider_after_release: false,
                outsider_active: AtomicBool::new(false),
            }
        }
        fn calls(&self) -> Vec<(String, String)> {
            self.calls.lock().unwrap().clone()
        }
    }
    impl Backend for Mock {
        async fn inspect(&self, paths: &Paths, job: &Job) -> Result<Check> {
            let mut report = job.check.clone();
            let calls = self.calls();
            let pkg = crate::uci::Package::read(&paths.uhttpd)?;
            let https = pkg
                .sections
                .iter()
                .flat_map(|s| s.options.iter())
                .filter(|(k, _)| k == "listen_https")
                .flat_map(|(_, v)| match v {
                    crate::uci::Value::Single(v) => vec![v.clone()],
                    crate::uci::Value::List(v) => v.clone(),
                })
                .any(|a| {
                    a.parse::<SocketAddr>()
                        .is_ok_and(|a| overlaps(a, job.check.bind))
                });
            if self.outsider_after_release && !https {
                self.outsider_active.store(true, Ordering::Release);
            }
            report.owners.retain(|o| {
                if o.service.as_deref() == Some("uhttpd") {
                    https && !self.outsider_active.load(Ordering::Acquire)
                } else {
                    calls
                        .iter()
                        .rev()
                        .find(|(name, _)| Some(name) == o.service.as_ref())
                        .is_none_or(|(_, action)| action != "stop")
                }
            });
            for owner in &mut report.owners {
                if self.changed_owner {
                    owner.start += 1;
                }
                if owner.service.as_deref() == Some("uhttpd") {
                    owner.pid = owner.pid.map(|p| {
                        p + calls
                            .iter()
                            .filter(|(n, a)| n == "uhttpd" && a == "restart")
                            .count() as u32
                    });
                }
            }
            if self.cow_target.load(Ordering::Acquire) {
                report.owners.push(Owner {
                    pid: Some(5000),
                    process: "cowplay".into(),
                    service: Some("cowplay".into()),
                    addresses: vec![job.check.bind],
                    start: 999,
                    inodes: vec![100],
                });
            }
            if self.outsider_active.load(Ordering::Acquire) {
                report.owners.push(Owner {
                    pid: Some(5001),
                    process: "unrelated".into(),
                    service: None,
                    addresses: vec![job.check.bind],
                    start: 1000,
                    inodes: vec![101],
                });
            }
            report.available = report.owners.is_empty();
            Ok(report)
        }
        async fn service(&self, paths: &Paths, name: &str, action: &str) -> Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push((name.into(), action.into()));
            if self.fail == Some((name, action)) && !self.failed.swap(true, Ordering::AcqRel) {
                return Err("合成服务操作失败".into());
            }
            if name == "cowplay" {
                if action == "stop" {
                    self.cow_running.store(false, Ordering::Release);
                    self.cow_target.store(false, Ordering::Release);
                } else if action == "restart" || !self.cow_running.load(Ordering::Acquire) {
                    self.cow_running.store(true, Ordering::Release);
                    self.cow_target.store(
                        crate::uci::Package::read(&paths.cowplay)?.get("main", "port")
                            == Some("443"),
                        Ordering::Release,
                    );
                }
            }
            Ok(())
        }
        async fn management(&self, paths: &Paths, job: &Job) -> Result<Job> {
            let text = std::fs::read_to_string(&paths.uhttpd).unwrap();
            assert!(
                text.contains("0.0.0.0:443") && text.contains("0.0.0.0:8443"),
                "old management listener must remain until the browser confirms"
            );
            if !self.confirmed {
                return Err("合成浏览器未确认新入口".into());
            }
            let mut job = read_job(paths, &job.id)?;
            write_confirmation(paths, &job)?;
            job.confirmed = true;
            if self.concurrent_config {
                std::fs::write(&paths.uhttpd, text + "# concurrent change\n").unwrap();
            }
            Ok(job)
        }
        async fn health(&self, _: &Paths, _: &Job) -> Result<()> {
            if self.health_ok {
                Ok(())
            } else {
                Err("合成 HTTPS 验证失败".into())
            }
        }
        async fn restored(&self, _: &Paths, _: &Job) -> Result<()> {
            if self.restored_ok {
                Ok(())
            } else {
                Err("合成原监听未建立".into())
            }
        }
    }
    fn fixture() -> (tempfile::TempDir, Paths, Job) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            proc: dir.path().join("proc"),
            uhttpd: dir.path().join("uhttpd"),
            cowplay: dir.path().join("cowplay"),
            config: dir.path().join("config"),
            init: dir.path().join("init"),
            jobs: dir.path().join("jobs"),
        };
        std::fs::create_dir_all(paths.jobs.join("takeover-synthetic")).unwrap();
        let original_cowplay=b"config cowplay 'main'\n option port '18443'\n option custom 'synthetic-private-config-only'\n".to_vec();
        let applied_cowplay=b"config cowplay 'main'\n option port '443'\n option custom 'synthetic-private-config-only'\n".to_vec();
        let uhttpd=b"config uhttpd 'main'\n list listen_http '0.0.0.0:80'\n list listen_https '0.0.0.0:443'\n list listen_https '[::]:443'\n option cert '/synthetic/cert.pem'\n option key '/synthetic/key.pem'\n option custom 'keep'\n".to_vec();
        std::fs::write(&paths.cowplay, &applied_cowplay).unwrap();
        std::fs::write(&paths.uhttpd, &uhttpd).unwrap();
        let owner = Owner {
            pid: Some(4242),
            process: "uhttpd".into(),
            service: Some("uhttpd".into()),
            addresses: vec!["0.0.0.0:443".parse().unwrap()],
            start: 123,
            inodes: vec![10],
        };
        let check = Check {
            ok: true,
            port: 443,
            bind: "0.0.0.0:443".parse().unwrap(),
            running_port: Some(18443),
            available: false,
            owners: vec![owner],
            can_takeover: true,
            management_port: Some(8443),
            fingerprint: "synthetic".into(),
            issue: None,
        };
        let job = Job {
            id: "takeover-synthetic".into(),
            phase: "queued".into(),
            message: String::new(),
            check,
            management_scheme: "https".into(),
            management_port: 8443,
            original_management_port: 443,
            confirmed: false,
            created: now(),
            pid: 0,
            saved_cowplay: original_cowplay.clone(),
            original_cowplay,
            applied_cowplay: Some(applied_cowplay),
            original_uhttpd: Some(uhttpd),
            written_uhttpd: None,
            stopped: Vec::new(),
            cowplay_touched: false,
        };
        write_job(&paths, &job).unwrap();
        (dir, paths, job)
    }
    #[test]
    fn proc_listener_identity_handles_ipv4_ipv6_and_pid_reuse() {
        use std::os::unix::fs::symlink;
        let (_dir, paths, _job) = fixture();
        std::fs::create_dir_all(paths.proc.join("net")).unwrap();
        std::fs::create_dir_all(paths.proc.join("4242/fd")).unwrap();
        std::fs::write(paths.proc.join("net/tcp"),"header\n0: 00000000:01BB 00000000:0000 0A 0:0 0:0 0 0 0 10\n1: 0100007F:01BB 00000000:0000 01 0:0 0:0 0 0 0 12\n").unwrap();
        std::fs::write(paths.proc.join("net/tcp6"),"header\n0: 00000000000000000000000000000000:01BB 00000000000000000000000000000000:0000 0A 0:0 0:0 0 0 0 11\n").unwrap();
        symlink("socket:[10]", paths.proc.join("4242/fd/3")).unwrap();
        symlink("socket:[11]", paths.proc.join("4242/fd/4")).unwrap();
        std::fs::write(paths.proc.join("4242/comm"), "uhttpd\n").unwrap();
        let mut fields = vec!["0"; 25];
        fields[0] = "S";
        fields[19] = "987";
        std::fs::write(
            paths.proc.join("4242/stat"),
            format!("4242 (uhttpd worker) {}", fields.join(" ")),
        )
        .unwrap();
        let result = owners(&paths, "0.0.0.0:443".parse().unwrap());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].start, 987);
        assert_eq!(result[0].inodes, vec![10]);
        assert_eq!(result[0].addresses.len(), 1);
        assert_eq!(
            owners(&paths, "[::]:443".parse().unwrap())[0].inodes,
            vec![11]
        );
        assert!(!overlaps(
            "192.0.2.1:443".parse().unwrap(),
            "192.0.2.2:443".parse().unwrap()
        ));
        assert!(
            !overlaps("[::]:443".parse().unwrap(), "0.0.0.0:443".parse().unwrap()),
            "v6-only must not be guessed to conflict with IPv4"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn real_linux_listener_is_associated_with_its_process_and_released_inode() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let paths = Paths::live();
        let original = owners(&paths, address)
            .into_iter()
            .find(|owner| owner.pid == Some(std::process::id()))
            .expect("the real listener must resolve to this test process");
        assert!(original.start > 0);
        assert!(!original.inodes.is_empty());
        drop(listener);
        assert!(!owners(&paths, address).iter().any(|owner| {
            owner
                .inodes
                .iter()
                .any(|inode| original.inodes.contains(inode))
        }));
    }
    #[tokio::test]
    async fn confirmed_uhttpd_takeover_keeps_management_and_preserves_other_settings() {
        let (_dir, paths, mut job) = fixture();
        let backend = Mock::good();
        execute(&paths, &mut job, &backend).await.unwrap();
        assert_eq!(job.phase, "complete");
        let config = std::fs::read_to_string(&paths.uhttpd).unwrap();
        assert!(!config.contains("0.0.0.0:443'"));
        assert!(config.contains("0.0.0.0:8443'"));
        assert!(
            config.contains("[::]:443'"),
            "non-conflicting IPv6 configuration stays intact"
        );
        assert!(config.contains(":80'"));
        assert!(config.contains("/synthetic/key.pem"));
        assert!(config.contains("'keep'"));
        assert_eq!(
            backend.calls(),
            vec![
                ("uhttpd".into(), "restart".into()),
                ("uhttpd".into(), "restart".into()),
                ("cowplay".into(), "restart".into())
            ]
        );
        let public = public_job(&job).to_string();
        assert!(!public.contains("synthetic-private-config-only"));
        assert!(!public.contains("/synthetic/key.pem"));
    }
    #[tokio::test]
    async fn management_confirmation_failure_restores_without_releasing_old_port() {
        let (_dir, paths, mut job) = fixture();
        let mut backend = Mock::good();
        backend.confirmed = false;
        assert!(execute(&paths, &mut job, &backend).await.is_err());
        recover(&paths, &mut job, &backend).await.unwrap();
        assert_eq!(
            std::fs::read(&paths.uhttpd).unwrap(),
            job.original_uhttpd.unwrap()
        );
        assert_eq!(std::fs::read(&paths.cowplay).unwrap(), job.original_cowplay);
        assert!(backend.calls().iter().all(|(name, _)| name == "uhttpd"));
    }
    #[tokio::test]
    async fn failed_service_or_https_recovery_restores_files_and_previous_receiver() {
        for failure in [
            Some(("uhttpd", "restart")),
            Some(("cowplay", "restart")),
            None,
        ] {
            let (_dir, paths, mut job) = fixture();
            let mut backend = Mock::good();
            backend.fail = failure;
            backend.health_ok = failure.is_some();
            assert!(execute(&paths, &mut job, &backend).await.is_err());
            recover(&paths, &mut job, &backend).await.unwrap();
            assert_eq!(
                std::fs::read(&paths.uhttpd).unwrap(),
                job.original_uhttpd.unwrap()
            );
            assert_eq!(std::fs::read(&paths.cowplay).unwrap(), job.original_cowplay);
            if job.cowplay_touched {
                assert!(
                    backend
                        .calls()
                        .contains(&("cowplay".into(), "start".into()))
                );
            }
        }
    }
    #[tokio::test]
    async fn changed_owner_is_not_stopped_and_concurrent_config_is_not_overwritten() {
        let (_dir, paths, mut job) = fixture();
        let mut backend = Mock::good();
        backend.changed_owner = true;
        assert!(
            execute(&paths, &mut job, &backend)
                .await
                .unwrap_err()
                .contains("占用者已变化")
        );
        assert!(backend.calls().is_empty());
        let (_dir, paths, mut job) = fixture();
        let mut backend = Mock::good();
        backend.concurrent_config = true;
        assert!(execute(&paths, &mut job, &backend).await.is_err());
        assert!(
            recover(&paths, &mut job, &backend)
                .await
                .unwrap_err()
                .contains("未覆盖")
        );
        assert!(
            std::fs::read_to_string(&paths.uhttpd)
                .unwrap()
                .contains("# concurrent change")
        );
    }
    #[tokio::test]
    async fn identified_other_service_is_stopped_once_and_restarted_on_failure() {
        let (_dir, paths, mut job) = fixture();
        job.check.owners.push(Owner {
            pid: Some(4243),
            process: "nginx".into(),
            service: Some("nginx".into()),
            addresses: vec!["0.0.0.0:443".parse().unwrap()],
            start: 124,
            inodes: vec![12],
        });
        write_job(&paths, &job).unwrap();
        let mut backend = Mock::good();
        backend.health_ok = false;
        assert!(execute(&paths, &mut job, &backend).await.is_err());
        recover(&paths, &mut job, &backend).await.unwrap();
        assert_eq!(
            backend
                .calls()
                .iter()
                .filter(|(n, a)| n == "nginx" && a == "stop")
                .count(),
            1
        );
        assert!(backend.calls().contains(&("nginx".into(), "start".into())));
    }
    #[tokio::test]
    async fn concurrent_cowplay_edit_does_not_skip_other_service_recovery() {
        let (_dir, paths, mut job) = fixture();
        job.check.owners.push(Owner {
            pid: Some(4243),
            process: "nginx".into(),
            service: Some("nginx".into()),
            addresses: vec!["0.0.0.0:443".parse().unwrap()],
            start: 124,
            inodes: vec![12],
        });
        write_job(&paths, &job).unwrap();
        let mut backend = Mock::good();
        backend.health_ok = false;
        assert!(execute(&paths, &mut job, &backend).await.is_err());
        let changed =
            std::fs::read_to_string(&paths.cowplay).unwrap() + "# external CowPlay edit\n";
        std::fs::write(&paths.cowplay, &changed).unwrap();
        assert!(recover(&paths, &mut job, &backend).await.is_err());
        assert_eq!(std::fs::read_to_string(&paths.cowplay).unwrap(), changed);
        assert!(
            backend.calls().contains(&("nginx".into(), "start".into())),
            "one CAS mismatch must not skip other service recovery"
        );
    }
    #[tokio::test]
    async fn journal_failure_does_not_skip_independent_service_recovery() {
        let (_dir, paths, mut job) = fixture();
        job.check.owners.push(Owner {
            pid: Some(4243),
            process: "nginx".into(),
            service: Some("nginx".into()),
            addresses: vec!["0.0.0.0:443".parse().unwrap()],
            start: 124,
            inodes: vec![12],
        });
        write_job(&paths, &job).unwrap();
        let mut backend = Mock::good();
        backend.health_ok = false;
        assert!(execute(&paths, &mut job, &backend).await.is_err());
        let journal = job_path(&paths, &job.id).unwrap();
        std::fs::remove_file(&journal).unwrap();
        std::fs::create_dir(&journal).unwrap();
        assert!(recover(&paths, &mut job, &backend).await.is_err());
        assert!(
            backend.calls().contains(&("nginx".into(), "start".into())),
            "a failed management journal must not skip independent service recovery"
        );
        assert!(
            backend
                .calls()
                .contains(&("cowplay".into(), "start".into()))
        );
    }
    #[tokio::test]
    async fn applied_port_can_be_recovered_before_worker_commit() {
        let (_dir, paths, mut job) = fixture();
        job.phase = "prepared".into();
        job.applied_cowplay = None;
        let backend = Mock::good();
        recover(&paths, &mut job, &backend).await.unwrap();
        assert_eq!(
            crate::uci::Package::read(&paths.cowplay)
                .unwrap()
                .get("main", "port"),
            Some("18443"),
            "an applied port must not remain behind an uncommitted takeover"
        );
        assert!(backend.calls().is_empty());
        job.saved_cowplay = job
            .applied_cowplay
            .clone()
            .unwrap_or_else(|| b"config cowplay 'main'\n option port '443'\n".to_vec());
        std::fs::write(&paths.cowplay, &job.saved_cowplay).unwrap();
        restore_prepared_port(&paths, &job).unwrap();
        assert_eq!(
            crate::uci::Package::read(&paths.cowplay)
                .unwrap()
                .get("main", "port"),
            Some("18443")
        );
        job.check.running_port = None;
        job.original_cowplay = b"config cowplay 'main'\n option custom 'keep'\n".to_vec();
        std::fs::write(
            &paths.cowplay,
            "config cowplay 'main'\n option port '443'\n option custom 'external edit kept'\n",
        )
        .unwrap();
        restore_prepared_port(&paths, &job).unwrap();
        let restored = crate::uci::Package::read(&paths.cowplay).unwrap();
        assert_eq!(restored.get("main", "port"), None);
        assert_eq!(restored.get("main", "custom"), Some("external edit kept"));
        assert_eq!(crate::policy::Policy::from_uci(&restored).port, 18443);
    }
    #[test]
    fn delayed_confirmation_cannot_rewrite_worker_journal() {
        let (_dir, paths, mut job) = fixture();
        job.phase = "await_management".into();
        write_job(&paths, &job).unwrap();
        let delayed = read_job(&paths, &job.id).unwrap();
        write_confirmation(&paths, &delayed).unwrap();
        job.phase = "starting".into();
        job.stopped.push("nginx".into());
        write_job(&paths, &job).unwrap();
        write_confirmation(&paths, &delayed).unwrap();
        let current = read_job(&paths, &job.id).unwrap();
        assert_eq!(current.phase, "starting");
        assert_eq!(current.stopped, vec!["nginx"]);
    }
    #[tokio::test]
    async fn failed_stop_keeps_backup_management_and_zero_exit_is_not_recovery_proof() {
        let (_dir, paths, mut job) = fixture();
        let mut backend = Mock::good();
        backend.health_ok = false;
        backend.fail = Some(("cowplay", "stop"));
        assert!(execute(&paths, &mut job, &backend).await.is_err());
        assert!(recover(&paths, &mut job, &backend).await.is_err());
        assert!(
            std::fs::read_to_string(&paths.uhttpd)
                .unwrap()
                .contains("0.0.0.0:8443")
        );
        let (_dir, paths, mut job) = fixture();
        let mut backend = Mock::good();
        backend.health_ok = false;
        backend.restored_ok = false;
        assert!(execute(&paths, &mut job, &backend).await.is_err());
        assert!(
            recover(&paths, &mut job, &backend)
                .await
                .unwrap_err()
                .contains("监听未建立")
        );
    }
    #[tokio::test]
    async fn unrelated_trusted_https_200_is_not_a_receiver_health_check() {
        use std::io::{Read, Write};
        let dir = tempfile::tempdir().unwrap();
        let generated = rcgen::generate_simple_self_signed(vec!["cowplay.test".into()]).unwrap();
        let certificate = dir.path().join("cert.pem");
        std::fs::write(&certificate, generated.cert.pem()).unwrap();
        let config = dir.path().join("config.json");
        std::fs::write(&config,serde_json::json!({"receiver":{"web":{"origin":"https://cowplay.test","tls_certificate":certificate}}}).to_string()).unwrap();
        let uci = dir.path().join("cowplay");
        std::fs::write(&uci, "config cowplay 'main'\n").unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let key = rustls_pki_types::PrivateKeyDer::Pkcs8(
            rustls_pki_types::PrivatePkcs8KeyDer::from(generated.signing_key.serialize_der()),
        );
        let tls = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![generated.cert.der().clone()], key)
        .unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut stream = rustls::StreamOwned::new(
                rustls::ServerConnection::new(std::sync::Arc::new(tls)).unwrap(),
                stream,
            );
            let mut buffer = [0; 1024];
            stream.read(&mut buffer).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
            stream.flush().unwrap();
        });
        assert!(
            verify_https(&config, &uci, address)
                .await
                .unwrap_err()
                .contains("CowPlay 健康响应")
        );
        server.join().unwrap();
    }
    #[tokio::test]
    async fn new_owner_before_receiver_start_cannot_remove_backup_management() {
        let (_dir, paths, mut job) = fixture();
        let mut backend = Mock::good();
        backend.outsider_after_release = true;
        assert!(execute(&paths, &mut job, &backend).await.is_err());
        assert!(!job.cowplay_touched);
        assert!(
            recover(&paths, &mut job, &backend)
                .await
                .unwrap_err()
                .contains("保留备用管理入口")
        );
        assert!(
            std::fs::read_to_string(&paths.uhttpd)
                .unwrap()
                .contains("0.0.0.0:8443")
        );
        assert!(!backend.calls().iter().any(|(name, _)| name == "unrelated"));
    }
    #[test]
    fn reservations_and_job_paths_do_not_allow_parallel_or_arbitrary_actions() {
        let (_dir, paths, job) = fixture();
        let lock = job_lock(&paths).unwrap();
        assert!(job_lock(&paths).is_err());
        drop(lock);
        reserve(&paths, &job).unwrap();
        assert!(reserve(&paths, &job).is_err());
        release(&paths, &job);
        assert!(job_path(&paths, "../../etc/config").is_err());
        assert!(!valid_service("uhttpd;reboot"));
        assert!(!valid_service("../network"));
        let mut changed = job.check.clone();
        let before = fingerprint(&changed, &paths);
        changed.owners[0].start += 1;
        assert_ne!(before, fingerprint(&changed, &paths));
    }
}
