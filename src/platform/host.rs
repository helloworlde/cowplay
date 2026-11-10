use serde_json::{Value, json};
use std::time::Duration;

// ---------- helpers: /proc and system probes ----------

pub(crate) fn read_cpu() -> Option<(u64, u64)> {
    let text = std::fs::read_to_string("/proc/stat").ok()?;
    let line = text.lines().next()?.strip_prefix("cpu ")?;
    let v: Vec<u64> = line.split_whitespace().filter_map(|x| x.parse().ok()).collect();
    if v.len() < 4 {
        return None;
    }
    let idle = v[3] + v.get(4).copied().unwrap_or(0);
    let total: u64 = v.iter().take(8).sum();
    Some((total - idle, total))
}
pub(crate) fn read_proc_ticks() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/self/stat").ok()?;
    let rest = &text[text.rfind(')')? + 2..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    Some(f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?)
}
/// First number of each `Key: value [unit]` line of a /proc file.
fn read_proc_numbers(path: &str) -> std::collections::BTreeMap<String, u64> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            Some((k.to_owned(), v.split_whitespace().next()?.parse().ok()?))
        })
        .collect()
}
pub(crate) fn read_proc_status() -> std::collections::BTreeMap<String, u64> {
    read_proc_numbers("/proc/self/status")
}
pub(crate) fn read_meminfo() -> std::collections::BTreeMap<String, u64> {
    read_proc_numbers("/proc/meminfo")
}
pub(crate) fn read_temps() -> Vec<(String, f64)> {
    let mut out = Vec::new();
    if let Ok(dir) = std::fs::read_dir("/sys/class/thermal") {
        for e in dir.flatten() {
            let p = e.path();
            if !e.file_name().to_string_lossy().starts_with("thermal_zone") {
                continue;
            }
            let t = std::fs::read_to_string(p.join("temp"))
                .ok()
                .and_then(|s| s.trim().parse::<f64>().ok());
            let kind = std::fs::read_to_string(p.join("type"))
                .map(|s| s.trim().to_owned())
                .unwrap_or_else(|_| e.file_name().to_string_lossy().into_owned());
            if let Some(t) = t {
                let c = if t > 1000.0 { t / 1000.0 } else { t };
                if (-40.0..150.0).contains(&c) {
                    out.push((kind, (c * 10.0).round() / 10.0));
                }
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}
pub(crate) fn disk_free(path: &str) -> Option<Value> {
    let c = std::ffi::CString::new(path).ok()?;
    // SAFETY: zeroed statvfs is a valid out-parameter; c is NUL-terminated.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let bs = st.f_frsize as u64;
    Some(json!({"path": path, "free_kb": st.f_bavail as u64 * bs / 1024, "total_kb": st.f_blocks as u64 * bs / 1024}))
}
pub(crate) fn default_route() -> Option<String> {
    std::fs::read_to_string("/proc/net/route")
        .ok()?
        .lines()
        .skip(1)
        .find_map(|l| {
            let f: Vec<_> = l.split_whitespace().collect();
            (f.len() > 2 && f[1] == "00000000").then(|| f[0].to_owned())
        })
}
pub(crate) fn local_addresses() -> Vec<std::net::IpAddr> {
    let mut out = Vec::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs allocates a list we walk read-only and free once.
    unsafe {
        if libc::getifaddrs(&mut head) != 0 {
            return out;
        }
        let mut cur = head;
        while !cur.is_null() {
            let a = (*cur).ifa_addr;
            if !a.is_null() {
                match (*a).sa_family as i32 {
                    libc::AF_INET => {
                        let s = &*(a as *const libc::sockaddr_in);
                        out.push(std::net::IpAddr::from(u32::from_be(s.sin_addr.s_addr).to_be_bytes()));
                    }
                    libc::AF_INET6 => {
                        let s = &*(a as *const libc::sockaddr_in6);
                        out.push(std::net::IpAddr::from(s.sin6_addr.s6_addr));
                    }
                    _ => {}
                }
            }
            cur = (*cur).ifa_next;
        }
        libc::freeifaddrs(head);
    }
    out
}
pub(crate) async fn run_bounded(program: &str, args: &[&str]) -> Option<String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(2), cmd.output())
        .await
        .ok()?
        .ok()?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Some(text)
}
pub(crate) async fn command_version(program: &str, args: &[&str]) -> Option<String> {
    let text = run_bounded(program, args).await?;
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(80).collect())
}
pub(crate) async fn iw_interfaces() -> Vec<(String, String)> {
    let Some(text) = run_bounded("iw", &["dev"]).await else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut current: Option<String> = None;
    for l in text.lines().map(str::trim) {
        if let Some(i) = l.strip_prefix("Interface ") {
            current = Some(i.to_owned());
        } else if let (Some(s), Some(i)) = (l.strip_prefix("ssid "), current.as_ref()) {
            out.push((i.clone(), s.to_owned()));
        }
    }
    out
}
pub(crate) async fn station_signal(mac: &str) -> Option<i32> {
    if mac.len() != 17 || !mac.chars().all(|c| c.is_ascii_hexdigit() || c == ':') {
        return None;
    }
    for (ifname, _) in iw_interfaces().await {
        if let Some(text) = run_bounded("iw", &["dev", &ifname, "station", "get", mac]).await {
            if let Some(v) = text.lines().find_map(|l| {
                l.trim()
                    .strip_prefix("signal:")
                    .and_then(|v| v.split_whitespace().next())
                    .and_then(|v| v.parse::<i32>().ok())
            }) {
                return Some(v);
            }
        }
    }
    None
}
