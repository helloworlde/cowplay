use super::{log_storage_check, Admin, RESOLUTIONS, duration_text, fit_window, redact, render_toml, saved_phone_name, step_name, summary, tls_info, unsupported};
use crate::platform::host::{command_version, default_route, disk_free, iw_interfaces, local_addresses, read_cpu, read_meminfo, read_proc_status, read_proc_ticks, read_temps, station_signal};
use f50_server::diag::{self, LogQuery, diag};
use serde_json::{Value, json};
use std::{path::Path, time::{Duration, Instant}};

impl Admin {
    // ---------- sampling (called every second by the service) ----------
    pub async fn sample(&self) {
        let cpu_total = read_cpu();
        let proc_ticks = read_proc_ticks();
        let ncpu = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1) as f64;
        let temps = read_temps();
        let max_temp = temps.iter().map(|(_, t)| *t).fold(None, |a: Option<f64>, t| {
            Some(a.map_or(t, |a| a.max(t)))
        });
        let mem = read_meminfo();
        let status = read_proc_status();
        let (cpu, proc_cpu) = {
            let mut s = self.sys.lock().unwrap();
            if let (Some((busy, total)), Some((pb, pt))) = (cpu_total, s.cpu_prev) {
                if total > pt {
                    s.cpu = Some((busy.saturating_sub(pb)) as f64 * 100.0 / (total - pt) as f64);
                }
            }
            s.cpu_prev = cpu_total;
            if let Some(ticks) = proc_ticks {
                if let Some((prev, at)) = s.proc_prev {
                    let hz = 100.0; // USER_HZ on Linux/OpenWrt
                    let dt = at.elapsed().as_secs_f64().max(0.001);
                    s.proc_cpu = Some(((ticks.saturating_sub(prev)) as f64 / hz) / dt * 100.0 / ncpu);
                }
                s.proc_prev = Some((ticks, Instant::now()));
            }
            if let Some(rss) = status.get("VmRSS") {
                s.peak_rss_kb = s.peak_rss_kb.max(*rss);
            }
            (s.cpu, s.proc_cpu)
        };
        self.phone_probe().await;
        let link = diag().link();
        if link.wifi == "active" && diag::now_ms().saturating_sub(link.updated_ms) > 30_000 {
            diag().link_fail(
                "wifi",
                "iPhone 30 秒内没有通过 Wi‑Fi 连上 F50",
                "确认 iPhone 的 Wi‑Fi 已打开，没有锁定在其他网络；然后点“连接 iPhone”重试。",
            );
        }
        let phone = self.sys.lock().unwrap().phone.clone();
        diag().set_system(json!({
            "cpu_percent": cpu,
            "process_cpu_percent": proc_cpu,
            "temps": temps.iter().map(|(k,v)| json!({"zone":k,"c":v})).collect::<Vec<_>>(),
            "temp_max_c": max_temp,
            "mem_total_kb": mem.get("MemTotal"),
            "mem_available_kb": mem.get("MemAvailable"),
            "rss_kb": status.get("VmRSS"),
            "threads": status.get("Threads"),
            "phone": phone.as_ref().map(|(ip,name,rssi,_)| json!({"ip":ip,"name":name,"rssi_dbm":rssi})),
        }));
        diag().tick(cpu, max_temp);
        let health_due = {
            let bucket = diag().started.elapsed().as_secs() / 10;
            let mut sys = self.sys.lock().unwrap();
            let due = bucket != sys.health_bucket;
            sys.health_bucket = bucket;
            due
        };
        if health_due {
            let metrics = diag().metrics();
            f50_server::diag_log!(Info, "health", "sample={} cpu={cpu:?} proc_cpu={proc_cpu:?} temp={max_temp:?} rss_kb={:?} frames={} bytes={} dropped={} viewer={} storage={}",
                diag().started.elapsed().as_secs(), status.get("VmRSS"), metrics["server"]["frames_in"], metrics["server"]["bytes_in"], metrics["server"]["dropped"], metrics["viewer"], diag().storage_status());
        }
        // Persist viewport reports without terminating an active phone for geometry alone.
        self.apply_display(false);
        self.observe_auto_fps();
        crate::console::sample_wifi(self).await;
    }
    /// Phone hostname from DHCP leases and RSSI from `iw`, refreshed every five seconds.
    async fn phone_probe(&self) {
        let Some(ip) = diag().peer() else {
            self.sys.lock().unwrap().phone = None;
            return;
        };
        if self
            .sys
            .lock()
            .unwrap()
            .phone
            .as_ref()
            .is_some_and(|(p, _, _, t)| *p == ip && t.elapsed() < Duration::from_secs(5))
        {
            return;
        }
        let lease = std::fs::read_to_string("/tmp/dhcp.leases")
            .ok()
            .and_then(|text| {
                text.lines().find_map(|l| {
                    let f: Vec<_> = l.split_whitespace().collect();
                    (f.len() >= 4 && f[2] == ip).then(|| (f[1].to_owned(), f[3].to_owned()))
                })
            });
        let name = lease
            .as_ref()
            .map(|(_, h)| h.clone())
            .filter(|h| h != "*" && h.len() <= 64);
        // The kernel neighbour entry exists for any live peer; the lease file only for dnsmasq clients.
        let rssi = match crate::console::peer_mac(&ip).or(lease.map(|(m, _)| m)) {
            Some(mac) => station_signal(&mac).await,
            None => None,
        };
        if let Some(n) = &name {
            diag().set_phone(n.clone());
        }
        self.sys.lock().unwrap().phone = Some((ip, name, rssi, Instant::now()));
    }

    // ---------- reports ----------
    pub fn overview(&self) -> Value {
        let (current, history) = diag().sessions();
        json!({
            "build": f50_server::build_info::BUILD_ID,
            "version": env!("CARGO_PKG_VERSION"),
            "diag_code": diag().diag_code(),
            "now_ms": diag::now_ms(),
            "started_ms": diag().started_ms,
            "uptime_s": diag().started.elapsed().as_secs(),
            "origin": self.origin,
            "pairing": self.manager.status(),
            "selection_epoch":self.hub.phones.epoch(),
            "hub": self.hub.observation(),
            "link": diag().link(),
            "phone_name": saved_phone_name(&self.state_dir),
            "pair_setup_s": self.hub.pair_setup_remaining().as_secs(),
            "session": current.map(|c| summary(&c)),
            "sessions": history.iter().map(summary).collect::<Vec<_>>(),
            "prefs": self.prefs(),
            "verbose_remaining_s": diag().verbose_remaining(),
            "warnings_24h": diag().warning_count(diag::now_ms().saturating_sub(86_400_000)),
            "display": {
                "configured": {"width": self.display.0, "height": self.display.1},
                "spec": self.hub.display_spec(),
                "resolutions": RESOLUTIONS,
                "car_window": diag().caps().and_then(|c| fit_window(c["width"].as_f64()? * c["dpr"].as_f64().unwrap_or(1.0), c["height"].as_f64()? * c["dpr"].as_f64().unwrap_or(1.0))),
                "car_hevc": diag().caps().and_then(|c| c["hevc"].as_bool()),
            },
            "unsupported": unsupported(),
            "ddns": {
                "token_set": std::fs::metadata(self.token_path()).is_ok_and(|m| m.len() > 0),
                "ip": self.ddns_ip(&self.prefs().ddns),
                "origin_host": self.origin.trim_start_matches("https://").split(':').next(),
                "cert_days": tls_info(&self.tls.lock().unwrap().0).ok().map(|(d, _)| d),
                "state": self.ddns_state.lock().unwrap().clone(),
            },
            "selftest": self.selftest.lock().unwrap().clone(),
        })
    }
    pub fn process(&self) -> Value {
        let status = read_proc_status();
        let fds = std::fs::read_dir("/proc/self/fd").map(|d| d.count()).ok();
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit writes into the provided struct only.
        let fd_limit = (unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0)
            .then_some(limit.rlim_cur as u64);
        // SAFETY: getppid has no preconditions.
        let ppid = unsafe { libc::getppid() };
        let s = self.sys.lock().unwrap();
        json!({
            "binary": std::fs::read_link("/proc/self/exe").ok().map(|p| p.display().to_string()),
            "version": env!("CARGO_PKG_VERSION"),
            "build": f50_server::build_info::BUILD_ID,
            "pid": std::process::id(),
            "ppid": ppid,
            "launcher": if ppid == 1 && self.openwrt { "procd（init 子进程，未配置自动拉起）" } else { "前台或其他父进程" },
            "uptime_s": diag().started.elapsed().as_secs(),
            "rss_kb": status.get("VmRSS"),
            "rss_peak_kb": status.get("VmHWM").copied().unwrap_or(s.peak_rss_kb),
            "threads": status.get("Threads"),
            "cpu_percent": s.proc_cpu,
            "cpus": std::thread::available_parallelism().map(|n| n.get()).ok(),
            "fds": fds,
            "fd_limit": fd_limit,
            "log_level": if diag().verbose() { "debug" } else { "info" },
            "log_sink": diag().sink_path(),
            "log_mirror": diag().mirror(),
            "ports": [
                {"port": self.web_bind.to_string(), "use": "HTTPS 网页 + 查看 WebSocket（车机 / 手机 / 管理）", "connections": if self.hub.viewer_active() { 1 } else { 0 }},
                {"port": self.control_bind.to_string(), "use": "CarPlay 控制（iPhone）", "connections": if self.hub.phone_active.load(std::sync::atomic::Ordering::Acquire) { 1 } else { 0 }},
                {"port": "5353/udp", "use": "mDNS 发现", "connections": Value::Null},
            ],
        })
    }
    pub async fn system(&self) -> Value {
        let versions = {
            let cached = self.sys.lock().unwrap().versions.clone();
            match cached {
                Some(v) => v,
                None => {
                    let v = json!({
                        // The private BlueZ needs its own libraries, exactly as the board starts it.
                        "bluez": if self.openwrt {
                            command_version("/usr/lib/cowplay/bluez/lib/ld-musl-aarch64.so.1", &["--library-path", "/usr/lib/cowplay/bluez/usr/lib:/usr/lib/cowplay/bluez/lib", "/usr/lib/cowplay/bluez/usr/bin/bluetoothd", "--version"]).await.map(|v| format!("BlueZ {v}（私有运行时）"))
                        } else { command_version("bluetoothd", &["--version"]).await.map(|v| format!("BlueZ {v}（系统服务）")) },
                        "hostapd": command_version("hostapd", &["-v"]).await,
                    });
                    self.sys.lock().unwrap().versions = Some(v.clone());
                    v
                }
            }
        };
        let release = std::fs::read_to_string("/etc/openwrt_release").unwrap_or_default();
        let field = |k: &str| {
            release.lines().find_map(|l| {
                l.strip_prefix(&format!("{k}="))
                    .map(|v| v.trim_matches(|c| c == '\'' || c == '"').to_owned())
            })
        };
        let model = std::fs::read_to_string("/tmp/sysinfo/model")
            .or_else(|_| std::fs::read_to_string("/proc/device-tree/model"))
            .ok()
            .map(|s| s.trim_matches(char::from(0)).trim().to_owned());
        let mem = read_meminfo();
        json!({
            "openwrt": field("DISTRIB_DESCRIPTION"),
            "openwrt_revision": field("DISTRIB_REVISION"),
            "arch": field("DISTRIB_ARCH").unwrap_or_else(|| std::env::consts::ARCH.into()),
            "platform": if self.openwrt { "f50" } else { "linux" },
            "kernel": std::fs::read_to_string("/proc/sys/kernel/osrelease").ok().map(|s| s.trim().to_owned()),
            "model": model,
            "mem_total_kb": mem.get("MemTotal"),
            "mem_available_kb": mem.get("MemAvailable"),
            "overlay": disk_free("/overlay").or_else(|| disk_free("/")),
            "temps": read_temps().iter().map(|(k,v)| json!({"zone":k,"c":v})).collect::<Vec<_>>(),
            "bluez": versions["bluez"],
            "hostapd": versions["hostapd"],
            "default_route": default_route(),
            "time_ms": diag::now_ms(),
            "time_plausible": diag::now_ms() > 1_735_689_600_000u64,
        })
    }
    pub async fn run_selftest(&self) -> Value {
        let mut checks = Vec::new();
        let mut add = |id: &str, label: &str, status: &str, detail: String| {
            checks.push(json!({"id":id,"label":label,"status":status,"detail":detail}))
        };
        let up = diag().started.elapsed().as_secs();
        add("service", "服务进程", "ok", format!("运行 {} · PID {}", duration_text(up), std::process::id()));
        let hci = Path::new(&format!("/sys/class/bluetooth/hci{}", self.manager.adapter_index())).exists();
        let powered = self.manager.adapter_powered().await;
        let pairing = self.manager.status();
        add(
            "bluetooth",
            &format!("蓝牙 hci{}", self.manager.adapter_index()),
            match (hci, powered) {
                (true, Some(true)) => "ok",
                (true, _) => "warn",
                _ => "fail",
            },
            match (hci, powered) {
                (false, _) => format!("未找到 hci{}", self.manager.adapter_index()),
                (true, None) => "BlueZ 未响应".into(),
                (true, Some(false)) => "适配器未上电".into(),
                (true, Some(true)) => format!(
                    "BlueZ 正常 · {}{}",
                    if pairing.paired { "已保存手机" } else { "未保存手机" },
                    if pairing.rfcomm { " · 手机通道已连接" } else { "" }
                ),
            },
        );
        match &self.wifi {
            Some((ssid, ch)) => {
                let found = iw_interfaces().await;
                let matched = found.iter().find(|(_, s)| s == ssid);
                add(
                    "wifi",
                    "Wi‑Fi 热点",
                    if matched.is_some() { "ok" } else { "warn" },
                    match (matched, found.is_empty()) {
                        (Some((ifname, _)), _) => format!("{ssid} · CH{ch} · {ifname}"),
                        (None, true) => format!("已配置 {ssid} · CH{ch}，无法用 iw 核对接口"),
                        (None, false) => format!("已配置 {ssid}，但没有接口在广播该 SSID"),
                    },
                );
            }
            None => add("wifi", "Wi‑Fi 热点", "fail", "未配置交接热点".into()),
        }
        match default_route() {
            Some(r) => add("uplink", "上行网络", "ok", format!("默认路由 via {r}")),
            None => add("uplink", "上行网络", "warn", "无默认路由（不影响本地投屏）".into()),
        }
        let host = self
            .origin
            .trim_start_matches("https://")
            .split('/')
            .next()
            .unwrap_or("")
            .to_owned();
        let lookup_target = if host.contains(':') { host.clone() } else { format!("{host}:443") };
        let resolved: Vec<std::net::IpAddr> = tokio::time::timeout(
            Duration::from_secs(3),
            tokio::net::lookup_host(lookup_target),
        )
        .await
        .ok()
        .and_then(|r| r.ok())
        .map(|it| it.map(|a| a.ip()).collect())
        .unwrap_or_default();
        let local = local_addresses();
        let ok = resolved.iter().any(|a| local.contains(a));
        let name = host.split(':').next().unwrap_or("");
        add(
            "dns",
            "DNS 解析",
            if ok { "ok" } else if resolved.is_empty() { "fail" } else { "warn" },
            if resolved.is_empty() {
                format!("{name} 无法解析")
            } else {
                format!(
                    "{name} → {}{}",
                    resolved.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(", "),
                    if ok { "（本机）" } else { "（不是本机地址）" }
                )
            },
        );
        match tls_info(&self.tls.lock().unwrap().0) {
            Ok((days, chain)) => add(
                "tls",
                "TLS 证书",
                if days < 0 { "fail" } else if days <= 14 { "warn" } else { "ok" },
                if days < 0 { format!("已过期 {} 天 · 链 {chain} 张", -days) } else { format!("剩 {days} 天 · 链 {chain} 张") },
            ),
            Err(e) => add("tls", "TLS 证书", "fail", e),
        }
        add("identity", "配件身份", "ok", "已加载（启动时校验私钥与证书匹配）".into());
        let video = self.hub.video_active();
        let viewer = self.hub.viewer_active();
        add(
            "video",
            "视频管线",
            if video && viewer { "ok" } else { "idle" },
            match (video, viewer) {
                (true, true) => format!("转发正常 · 队列 {} KB", self.hub.queue_bytes() / 1024),
                (true, false) => "收到视频，车机未连接查看".into(),
                (false, true) => "车机已连接，等待手机视频".into(),
                (false, false) => "空闲（手机未投屏）".into(),
            },
        );
        match disk_free("/overlay").or_else(|| disk_free("/")) {
            Some(v) => {
                let free = v["free_kb"].as_u64().unwrap_or(0);
                add("storage", "存储空间", if free < 5 * 1024 { "warn" } else { "ok" }, format!("/overlay 剩 {} MB", free / 1024))
            }
            None => add("storage", "存储空间", "unknown", "无法读取".into()),
        }
        match diag().caps() {
            Some(c) => {
                let h264 = c["h264"].as_bool().unwrap_or(false);
                add("browser", "车机浏览器", if h264 { "ok" } else { "warn" }, if h264 { format!("H.264 {}", c["h264_hw"].as_str().unwrap_or("解码可用")) } else { "不支持 WebCodecs H.264".into() });
                let fs = c["fullscreen"].as_bool().unwrap_or(false);
                add("fullscreen", "车机全屏 API", if fs { "ok" } else { "warn" }, if fs { "支持".into() } else { "不支持，已用沉浸模式".into() });
            }
            None => add("browser", "车机浏览器", "unknown", "车机尚未连接上报能力".into()),
        }
        let link = diag().link();
        let phone_live = self.hub.observation()["phone"].as_bool() == Some(true);
        match (&link.error, phone_live) {
            (Some(e), _) if diag::now_ms().saturating_sub(e.ts_ms) < 30 * 60_000 => add(
                "link",
                "最近一次手机连接",
                "fail",
                format!("{}（{}）· {}", e.title, step_name(e.step), e.hint),
            ),
            (_, true) => add("link", "最近一次手机连接", "ok", format!("CarPlay 会话进行中 · {}", link.carplay_detail)),
            _ if link.carplay == "ok" => add("link", "最近一次手机连接", "ok", link.carplay_detail.clone()),
            _ => add("link", "最近一次手机连接", "idle", "30 分钟内没有连接失败记录".into()),
        }
        add(
            "time",
            "系统时间",
            if diag::now_ms() > 1_735_689_600_000u64 { "ok" } else { "warn" },
            if diag::now_ms() > 1_735_689_600_000u64 { "时间合理（UTC）".into() } else { "时间明显偏早，可能未同步".into() },
        );
        let (storage_status, storage_text) = log_storage_check(self.prefs().log.usb, &diag().storage_status());
        add("log_storage", "日志持久化", storage_status, storage_text);
        let passed = checks.iter().filter(|c| c["status"] == "ok").count();
        let warned = checks.iter().filter(|c| c["status"] == "warn").count();
        let failed = checks.iter().filter(|c| c["status"] == "fail").count();
        let result = json!({"ts_ms": diag::now_ms(), "checks": checks, "passed": passed, "warned": warned, "failed": failed});
        f50_server::diag_log!(Info, "web", "selftest passed={passed} warn={warned} fail={failed}");
        *self.selftest.lock().unwrap() = Some(result.clone());
        result
    }
    pub fn config_text(&self) -> String {
        let prefs = self.prefs();
        let mut out = format!("# {} + 运行时设置 · 只读 · 密钥已隐藏\n", self.config_path.display());
        let config = std::fs::read(&self.config_path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .unwrap_or(Value::Null);
        render_toml(&mut out, "", &redact(config));
        out.push('\n');
        render_toml(&mut out, "runtime", &serde_json::to_value(&prefs).unwrap_or(Value::Null));
        out.push_str(&format!(
            "\n[runtime.effective_display]   # 下次手机连接时向 iPhone 声明\nwidth     = {}\nheight    = {}\nmax_fps   = {}\nwidth_mm  = {}\nhevc      = {}\nlog_level = \"{}\"\n",
            self.hub.display_spec().map_or(self.display.0, |s| s.width),
            self.hub.display_spec().map_or(self.display.1, |s| s.height),
            self.hub.display_spec().map_or(30, |s| s.fps),
            self.hub.display_spec().map_or(180, |s| s.width_mm),
            self.hub.display_spec().is_some_and(|s| s.hevc),
            if diag().verbose() { "debug" } else { "info" }
        ));
        diag::mask_macs(&out)
    }
    pub(super) async fn stored_logs(&self) -> Value {
        let prefs = self.prefs();
        if !prefs.log.usb { return json!({"enabled": false, "tail": "", "note": "持久日志未启用；RAM日志不会跨服务重启保留"}); }
        crate::persistent_logs::export(prefs.log.usb_path).await
    }
    pub async fn bundle(&self) -> Value {
        let (current, history) = diag().sessions();
        let logs = diag().logs(&LogQuery {
            limit: Some(5000),
            ..Default::default()
        });
        let cached = self.selftest.lock().unwrap().clone();
        let selftest = match cached {
            Some(v) => v,
            None => self.run_selftest().await,
        };
        let mut sessions: Vec<_> = current.into_iter().collect();
        sessions.extend(history.into_iter().take(3 - sessions.len().min(3)));
        let value = json!({
            "kind": "cowplay-diagnostics",
            "generated_ms": diag::now_ms(),
            "log_storage": diag().storage_status(),
            "stored_logs": self.stored_logs().await,
            "memory_log_export": {"truncated": logs.matched > logs.entries.len() || logs.evicted > 0, "evicted": logs.evicted, "oldest_seq": logs.oldest_seq, "last_seq": logs.last_seq},
            "diag_code": diag().diag_code(),
            "build": f50_server::build_info::BUILD_ID,
            "version": env!("CARGO_PKG_VERSION"),
            "redaction": "访问令牌、Wi‑Fi 密码、私钥与种子文件内容均不包含；MAC 地址中间两段已隐藏",
            "overview": self.overview(),
            "selftest": selftest,
            "sessions": sessions,
            "metrics": diag().metrics(),
            "process": self.process(),
            "system": self.system().await,
            "config": self.config_text(),
            "logs": logs.entries.iter().rev().map(diag::format_line).collect::<String>(),
        });
        let mut value = value;
        if let Some(p) = value.pointer_mut("/overview/pairing/passkey") {
            *p = Value::Null;
        }
        // Mask every MAC address in the serialized bundle, including protocol log text.
        serde_json::from_str(&diag::mask_macs(&value.to_string())).unwrap_or(value)
    }
}
