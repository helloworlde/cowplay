//! API for the car (`/car`) and phone (`/phone`, `/pair`) pages: one state summary, per-screen
//! preferences, remote commands, debug statistics and a diagnosis with a conclusion.
//! Values that cannot be measured on this device are `null` and shown as "—".
use crate::admin::{Admin, fail, json_ok, run_bounded};
use axum::{
    Router,
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::{get, post},
};
use f50_server::{
    clients::{Client, ScreenPrefs},
    diag::{self, diag},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

// ---------- small helpers ----------

fn client(a: &Admin, h: &HeaderMap, mutation: bool) -> Result<Client, Response> {
    let c = a.hub.clients.from_headers(h).ok_or_else(|| fail(StatusCode::UNAUTHORIZED, "这台设备还没有被授权"))?;
    if mutation && !f50_server::auth::same_origin(&a.hub, h) {
        return Err(fail(StatusCode::FORBIDDEN, "来源不一致"));
    }
    Ok(c)
}
/// `DA:1E:C4:••:••:16` from a BlueZ device path.
pub fn masked_bt(device: &str) -> String {
    let mac = device.rsplit("dev_").next().unwrap_or("").replace('_', ":");
    if mac.len() != 17 {
        return "—".into();
    }
    format!("{}:••:••:{}", &mac[..8], &mac[15..])
}

// ---------- phone usage history (last used per saved iPhone) ----------

/// Lazily loads `phones-used.json` into the admin state; writes it back only when `change` says so.
fn with_usage<R>(a: &Admin, change: impl FnOnce(&mut BTreeMap<String, u64>) -> (R, bool)) -> R {
    let path = a.state_dir.join("phones-used.json");
    let mut slot = a.phone_usage.lock().unwrap();
    let used = slot.get_or_insert_with(|| std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default());
    let (result, changed) = change(used);
    if changed {
        if let Ok(bytes) = serde_json::to_vec(used) {
            let _ = f50_server::settings::write_private(&path, &bytes);
        }
    }
    result
}
fn usage_touch(a: &Admin, device: &str) {
    let now = diag::now_ms();
    with_usage(a, |used| {
        let fresh = used.get(device).is_some_and(|t| now.saturating_sub(*t) < 60_000);
        if !fresh {
            used.insert(device.to_owned(), now);
        }
        ((), !fresh)
    })
}
fn usage_forget(a: &Admin, device: &str) {
    with_usage(a, |used| ((), used.remove(device).is_some()))
}
fn usage_last(a: &Admin, device: &str) -> Option<u64> {
    with_usage(a, |used| (used.get(device).copied(), false))
}

/// The phone in the live CarPlay session, if any.
pub fn current_phone(a: &Admin) -> Option<String> {
    let live = a.hub.has_phone_session() || a.hub.phone_verified.load(Ordering::Acquire);
    live.then(|| a.hub.phones.snapshot().device).flatten()
}

/// Limits our implementation can declare; iPhone support remains unknown until evidenced.
pub fn display_limits(a:&Admin)->Value {
    use f50_core::info::{MIN_WIDTH,MIN_HEIGHT,MAX_WIDTH,MAX_HEIGHT};
    json!({"receiver":{"min_width":MIN_WIDTH,"min_height":MIN_HEIGHT,"max_width":MAX_WIDTH,"max_height":MAX_HEIGHT,"dimension_step":2,"frame_rates":[30,60],"codecs":["h264","hevc"],"resolution_aliases":{"720p":"1280x720","1080p":"1920x1080","1440p":"2560x1440","2160p":"3840x2160"}},
      "iphone":{"ranges_known":false,"note":"当前协议没有返回此 iPhone 的完整支持范围；参数需由手机接受并实际返回画面。"},
      "requested":a.hub.display_spec(),
      // hub.dimensions are requested coordinates, not measured decoded media.
      "observed":Value::Null})
}
/// Saved iPhones shared by the car, phone and LuCI pages.
pub async fn phones(a: &Admin) -> Result<Vec<Value>, String> {
    let saved = a.manager.saved_phones().await.map_err(|e| e.to_string())?;
    Ok(phone_values(a, &saved))
}
fn phone_values(a: &Admin, saved: &[crate::bluetooth::SavedPhone]) -> Vec<Value> {
    let current = current_phone(a);
    if let Some(c) = &current {
        usage_touch(a, c);
    }
    let policy = a.policy();
    let preferred = if saved.iter().any(|p| p.id == policy.preferred) { Some(policy.preferred.clone()) } else { a.manager.selected_phone() };
    let mut list: Vec<Value> = saved
        .iter()
        .map(|p| {
            let is_current = current.as_deref() == Some(p.id.as_str());
            json!({
                "id": p.id,
                "name": p.name,
                "online": p.connected || is_current,
                "current": is_current,
                // The phone the F50 calls (or last called); the car names it while connecting.
                "selected": p.selected,
                "preferred": preferred.as_deref() == Some(p.id.as_str()),
                "bt": masked_bt(&p.id),
                "last_ms": if is_current { Some(diag::now_ms()) } else { usage_last(a, &p.id) },
            })
        })
        .collect();
    list.sort_by(|x, y| {
        let key = |v: &Value| (!v["current"].as_bool().unwrap_or(false), !v["preferred"].as_bool().unwrap_or(false), std::cmp::Reverse(v["last_ms"].as_u64().unwrap_or(0)));
        key(x).cmp(&key(y))
    });
    list
}

// ---------- Wi-Fi and station sampling (every 5 s from Admin::sample) ----------

fn parse_station_dump(text: &str) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    let mut mac = String::new();
    let mut cur = serde_json::Map::new();
    let flush = |mac: &str, cur: &mut serde_json::Map<String, Value>, out: &mut Vec<(String, Value)>| {
        if !mac.is_empty() {
            out.push((mac.to_owned(), Value::Object(std::mem::take(cur))));
        }
    };
    for l in text.lines() {
        let t = l.trim();
        if let Some(rest) = t.strip_prefix("Station ") {
            flush(&mac, &mut cur, &mut out);
            mac = rest.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        } else if let Some((k, v)) = t.split_once(':') {
            let v = v.trim();
            let first = v.split_whitespace().next().unwrap_or("");
            match k.trim() {
                "signal" => {
                    cur.insert("signal".into(), json!(first.parse::<i32>().ok()));
                }
                "tx bitrate" => {
                    cur.insert("rate_mbps".into(), json!(first.parse::<f64>().ok()));
                }
                "tx retries" => {
                    cur.insert("tx_retries".into(), json!(first.parse::<u64>().ok()));
                }
                "tx packets" => {
                    cur.insert("tx_packets".into(), json!(first.parse::<u64>().ok()));
                }
                _ => {}
            }
        }
    }
    flush(&mac, &mut cur, &mut out);
    out
}
/// MAC of an IPv4 neighbour from `/proc/net/arp` (complete entries only).
fn arp_mac(arp: &str, ip: &str) -> Option<String> {
    arp.lines().skip(1).find_map(|l| {
        let f: Vec<_> = l.split_whitespace().collect();
        (f.len() >= 4 && f[0] == ip && f[2] != "0x0" && f[3] != "00:00:00:00:00:00").then(|| f[3].to_ascii_lowercase())
    })
}
/// The iPhone's Wi‑Fi MAC: kernel neighbour table first (any DHCP server, static or private
/// addresses), then dnsmasq leases.
pub(crate) fn peer_mac(ip: &str) -> Option<String> {
    std::fs::read_to_string("/proc/net/arp").ok().and_then(|t| arp_mac(&t, ip)).or_else(|| {
        std::fs::read_to_string("/tmp/dhcp.leases").ok().and_then(|t| {
            t.lines().find_map(|l| {
                let f: Vec<_> = l.split_whitespace().collect();
                (f.len() >= 3 && f[2] == ip).then(|| f[1].to_ascii_lowercase())
            })
        })
    })
}
fn parse_iw_info(text: &str) -> Value {
    let mut ssid = None;
    let mut channel = None;
    let mut width = None;
    let mut ap = false;
    for l in text.lines().map(str::trim) {
        if let Some(s) = l.strip_prefix("ssid ") {
            ssid = Some(s.to_owned());
        } else if let Some(rest) = l.strip_prefix("channel ") {
            channel = rest.split_whitespace().next().and_then(|v| v.parse::<u32>().ok());
            width = rest.split("width:").nth(1).and_then(|w| w.split_whitespace().next()).and_then(|v| v.parse::<u32>().ok());
        } else if l == "type AP" {
            ap = true;
        }
    }
    json!({"ssid": ssid, "channel": channel, "width_mhz": width, "ap": ap})
}
fn wifi_interface(ifaces: &[(String,String)], preferred: &str, ssid: Option<&str>) -> Option<String> {
    let ssid = ssid?;
    ifaces.iter().find(|(i,s)| i==preferred && s==ssid)
        .or_else(||ifaces.iter().find(|(_,s)|s==ssid)).map(|(i,_)|i.clone())
}
pub async fn sample_wifi(a: &Admin) {
    let peer = diag().peer();
    if a.sys.lock().unwrap().wifi.as_ref().is_some_and(|(v, t)| t.elapsed() < Duration::from_secs(5) && v["peer_ip"].as_str() == peer.as_deref()) {
        return;
    }
    let policy = a.policy();
    let ifaces = crate::admin::iw_interfaces().await;
    let wanted_ssid = a.wifi.as_ref().map(|(s, _)| s.clone());
    let iface = wifi_interface(&ifaces,&policy.wifi_iface,wanted_ssid.as_deref());
    let mut value = json!({"iface": iface, "ssid": if iface.is_some() {wanted_ssid.clone()}else{None}, "channel": null, "configured_ssid":wanted_ssid,"configured_channel":a.wifi.as_ref().map(|w|w.1),"source":"iw dev / station dump; only matching CarPlay SSID", "width_mhz": null, "clients": null, "phone": null,"peer_ip":peer,"phone_status":"no_interface"});
    if let Some(i) = &iface {
        value["phone_status"] = json!("query_failed");
        if let Some(info) = run_bounded("iw", &["dev", i, "info"]).await {
            let v = parse_iw_info(&info);
            for k in ["ssid", "channel", "width_mhz"] {
                if !v[k].is_null() {
                    value[k] = v[k].clone();
                }
            }
        }
        if let Some(dump) = run_bounded("iw", &["dev", i, "station", "dump"]).await {
            let stations = parse_station_dump(&dump);
            // iw command errors are text too: do not turn a failure into "0 clients".
            if !stations.is_empty() || dump.trim().is_empty() { value["clients"] = json!(stations.len()); }
            let phone_mac = peer.as_ref().and_then(|ip| peer_mac(ip));
            value["phone_status"] = json!(station_status(&stations, phone_mac.as_deref(), peer.is_some(), !dump.trim().is_empty()));
            // No MAC means the iPhone is not identified on this interface: leave its link unknown.
            if let Some((_, st)) = phone_mac.and_then(|m| stations.into_iter().find(|(mac, _)| *mac == m)) {
                let retry = match (st["tx_retries"].as_u64(), st["tx_packets"].as_u64()) {
                    (Some(r), Some(p)) if p > 0 => Some(r as f64 * 100.0 / (p + r) as f64),
                    _ => None,
                };
                // Some vendor drivers (the F50's) list the station but report no signal or rates.
                value["phone"] = json!({"signal": st["signal"], "rate_mbps": st["rate_mbps"], "retry_pct": retry, "associated": true});
            }
        }
    }
    a.sys.lock().unwrap().wifi = Some((value, Instant::now()));
    if let Some(c) = current_phone(a) {
        usage_touch(a, &c);
    }
}
fn station_status(stations: &[(String,Value)], mac: Option<&str>, connected: bool, nonempty: bool) -> &'static str {
    if !connected { return "disconnected"; }
    if stations.is_empty() && nonempty { return "query_failed"; }
    let Some(mac) = mac else { return "unidentified"; };
    match stations.iter().find(|(m,_)| m==mac) {
        Some((_,s)) if s["signal"].is_number() && s["rate_mbps"].is_number() => "available",
        Some((_,s)) if s["signal"].is_number() || s["rate_mbps"].is_number() => "partial",
        Some(_) => "driver_unavailable",
        None => "station_missing",
    }
}

fn station_for_peer(value:&Value,age:Duration,peer:Option<&str>)->Value{
    let mut v=value.clone();
    v["sample_age_ms"]=json!(age.as_millis() as u64);
    if age>Duration::from_secs(15)||v["peer_ip"].as_str()!=peer {v["phone"]=Value::Null;v["phone_status"]=json!("stale");}
    v
}
fn phone_for_peer(value:&Value,peer:Option<&str>)->Value{
    if peer.is_some()&&value["ip"].as_str()==peer {value.clone()}else{Value::Null}
}

// ---------- statistics ----------

/// Everything the debug overlay and diagnosis need, one second fresh.
pub fn stats(a: &Admin) -> Value {
    let m = diag().metrics();
    let system = &m["system"];
    let peer = diag().peer();
    let wifi = a.sys.lock().unwrap().wifi.as_ref().map(|(v,t)|station_for_peer(v,t.elapsed(),peer.as_deref())).unwrap_or(Value::Null);
    let sample = a.hub.viewer_client().and_then(|id| diag().viewer_sample_for(&id, a.hub.viewer_generation()));
    let report_age = sample.as_ref().map(|(age, _)| age.as_millis() as u64);
    let last_report = sample.as_ref().and_then(|(_, report)| serde_json::to_value(report).ok()).unwrap_or(Value::Null);
    let report = sample.as_ref().filter(|(age, _)| *age < Duration::from_secs(5))
        .and_then(|(_, report)| serde_json::to_value(report).ok()).unwrap_or(Value::Null);
    let spec = a.hub.display_spec();
    let (session, _) = diag().sessions();
    let mem_free = system["mem_available_kb"].as_u64().map(|k| k / 1024);
    let mem_total = system["mem_total_kb"].as_u64().map(|k| k / 1024);
    let phone = phone_for_peer(&system["phone"],peer.as_deref());
    let stalls_10m = diag().recent(100).iter().filter(|e| e.msg.starts_with("画面停住") && diag::now_ms().saturating_sub(e.ts_ms) < 600_000).count();
    json!({
        "ts_ms": diag::now_ms(),
        "diag_code": diag().diag_code(),
        "session": session.as_ref().map(|s| json!({"id": s.id, "started_ms": s.started_ms, "phone": s.phone, "first_frame_ms": s.first_frame_ms, "model": s.model, "os": s.os})),
        "spec": spec,
        "server": m["server"],
        "car": report,
        "car_last_report": last_report,
        "car_sample_age_ms": report_age,
        "history": m["history"],
        "stalls_10m": stalls_10m,
        "f50": {
            "sample_age_ms": m["system_age_ms"],
            "cpu": system["cpu_percent"],
            "temp": system["temp_max_c"],
            "mem_free_mb": mem_free,
            "mem_total_mb": mem_total,
            "wifi": wifi,
            "uplink": crate::admin::default_route().unwrap_or_else(|| "无默认路由".into()),
        },
        "phone": {
            "ip": phone["ip"],
            "name": phone["name"],
            "rssi": phone["rssi_dbm"].as_i64().or(wifi["phone"]["signal"].as_i64()),
            "rate_mbps": wifi["phone"]["rate_mbps"],
            "retry_pct": wifi["phone"]["retry_pct"],
            "associated": wifi["phone"]["associated"].as_bool(),
            "bt": if a.manager.status().rfcomm { Some("iAP2") } else { None },
            "wifi_status": wifi["phone_status"],
            "model": session.as_ref().and_then(|s| s.model.clone()),
            "ios": session.as_ref().and_then(|s| s.os.clone()),
        },
    })
}

// ---------- diagnosis (server computes, three pages only display) ----------

struct Check {
    group: &'static str,
    name: &'static str,
    value: Option<f64>,
    text: String,
    limit: f64,
    /// true: higher is worse; false: lower is worse.
    upper: bool,
    unit: &'static str,
}
impl Check {
    fn bad(&self) -> bool {
        match self.value {
            Some(v) if self.upper => v > self.limit,
            Some(v) => v < self.limit,
            None => false,
        }
    }
    /// Bar fill where the normal limit sits in the middle.
    fn ratio(&self) -> f64 {
        match self.value {
            Some(v) if self.upper => (v / self.limit / 2.0).clamp(0.0, 1.0),
            Some(v) => {
                // dBm and fps: distance below the limit fills the bar past the middle.
                if self.unit == "dBm" { ((-v - 30.0) / ((-self.limit - 30.0) * 2.0)).clamp(0.0, 1.0) } else { (self.limit / v.max(0.1) / 2.0).clamp(0.0, 1.0) }
            }
            None => 0.0,
        }
    }
}
fn fmt(v: Option<f64>, digits: usize, unit: &str) -> String {
    match v {
        Some(v) => format!("{v:.digits$} {unit}").trim().to_owned(),
        None => "—".into(),
    }
}
pub fn diagnosis(a: &Admin) -> Value {
    let quality = a.viewer().map(|v| v.prefs.quality).unwrap_or_else(|| a.policy().defaults.quality);
    let connected = a.hub.has_phone_session() && a.hub.phone_verified.load(std::sync::atomic::Ordering::Acquire) && a.viewer().is_some();
    let mut result = diagnosis_samples(&stats(a), &quality, &diag().link(), connected);
    let events: Vec<Value> = diag().recent(12).into_iter()
        .filter(|e| e.kind != "info" || !e.detail.is_empty()).take(6)
        .map(|e| json!({"level": e.kind, "title": e.msg, "cause": e.detail, "ts_ms": e.ts_ms})).collect();
    result["events"] = json!(events);
    result["code"] = json!(diag().diag_code());
    result["context"] = connection_context(a);
    result["selftest"] = json!(a.current_selftest());
    result
}

fn diagnosis_samples(s: &Value, quality: &str, link: &f50_server::diag::Link, connected: bool) -> Value {
    let car = if s["car"]["visible"] == false { &Value::Null } else { &s["car"] };
    let f50 = &s["f50"];
    let phone = &s["phone"];
    let incoming = s["server"]["fps_in"].as_f64().filter(|f| f.is_finite() && *f > 0.0);
    let streaming = connected && incoming.is_some() && car["fps"].as_f64().is_some_and(|f| f.is_finite() && f >= 0.0);
    let cap = s["spec"]["fps"].as_f64().filter(|f| f.is_finite() && *f > 0.0).unwrap_or(if quality == "smooth" { 30.0 } else { 60.0 });
    let target_fps = incoming.unwrap_or(cap).min(cap);
    let stalls = s["stalls_10m"].as_u64().unwrap_or(0) as f64;
    let checks = vec![
        Check { group: "车机 ⟷ 接收端", name: "延迟", value: car["rtt_ms"].as_f64(), text: fmt(car["rtt_ms"].as_f64(), 0, "ms"), limit: 30.0, upper: true, unit: "ms" },
        Check { group: "车机 ⟷ 接收端", name: "抖动", value: car["jitter_ms"].as_f64(), text: fmt(car["jitter_ms"].as_f64(), 0, "ms"), limit: 10.0, upper: true, unit: "ms" },
        Check { group: "车机 ⟷ 接收端", name: "丢包", value: car["loss_pct"].as_f64(), text: fmt(car["loss_pct"].as_f64(), 1, "%"), limit: 1.0, upper: true, unit: "%" },
        Check { group: "接收端 ⟷ iPhone", name: "信号", value: phone["rssi"].as_f64(), text: phone["rssi"].as_f64().map(|v| format!("{} dBm", v.round()).replace('-', "−")).unwrap_or("—".into()), limit: -70.0, upper: false, unit: "dBm" },
        Check { group: "接收端 ⟷ iPhone", name: "速率", value: phone["rate_mbps"].as_f64(), text: fmt(phone["rate_mbps"].as_f64(), 0, "Mbps"), limit: 0.0, upper: false, unit: "Mbps" },
        Check { group: "接收端 ⟷ iPhone", name: "重传", value: phone["retry_pct"].as_f64(), text: fmt(phone["retry_pct"].as_f64(), 1, "%"), limit: 5.0, upper: true, unit: "%" },
        Check { group: "画面", name: "帧率", value: car["fps"].as_f64().filter(|_| streaming), text: fmt(car["fps"].as_f64().filter(|_| streaming), 1, "fps"), limit: (target_fps - 5.0).max(0.0), upper: false, unit: "fps" },
        Check { group: "画面", name: "解码", value: car["decode_ms"].as_f64().filter(|_| streaming), text: fmt(car["decode_ms"].as_f64().filter(|_| streaming), 1, "ms"), limit: 12.0, upper: true, unit: "ms" },
        Check { group: "画面", name: "停住", value: Some(stalls), text: if stalls > 0.0 { format!("{stalls} 次/10 分") } else { "0 次".into() }, limit: 0.5, upper: true, unit: "次" },
        Check { group: "接收端", name: "CPU", value: f50["cpu"].as_f64(), text: fmt(f50["cpu"].as_f64(), 0, "%"), limit: 70.0, upper: true, unit: "%" },
        Check { group: "接收端", name: "温度", value: f50["temp"].as_f64(), text: fmt(f50["temp"].as_f64(), 0, "°C"), limit: 75.0, upper: true, unit: "°C" },
    ];
    let rate_ratio = |c: &Check| if c.name == "速率" { c.value.map(|v| (200.0 / v.max(1.0)).clamp(0.05, 1.0) * 0.5).unwrap_or(0.0) } else { c.ratio() };
    let frame_reference = if streaming { format!("≥ {:.1} fps（输入 {:.1}，上限 {:.1}）", (target_fps - 5.0).max(0.0), incoming.unwrap_or(0.0), cap) } else { "跟随源画面更新".into() };
    let checks_json: Vec<Value> = checks
        .iter()
        .map(|c| {
            let limit = match c.name {
                "延迟" => "< 30 ms",
                "抖动" => "< 10 ms",
                "丢包" => "< 1 %",
                "信号" => "> −70 dBm",
                "速率" => "—",
                "重传" => "< 5 %",
                "帧率" => frame_reference.as_str(),
                "解码" => "< 12 ms",
                "停住" => "0 次",
                "CPU" => "< 70 %",
                _ => "< 75 °C",
            };
            json!({"group": c.group, "name": c.name, "value": c.value, "text": c.text, "limit": limit, "ratio": rate_ratio(c), "ok": c.value.map(|_| !c.bad()), "known": c.value.is_some()})
        })
        .collect();
    let jitter = checks[1].value;
    let fix = |label: &str| json!({"kind": "smooth", "label": label});
    let verdict = if let Some(e) = link.error.as_ref().filter(|e| diag::now_ms().saturating_sub(e.ts_ms) < 30 * 60_000) {
        json!({"level": "error", "title": e.title, "detail": e.hint, "action": null})
    } else if checks[1].bad() {
        json!({"level": "warn", "title": "车机到接收端的往返响应波动较大", "detail": format!("抖动 {}（参考 < 10 ms）{}。请结合动态画面、请求超时和设备负载判断是否影响使用。", checks[1].text, if stalls > 0.0 { format!("，最近 10 分钟记录 {stalls} 次绘制恢复") } else { String::new() }), "action": null})
    } else if checks[2].bad() {
        json!({"level": "warn", "title": "车机到接收端的网络丢包偏多", "detail": format!("丢包 {}（正常 < 1 %）。请检查当前网络连接及两端设备负载。", checks[2].text), "action": if quality != "smooth" { fix("切到流畅（30 帧）") } else { Value::Null }})
    } else if checks[3].bad() || checks[5].bad() {
        json!({"level": "warn", "title": "iPhone 到接收端的 Wi‑Fi 信号偏弱", "detail": format!("信号 {} · 重传 {}（正常 > −70 dBm、< 5 %）。让 iPhone 靠近接收端。", checks[3].text, checks[5].text), "action": if quality != "smooth" { fix("切到流畅（30 帧）") } else { Value::Null }})
    } else if checks[9].bad() || checks[10].bad() {
        json!({"level": "warn", "title": "接收端负载偏高", "detail": format!("CPU {} · 温度 {}（正常 < 70 %、< 75 °C）。注意通风，或先切到流畅。", checks[9].text, checks[10].text), "action": if quality != "smooth" { fix("切到流畅（30 帧）") } else { Value::Null }})
    } else if checks[6].bad() || checks[7].bad() {
        json!({"level": "warn", "title": "车机画面不够流畅", "detail": format!("帧率 {} · 解码 {}。车机浏览器解码偏慢时，流畅（30 帧）更稳。", checks[6].text, checks[7].text), "action": if quality != "smooth" { fix("切到流畅（30 帧）") } else { Value::Null }})
    } else if checks[8].bad() {
        json!({"level": "warn", "title": "最近画面停住过", "detail": format!("10 分钟内停住 {stalls} 次，均已自动恢复。"), "action": if quality != "smooth" { fix("切到流畅（30 帧）") } else { Value::Null }})
    } else if streaming && checks.iter().any(|c| c.value.is_none()) {
        json!({"level": "unknown", "title": "已观测到画面，部分链路指标未知", "detail": "当前已上报解码画面；缺少的网络或设备指标不能判为正常。请结合原始数据与现场表现排查。", "action": null})
    } else if streaming && quality == "smooth" {
        json!({"level": "ok", "title": "当前为流畅画质", "detail": format!("当前画面上限 {cap:.0} fps，往返抖动{}。可在设置中调整画质。", jitter.map(|j| format!(" {} ms", j.round())).unwrap_or_else(|| "未知".into())), "action": {"kind": "auto", "label": "改回自动"}})
    } else if streaming {
        json!({"level": "ok", "title": "当前采样未见异常", "detail": "已测指标处于参考范围；短时采样不能替代长期与现场验收。", "action": null})
    } else if connected {
        json!({"level": "ok", "title": "已连接，等待画面更新", "detail": "当前没有持续的新画面输入，暂不据此判断帧率或卡顿。", "action": null})
    } else {
        json!({"level": "ok", "title": "还没有在投屏", "detail": "连上 iPhone 后这里会给出三段链路的体检结论。", "action": null})
    };
    let problems = checks.iter().filter(|c| c.bad()).count() + usize::from(verdict["level"] == "error");
    json!({"verdict": verdict, "checks": checks_json, "problems": problems})
}

// ---------- step-by-step self test ----------

async fn ping_ms(ip: &str) -> Option<f64> {
    if ip.parse::<std::net::IpAddr>().is_err() {
        return None;
    }
    let out = run_bounded("ping", &["-c", "3", "-W", "1", "-q", ip]).await?;
    let line = out.lines().find(|l| l.contains("min/avg/max"))?;
    line.split('=').nth(1)?.trim().split('/').nth(1)?.trim().parse().ok()
}
/// A result is meaningful only for the same phone session and authorized screen.
pub(crate) fn connection_context(a: &Admin) -> Value {
    let (session, _) = diag().sessions();
    json!({"session":session.as_ref().map(|s|&s.id),"viewer":a.viewer().map(|v|v.public()["id"].clone()),"viewer_generation":a.hub.viewer_generation(),"phone":current_phone(a).map(|p|masked_bt(&p)),"epoch":a.hub.phones.epoch()})
}
/// 屏幕 → 接收端 → iPhone → 出画面 → 解码与声音. The car passes its own measured round trip.
pub async fn segments(a: &Admin, car_rtt: Option<f64>) -> Value {
    let started = Instant::now();
    let context = connection_context(a);
    let s = stats(a);
    let car_ms = car_rtt.or(s["car"]["rtt_ms"].as_f64());
    let phone_ip = s["phone"]["ip"].as_str().map(str::to_owned);
    let protocol_rtt = s["server"]["phone_rtt_ms"].as_f64();
    let phone_ms = match (protocol_rtt, &phone_ip) {
        (Some(ms), _) => Some(ms),
        (None, Some(ip)) => ping_ms(ip).await,
        _ => None,
    };
    let frame_ms = if a.hub.video_active() {
        let before = diag().metrics()["server"]["frames_in"].as_u64().unwrap_or(0);
        let t = Instant::now();
        let _ = a.hub.control.try_send(f50_server::Control::Keyframe);
        let mut got = None;
        while t.elapsed() < Duration::from_secs(3) {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if diag().metrics()["server"]["frames_in"].as_u64().unwrap_or(0) > before {
                got = Some(t.elapsed().as_millis() as f64);
                break;
            }
        }
        got
    } else {
        None
    };
    let latest = stats(a);
    let mut list = segment_observations(&latest, car_ms, phone_ms, protocol_rtt.is_some(), frame_ms, a.hub.video_active());
    let same = context == connection_context(a);
    if !same {
        for step in &mut list { step["state"]=json!("unknown");step["ok"]=json!(false);step["text"]=json!("检查期间连接设备已变化，请在当前设备连接稳定后重新检查"); }
    }
    let passed = list.iter().filter(|v| v["ok"] == true).count();
    let skipped = list.iter().filter(|v| v["state"] == "skipped").count();
    let unknown = list.iter().filter(|v| v["state"] == "unknown").count();
    let result = json!({"ts_ms": diag::now_ms(), "total":list.len(), "steps": list, "passed": passed, "unknown":unknown, "skipped":skipped, "ms": started.elapsed().as_millis() as u64,"context":context,"context_current":same});
    if same { *a.connection_check.lock().unwrap()=Some(result.clone()); }
    diag().event_detail("info", "web", "连接检查完成", format!("用时 {:.1} 秒；{passed} 项已观测，{skipped} 项未测量，{unknown} 项因连接变化失效", started.elapsed().as_secs_f64()));
    result
}

fn segment_observations(s: &Value, car_ms: Option<f64>, phone_ms: Option<f64>, protocol_rtt: bool, frame_ms: Option<f64>, video_active: bool) -> Vec<Value> {
    let car = &s["car"];
    let step = |name: &str, state: &str, text: String| json!({"step":name,"state":state,"ok":state=="ok","text":text});
    let painted = car["decoded"].as_u64().is_some_and(|n| n > 0) && car["width"].as_u64().is_some_and(|n| n > 0) && car["height"].as_u64().is_some_and(|n| n > 0);
    let audio = car["audio_rate"].as_u64().filter(|n| *n > 0);
    vec![
        step("屏幕 → 接收端", if car_ms.is_some(){"ok"}else{"skipped"}, car_ms.map(|v|format!("{v:.1} ms（当前浏览器上报）")).unwrap_or("当前屏幕没有新鲜上报；请保持投屏页在前台".into())),
        step("接收端 → iPhone", if phone_ms.is_some(){"ok"}else{"skipped"}, phone_ms.map(|v|format!("{v:.1} ms（{}）",if protocol_rtt{"AirPlay 协议探测"}else{"ICMP 探测"})).unwrap_or(if s["phone"]["ip"].is_string(){"无协议 RTT，ICMP 无响应；不能据此判定 CarPlay 失败".into()}else{"当前没有可探测的 iPhone 地址".into()})),
        step("iPhone 新视频帧", if frame_ms.is_some(){"ok"}else{"skipped"},frame_ms.map(|v|format!("{v:.0} ms（请求后收到新帧）")).unwrap_or(if video_active{"3 秒内无新帧；静止源可能不送帧，本项不能判定失败".into()}else{"当前无视频流，未请求关键帧".into()})),
        step("浏览器实际绘制",if painted{"ok"}else{"skipped"},if painted{format!("{}×{} · 已绘制 {} 帧；当前 {}，解码 {}（静止源可无新值）",car["width"],car["height"],car["decoded"],fmt(car["fps"].as_f64(),1,"fps"),fmt(car["decode_ms"].as_f64(),1,"ms"))}else{"当前查看通道尚无有效绘制尺寸和帧数".into()}),
        step("浏览器音频格式",if audio.is_some(){"ok"}else{"skipped"},audio.map(|rate|format!("{rate} Hz · {} 声道 · {}；已收到格式声明，不代表收到音频包；实际声音输出需现场确认",car["audio_channels"],car["audio_codec"].as_str().unwrap_or("格式未上报"))).unwrap_or("浏览器未上报音频格式；实际声音输出需现场确认".into())),
    ]
}

#[cfg(test)]
mod observation_tests {
    use super::*;
    #[test]
    fn static_rendered_picture_and_protocol_rtt_are_useful_without_inventing_audio() {
        let s=json!({"car":{"decoded":42,"width":1920,"height":912,"fps":0},"phone":{"ip":"192.0.2.1"}});
        let steps=segment_observations(&s,Some(2.0),Some(4.0),true,None,true);
        assert_eq!(steps[1]["state"],"ok");assert!(steps[1]["text"].as_str().unwrap().contains("AirPlay"));
        assert_eq!(steps[2]["state"],"skipped","a static source is not a failed connection");
        assert_eq!(steps[3]["state"],"ok");assert!(steps[3]["text"].as_str().unwrap().contains("1920×912"));
        assert_eq!(steps[4]["state"],"skipped");assert!(steps[4]["text"].as_str().unwrap().contains("未上报音频格式"));
    }
    #[test]
    fn missing_data_has_a_reason_and_audio_format_does_not_claim_sound_output() {
        let empty=segment_observations(&json!({}),None,None,false,None,false);
        assert!(empty.iter().all(|s|s["state"]=="skipped"&&!s["text"].as_str().unwrap().is_empty()));
        let audio=segment_observations(&json!({"car":{"audio_rate":48000,"audio_channels":2,"audio_codec":"aac"}}),None,None,false,None,false);
        assert_eq!(audio[4]["state"],"ok");assert!(audio[4]["text"].as_str().unwrap().contains("实际声音输出需现场确认"));
    }
}

// ---------- state summary ----------

pub fn network(a: &Admin) -> Value {
    let policy = a.policy();
    let cert = crate::admin::tls_info(&a.tls.lock().unwrap().0).ok();
    let ddns = a.ddns_state.lock().unwrap().clone();
    let lan = a.web_bind.ip();
    let lan = if lan.is_unspecified() { crate::admin::local_addresses().into_iter().find(|i| i.is_ipv4() && !i.is_loopback()).map(|i| i.to_string()) } else { Some(lan.to_string()) };
    let host = a.origin.trim_start_matches("https://").to_owned();
    json!({
        "ssid": a.wifi.as_ref().map(|w| w.0.clone()),
        "channel": a.wifi.as_ref().map(|w| w.1),
        "origin": a.origin,
        "host": host,
        "lan_ip": lan,
        "platform": if a.manager.private_runtime() { "f50" } else if a.openwrt { "openwrt" } else { "linux" },
        "receiver_name": a.manager.advertised_name(),
        "device_name": crate::receiver_name::local_device_name(),
        "luci": if a.openwrt { lan.as_ref().map(|ip| format!("http://{ip}/cgi-bin/luci/admin/services/cowplay")) } else { Some(format!("{}/admin", a.origin)) },
        "management": a.management_enabled.load(std::sync::atomic::Ordering::Acquire).then(|| format!("{}/manage", a.origin)),
        "cert_days": cert.map(|c| c.0),
        "cert_source": policy.cert_source,
        "ddns": {"enabled": policy.ddns_enabled && policy.ddns_mode=="builtin", "mode":policy.ddns_mode, "provider": policy.ddns_provider, "last_ms": ddns.last_update_ms, "ok": ddns.update_ok},
    })
}
fn wifi_qr(a: &Admin) -> Option<String> {
    let (ssid, _) = a.wifi.as_ref()?;
    let password = a.wifi_password()?;
    let esc = |s: &str| s.chars().flat_map(|c| if matches!(c, '\\' | ';' | ',' | ':' | '"') { vec!['\\', c] } else { vec![c] }).collect::<String>();
    Some(format!("WIFI:T:WPA;S:{};P:{};;", esc(ssid), esc(&password)))
}
pub async fn state(a: &Admin, me: &Client) -> Value {
    let policy = a.policy();
    let (devices,adapter)=tokio::join!(a.manager.device_lists(),a.manager.adapter_info());
    let devices=devices.map_err(|e|e.to_string());
    let adapter_error=adapter.as_ref().err().map(|e|e.to_string());
    let adapter=adapter.ok();
    let phones = devices.as_ref().map(|(saved, _)| phone_values(a, saved)).map_err(Clone::clone);
    let nearby = devices.as_ref().map(|(_, nearby)| nearby.as_slice()).unwrap_or(&[]);
    let pairing = a.manager.status();
    let viewer = a.viewer();
    let hub = a.hub.observation();
    let link = diag().link();
    let (session, _) = diag().sessions();
    let d = diagnosis(a);
    let screens: Vec<Value> = a.hub.clients.list().iter().filter(|c| c.kind == "screen").map(|c| {
        let mut v = c.public();
        v["viewing"] = json!(viewer.as_ref().is_some_and(|x| x.id == c.id));
        v
    }).collect();
    let current = current_phone(a);
    let pairing_name = match &pairing.pairing_device {
        Some(dev) => phones.as_ref().ok().and_then(|l| l.iter().find(|p| p["id"] == dev.as_str()).and_then(|p| p["name"].as_str().map(str::to_owned)))
            .or_else(|| nearby.iter().find(|d| d.id == *dev).map(|d| d.name.clone())),
        None => None,
    };
    json!({
        "now_ms": diag::now_ms(),
        "version": env!("CARGO_PKG_VERSION"),
        "build": f50_server::build_info::BUILD_ID,
        "uptime_s": diag().started.elapsed().as_secs(),
        "vehicle": a.hub.vehicle.snapshot(),
        "diag_code": diag().diag_code(),
        "me": {"id": me.handle(), "kind": me.kind, "name": me.name, "prefs": me.prefs, "window": me.window, "viewing": viewer.as_ref().is_some_and(|v| v.id == me.id)},
        "viewer": viewer.as_ref().map(|v| json!({"id": v.handle(), "name": v.name, "ua": v.ua, "ip": v.ip, "prefs": v.prefs, "window": v.window})),
        "screens": screens,
        "phones": phones.as_ref().ok(),
        "phones_error": phones.as_ref().err(),
        "current_phone": current,
        "stopped": a.user_stopped(),
        "handoff": handoff(pairing.rfcomm, a.hub.has_phone_session(), &link, diag::now_ms()),
        "pairing": {"phase": pairing.phase, "generation": pairing.generation, "request": pairing.request, "open": pairing.remaining_s.is_some(), "remaining_s": pairing.remaining_s, "passkey": pairing.passkey, "device": pairing.pairing_device, "name": pairing_name, "paired": pairing.paired, "discovering": pairing.discovering, "adapter_name": adapter.as_ref().map(|a|&a.controller_name), "adapter":adapter, "adapter_error":adapter_error, "pairing_error": pairing.pairing_error, "nearby": nearby},
        "link": link,
        "hub": hub,
        "spec": a.hub.display_spec(),
        "automatic_fps_limit": a.auto_fps_limit(),

        "display_limits":display_limits(a),
        "session": session.as_ref().map(crate::admin::summary),
        "policy": {"ask_when_multi": policy.ask_when_multi, "ask_timeout": policy.ask_timeout, "stall_ms": policy.stall_ms, "retries": policy.retries, "stall_card": policy.stall_card, "token_required": policy.token_required, "max_fps": policy.max_fps, "max_res": policy.max_res},
        "network": network(a),
        "wifi_qr": if me.kind == "screen" { wifi_qr(a) } else { None },
        "diag": {"level": d["verdict"]["level"], "problems": d["problems"], "title": d["verdict"]["title"]},
    })
}

// ---------- commands ----------

/// An iPhone is between Bluetooth and CarPlay: iAP2 is up, no session yet, a hand-off step is active
/// and nothing failed (e.g. it waits at “使用 CarPlay？”). A default re-invite would tear that down.
/// Bounded, and false once a session ended (iAP2 can stay up), so recovery is never blocked for good.
fn handoff(rfcomm: bool, session: bool, l: &diag::Link, now_ms: u64) -> bool {
    rfcomm && !session && l.error.is_none() && (l.bluetooth == "active" || l.wifi == "active") && now_ms.saturating_sub(l.updated_ms) < 120_000
}
pub(crate) fn handoff_now(a: &Admin) -> bool {
    handoff(a.manager.status().rfcomm, a.hub.has_phone_session(), &diag().link(), diag::now_ms())
}

/// End the live session and invite the chosen phone again (same path as a settings change).
async fn reconnect(a: &Arc<Admin>, device: Option<String>, automatic_epoch: Option<u64>) -> Result<(), String> {
    if automatic_epoch.is_some_and(|epoch| epoch != a.hub.phones.epoch() || a.stopped.load(Ordering::Acquire)) {
        return Ok(());
    }
    if let Some(epoch) = automatic_epoch {
        if a.hub.has_phone_session() {
            a.automatic.reinvite_current(a, epoch);
            return Ok(());
        }
    }
    let device = device.or_else(|| current_phone(a)).or_else(|| a.manager.fresh_pairing()).or_else(|| {
        let p = a.policy().preferred;
        (!p.is_empty()).then_some(p)
    });
    if a.hub.has_phone_session() && current_phone(a) == device {
        a.prepare_display();
        a.reinvite("用户请求重新连接 CarPlay");
        return Ok(());
    }
    match automatic_epoch {
        Some(epoch) => {
            if let Some(device) = device.or_else(|| a.manager.selected_phone()) { a.automatic.request(a, device, epoch); }
            Ok(())
        },
        None => crate::phone_flow::connect(&a.manager, &a.hub, device.as_deref()).await,
    }.map_err(|e| e.to_string())
}
async fn forget(a: &Arc<Admin>, device: &str) -> Result<(), String> {
    if current_phone(a).as_deref() == Some(device) {
        return Err("正在投屏的 iPhone 不能忘记，先切到另一台".into());
    }
    a.manager.forget(device).await.map_err(|e| e.to_string())?;
    a.hub.phones.forget(device).map_err(|e| format!("蓝牙已忘记，但 CarPlay 配对记录尚未保存：{e}"))?;
    usage_forget(a, device);
    if a.policy().preferred == device {
        a.update_policy(|p| p.preferred.clear())?;
    }
    diag().event_detail("info", "bt", "已忘记一台 iPhone", masked_bt(device));
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cmd {
    pub cmd: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub generation: Option<u64>,
}
/// Commands shared by the car, phone and LuCI (ctl). Long phone operations run in the background.
pub async fn run_cmd(a: &Arc<Admin>, c: Cmd) -> Result<Value, String> {
    let valid_id = |id: &Option<String>| id.as_deref().filter(|d| f50_server::phones::device_component(d).is_ok()).map(str::to_owned).ok_or_else(|| "无效的 iPhone".to_string());
    let manual = (matches!(c.cmd.as_str(), "switchPhone" | "openPairWindow" | "closePairWindow" | "disconnect")
        || (matches!(c.cmd.as_str(), "connect" | "reconnect") && c.id.is_some()))
        .then(|| a.automatic.manual());
    // Only a request naming what to do ends a user disconnect; the car's periodic id-less "connect"
    // and its stall-recovery "reconnect" are automation and must not call a phone back.
    if matches!(c.cmd.as_str(), "switchPhone" | "openPairWindow") || (matches!(c.cmd.as_str(), "connect" | "reconnect") && c.id.is_some()) {
        a.stopped.store(false, Ordering::Release);
    }
    match c.cmd.as_str() {
        "reconnect" => {
            let automatic_epoch = c.id.is_none().then(|| a.hub.phones.epoch());
            if c.id.is_none() && a.stopped.load(Ordering::Acquire) {
                return Ok(json!({"accepted": true}));
            }
            let a = a.clone();
            let id = c.id.clone();
            tokio::spawn(async move {
                let _manual = manual;
                if let Err(e) = reconnect(&a, id.clone(), automatic_epoch).await {
                    f50_server::diag_log!(Warn, "bt", "reconnect failed: {e}");
                }
                if let (Some(manual), Some(id)) = (&_manual, &id) { manual.selected(&a, id); }
            });
            Ok(json!({"accepted": true}))
        }
        "connect" => {
            let automatic_epoch = c.id.is_none().then(|| a.hub.phones.epoch());
            // Call the chosen (or preferred) phone unless it is already in CarPlay. A phone that
            // has just paired is being invited already; the default must not call an older phone.
            if c.id.is_none() && (a.stopped.load(Ordering::Acquire) || a.manager.fresh_pairing().is_some() || handoff_now(a)) {
                return Ok(json!({"accepted": true}));
            }
            let id = match &c.id { Some(_) => Some(valid_id(&c.id)?), None => {
                let list = phones(a).await?;
                list.iter().find(|p| p["preferred"] == true)
                    .or_else(|| list.first()).and_then(|p| p["id"].as_str()).map(str::to_owned)
            } };
            let a = a.clone();
            tokio::spawn(async move {
                let _manual = manual;
                let result = match automatic_epoch {
                    Some(epoch) => {
                        if let Some(id) = id { a.automatic.request(&a, id, epoch); }
                        Ok(())
                    },
                    None => {
                        let r = crate::phone_flow::connect(&a.manager, &a.hub, id.as_deref()).await;
                        if let (Some(manual), Some(id)) = (&_manual, &id) { manual.selected(&a, id); }
                        r
                    },
                };
                if let Err(e) = result {
                    f50_server::diag_log!(Info, "bt", "connect: {e}");
                }
            });
            Ok(json!({"accepted": true}))
        }
        "switchPhone" => {
            let id = valid_id(&c.id)?;
            let a = a.clone();
            tokio::spawn(async move {
                let _manual = manual;
                diag().event("info", "bt", "切换 iPhone");
                if let Err(e) = crate::phone_flow::connect(&a.manager, &a.hub, Some(&id)).await {
                    f50_server::diag_log!(Warn, "bt", "switch failed: {e}");
                }
                if let Some(manual) = &_manual { manual.selected(&a, &id); }
            });
            Ok(json!({"accepted": true}))
        }
        "restartVideo" => {
            if a.hub.restart_video() { Ok(json!({"ok": true})) } else { Err("车机尚未连接查看通道".into()) }
        }
        "reloadCar" => Ok(json!({"ok": a.hub.request_reload()})),
        "disconnect" => {
            a.disconnect().await.map_err(|e| e.to_string())?;
            Ok(json!({"ok": true}))
        }
        "forgetPhone" => {
            let id = valid_id(&c.id)?;
            forget(a, &id).await.map(|_| json!({"ok": true}))
        }
        "setPreferred" => {
            let id = valid_id(&c.id)?;
            a.manager.validate_phone(&id).await.map_err(|e| e.to_string())?;
            a.update_policy(|p| p.preferred = id.clone())?;
            Ok(json!({"ok": true}))
        }
        "openPairWindow" => {
            crate::phone_flow::pair_new(&a.manager, &a.hub).await.map_err(|e| e.to_string())?;
            Ok(json!({"ok": true, "remaining_s": a.manager.status().remaining_s}))
        }
        "closePairWindow" => {
            let epoch=crate::phone_flow::cancel_pairing(&a.manager, &a.hub,c.generation.ok_or("请刷新页面后取消当前配对")?).await.map_err(|e| e.to_string())?;
            // A delayed old acknowledgement cannot replace a later cancellation or stop a newer selection.
            a.cancelled_pairing_epoch.fetch_max(epoch,Ordering::AcqRel);
            Ok(json!({"ok": true}))
        }
        "selftest" => Ok(segments(a, None).await),
        "fullSelftest" => Ok(a.run_selftest().await),
        _ => Err("未知命令".into()),
    }
}

// ---------- HTTP ----------

pub fn router(admin: Arc<Admin>) -> Router {
    Router::new()
        .route("/api/state", get(state_get))
        .route("/api/prefs", post(prefs_post))
        .route("/api/cmd", post(cmd_post))
        .route("/api/stats", get(stats_get))
        .route("/api/diag", get(diag_get))
        .route("/api/selftest/segments", post(segments_post))
        .route("/api/token", get(token_get))
        .route("/api/policy", post(policy_post))
        .route("/api/logs/summary", get(summary_get))
        .with_state(admin)
}
async fn state_get(State(a): State<Arc<Admin>>, h: HeaderMap) -> Response {
    match client(&a, &h, false) {
        Ok(me) => json_ok(state(&a, &me).await),
        Err(r) => r,
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrefsPatch {
    /// self (default for screens) | viewer (phones: the screen showing CarPlay)
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    screen_id: Option<String>,
    prefs: Value,
}
async fn prefs_post(State(a): State<Arc<Admin>>, h: HeaderMap, body: Bytes) -> Response {
    let me = match client(&a, &h, true) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let Ok(p) = serde_json::from_slice::<PrefsPatch>(&body) else { return fail(StatusCode::BAD_REQUEST, "格式无效") };
    let target = if p.target.as_deref() == Some("viewer") || me.kind == "phone" {
        match a.viewer().or_else(|| a.hub.clients.list().into_iter().filter(|c| c.kind == "screen").max_by_key(|c| c.last_ms)) {
            Some(v) => v,
            None => return fail(StatusCode::CONFLICT, "还没有车机屏幕连上 CowPlay"),
        }
    } else {
        me
    };
    if p.screen_id.as_ref().is_some_and(|id|id!=&target.handle()) { return fail(StatusCode::CONFLICT, "车机屏幕已切换，请重新打开画面设置"); }
    let mut merged = serde_json::to_value(&target.prefs).unwrap_or(Value::Null);
    if let (Value::Object(m), Value::Object(patch)) = (&mut merged, p.prefs) {
        for (k, v) in patch {
            if !m.contains_key(&k) {
                return fail(StatusCode::BAD_REQUEST, format!("未知设置 {k}"));
            }
            m.insert(k, v);
        }
    }
    let prefs: ScreenPrefs = match serde_json::from_value(merged) {
        Ok(p) => p,
        Err(_) => return fail(StatusCode::BAD_REQUEST, "设置值无效"),
    };
    if let Err(error)=crate::admin::validate_screen_display(&prefs,&a.policy()){return fail(StatusCode::BAD_REQUEST,error);}
    let reconfigure = a.hub.viewer_client().as_deref() == Some(target.id.as_str())
        && (prefs.right_hand_drive != target.prefs.right_hand_drive || prefs.quality != target.prefs.quality || prefs.call != target.prefs.call || prefs.ui_size != target.prefs.ui_size
            || prefs.resolution != target.prefs.resolution || prefs.fps != target.prefs.fps || prefs.codec != target.prefs.codec);
    match a.hub.clients.set_prefs(&target.id, prefs) {
        Ok(c) => {
            a.apply_display(reconfigure);
            json_ok(json!({"prefs": c.prefs, "spec": a.hub.display_spec()}))
        }
        Err(e) => fail(StatusCode::BAD_REQUEST, e),
    }
}
async fn cmd_post(State(a): State<Arc<Admin>>, h: HeaderMap, body: Bytes) -> Response {
    if let Err(r) = client(&a, &h, true) {
        return r;
    }
    let Ok(c) = serde_json::from_slice::<Cmd>(&body) else { return fail(StatusCode::BAD_REQUEST, "格式无效") };
    match run_cmd(&a, c).await {
        Ok(v) => json_ok(v),
        Err(e) => fail(StatusCode::CONFLICT, e),
    }
}
async fn stats_get(State(a): State<Arc<Admin>>, h: HeaderMap) -> Response {
    match client(&a, &h, false) {
        Ok(_) => json_ok(stats(&a)),
        Err(r) => r,
    }
}
async fn diag_get(State(a): State<Arc<Admin>>, h: HeaderMap) -> Response {
    match client(&a, &h, false) {
        Ok(_) => json_ok(diagnosis(&a)),
        Err(r) => r,
    }
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct SegBody {
    #[serde(default)]
    car_rtt_ms: Option<f64>,
}
async fn segments_post(State(a): State<Arc<Admin>>, h: HeaderMap, body: Bytes) -> Response {
    if let Err(r) = client(&a, &h, true) {
        return r;
    }
    let b: SegBody = if body.is_empty() { SegBody::default() } else { serde_json::from_slice(&body).unwrap_or_default() };
    let r = segments(&a, b.car_rtt_ms.filter(|v| v.is_finite() && *v >= 0.0)).await;
    json_ok(r)
}
async fn token_get(State(a): State<Arc<Admin>>, h: HeaderMap) -> Response {
    match client(&a, &h, false) {
        Ok(c) if c.kind == "phone" => match a.hub.clients.phone_access_status(&c.id) {
            Some(status) => json_ok(status),
            None => fail(StatusCode::UNAUTHORIZED, "请重新验证访问码"),
        },
        Ok(_) => fail(StatusCode::FORBIDDEN, "令牌只能在手机或管理页查看"),
        Err(r) => r,
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyPatch {
    #[serde(default)]
    ask_when_multi: Option<bool>,
    #[serde(default)]
    max_res: Option<String>,
    #[serde(default)]
    max_fps: Option<u8>,
}
async fn policy_post(State(a): State<Arc<Admin>>, h: HeaderMap, body: Bytes) -> Response {
    if let Err(r) = client(&a, &h, true) {
        return r;
    }
    let Ok(p) = serde_json::from_slice::<PolicyPatch>(&body) else { return fail(StatusCode::BAD_REQUEST, "格式无效") };
    if p.max_res.as_ref().is_some_and(|v| crate::policy::resolution_box(v).is_none())
        || p.max_fps.is_some_and(|v| !f50_core::info::FRAME_RATES.contains(&v)) { return fail(StatusCode::BAD_REQUEST, "设备画面上限无效"); }
    let before=a.policy();
    match a.update_policy(|x| {
        if let Some(v) = p.ask_when_multi {
            x.ask_when_multi = v;
        }
        if let Some(v) = &p.max_res { x.max_res = v.clone(); }
        if let Some(v) = p.max_fps { x.max_fps = v; }
    }) {
        Ok(p) => { if p.max_res!=before.max_res||p.max_fps!=before.max_fps {a.apply_display(true);} json_ok(json!({"ask_when_multi": p.ask_when_multi,"max_res":p.max_res,"max_fps":p.max_fps,"spec":a.hub.display_spec()})) },
        Err(e) => fail(StatusCode::CONFLICT, e),
    }
}
#[derive(Deserialize)]
struct SummaryQuery {
    #[serde(default)]
    limit: Option<usize>,
}
/// Connection-stage summary of recent events (phone "日志 › 摘要").
async fn summary_get(State(a): State<Arc<Admin>>, h: HeaderMap, Query(q): Query<SummaryQuery>) -> Response {
    if let Err(r) = client(&a, &h, false) {
        return r;
    }
    let n = q.limit.unwrap_or(20).min(60);
    let events: Vec<Value> = diag().recent(n).into_iter().rev().map(|e| json!({"level": e.kind, "title": e.msg, "detail": e.detail, "ts_ms": e.ts_ms})).collect();
    json_ok(json!({"events": events, "warnings": diag().warning_count(diag::now_ms().saturating_sub(86_400_000))}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn picture_api_persists_independent_settings_and_validates_shared_limits() {
        let (manager,_server,hub,dir)=crate::bluetooth::recovery_tests::admin_fixture().await;
        let a=Admin::new(manager,hub.clone(),"https://test.invalid".into(),dir.path().join("cowplay.uci"),dir.path().join("config"),dir.path().to_owned(),"cert".into(),"key".into(),"127.0.0.1:8080".parse().unwrap(),"127.0.0.1:7000".parse().unwrap(),None,(800,480)).expect("valid synthetic policy");
        let screen=hub.clients.admit_token(&hub.clients.token(),"screen","synthetic","","").unwrap();
        let mut headers=HeaderMap::new();
        headers.insert("cookie",format!("{}={}",f50_server::clients::COOKIE,screen.id).parse().unwrap());
        let body=json!({"prefs":{"quality":"custom","resolution":"1440p","fps":45,"codec":"hevc"}}).to_string();
        assert_eq!(prefs_post(State(a.clone()),headers.clone(),body.clone().into()).await.status(),StatusCode::FORBIDDEN);
        headers.insert("origin","https://test.invalid".parse().unwrap());
        let response=prefs_post(State(a.clone()),headers.clone(),body.into()).await;
        assert_eq!(response.status(),StatusCode::OK);
        let bytes=axum::body::to_bytes(response.into_body(),10_000).await.unwrap();
        let result:Value=serde_json::from_slice(&bytes).unwrap();assert_eq!(result["prefs"]["fps"],45);
        let saved=hub.clients.get(&screen.id).unwrap().prefs;
        assert_eq!((saved.resolution.as_str(),saved.fps,saved.codec.as_str()),("1440p",45,"hevc"));
        assert_eq!(prefs_post(State(a.clone()),headers.clone(),r#"{"prefs":{"fps":120}}"#.into()).await.status(),StatusCode::BAD_REQUEST);
        assert_eq!(hub.clients.get(&screen.id).unwrap().prefs,saved);
        assert_eq!(prefs_post(State(a.clone()),headers.clone(),r#"{"prefs":{"resolution":"320x2160"}}"#.into()).await.status(),StatusCode::BAD_REQUEST);
        assert_eq!(hub.clients.get(&screen.id).unwrap().prefs,saved);
        assert_eq!(prefs_post(State(a.clone()),headers.clone(),r#"{"screen_id":"different-screen","prefs":{"fps":30}}"#.into()).await.status(),StatusCode::CONFLICT);
        assert_eq!(hub.clients.get(&screen.id).unwrap().prefs,saved);
        assert_eq!(prefs_post(State(a.clone()),headers.clone(),r#"{"prefs":{"resolution":"720x1280","right_hand_drive":true}}"#.into()).await.status(),StatusCode::OK);
        assert_eq!(prefs_post(State(a.clone()),headers.clone(),json!({"screen_id":screen.handle(),"prefs":{"fps":30}}).to_string().into()).await.status(),StatusCode::OK);
        let saved=hub.clients.get(&screen.id).unwrap().prefs;assert!(saved.right_hand_drive);assert_eq!(saved.exact_resolution(),Some((720,1280)));
        let before=std::fs::read(&a.uci_path).unwrap();
        assert_eq!(policy_post(State(a.clone()),headers.clone(),r#"{"max_res":"8k"}"#.into()).await.status(),StatusCode::BAD_REQUEST);
        assert_eq!(std::fs::read(&a.uci_path).unwrap(),before);
        assert_eq!(policy_post(State(a.clone()),headers.clone(),r#"{"max_res":"2160p","max_fps":60}"#.into()).await.status(),StatusCode::OK);
        assert_eq!(a.policy().max_box(),(3840,2160));
        assert_eq!(crate::policy::Policy::from_uci(&crate::uci::Package::read(&a.uci_path).unwrap()).max_res,"2160p");
        assert_eq!(policy_post(State(a.clone()),headers.clone(),r#"{"max_res":"1280x800","max_fps":45}"#.into()).await.status(),StatusCode::OK);
        assert_eq!(a.policy().max_box(),(1280,800));
        let compatible=std::fs::read(&a.uci_path).unwrap();
        assert_eq!(policy_post(State(a.clone()),headers.clone(),r#"{"max_res":"320x240"}"#.into()).await.status(),StatusCode::CONFLICT);
        assert_eq!(std::fs::read(&a.uci_path).unwrap(),compatible);
        let mut invalid=crate::uci::Package::read(&a.uci_path).unwrap();invalid.set("cowplay","main","max_res","320x240");invalid.write(&a.uci_path).unwrap();
        assert!(a.reload().is_err(),"LuCI/ctl reload must reject an incompatible exact size");assert_eq!(a.policy().max_box(),(1280,800));
        assert_eq!(crate::policy::Policy::from_uci(&crate::uci::Package::read(&a.uci_path).unwrap()).max_box(),(1280,800),"rejected limits are restored on disk before restart");
        let restarted=Admin::new(a.manager.clone(),hub.clone(),"https://test.invalid".into(),a.uci_path.clone(),dir.path().join("config"),dir.path().to_owned(),"cert".into(),"key".into(),"127.0.0.1:8080".parse().unwrap(),"127.0.0.1:7000".parse().unwrap(),None,(800,480)).unwrap();
        assert_eq!(restarted.policy().max_box(),(1280,800));
        let mut invalid=crate::uci::Package::read(&a.uci_path).unwrap();invalid.set("cowplay","main","max_res","320x240");invalid.write(&a.uci_path).unwrap();
        assert!(Admin::new(a.manager.clone(),hub.clone(),"https://test.invalid".into(),a.uci_path.clone(),dir.path().join("config"),dir.path().to_owned(),"cert".into(),"key".into(),"127.0.0.1:8080".parse().unwrap(),"127.0.0.1:7000".parse().unwrap(),None,(800,480)).is_err(),"invalid cold-start policy never falls back to a different aspect ratio");
        std::fs::write(&a.uci_path,&compatible).unwrap();a.reload().unwrap();
        assert_eq!(a.policy().max_fps,45);
        headers.insert("origin","https://foreign.invalid".parse().unwrap());
        assert_eq!(policy_post(State(a),headers,r#"{"max_res":"720p"}"#.into()).await.status(),StatusCode::FORBIDDEN);
    }
    #[test]
    fn vendor_station_listing_without_signal_is_unavailable_not_zero() {
        let stations=parse_station_dump("Station 00:11:22:33:44:55 (on wlan0)\n\tcurrent time:\t1794186370507 ms\n");
        assert_eq!(stations.len(),1);
        assert!(stations[0].1["signal"].is_null()&&stations[0].1["rate_mbps"].is_null());
        assert_eq!(station_status(&stations,Some("00:11:22:33:44:55"),true,true),"driver_unavailable");
        assert_eq!(station_status(&stations,None,true,true),"unidentified");
        assert_eq!(station_status(&stations,Some("00:11:22:33:44:66"),true,true),"station_missing");
        assert_eq!(station_status(&[],None,true,true),"query_failed");
        assert_eq!(station_status(&[],None,false,false),"disconnected");
    }
    #[test]
    fn wifi_sampling_cannot_fall_back_to_an_unrelated_access_point() {
        let ifaces=vec![("wlan0".into(),"other-ap".into()),("wlan1".into(),"CarPlay-test".into())];
        assert_eq!(wifi_interface(&ifaces,"wlan0",Some("CarPlay-test")),Some("wlan1".into()));
        assert_eq!(wifi_interface(&ifaces,"wlan0",Some("missing-ap")),None);
        assert_eq!(wifi_interface(&ifaces,"",None),None);
    }
    #[test]
    fn old_phone_metrics_do_not_cross_peer_changes_or_expiry() {
        let cached=json!({"peer_ip":"192.0.2.20","phone":{"signal":-54,"rate_mbps":866.7},"phone_status":"available"});
        let current=station_for_peer(&cached,Duration::from_secs(1),Some("192.0.2.20"));
        assert_eq!(current["phone"]["signal"],-54);
        for (age,peer) in [(1,Some("192.0.2.21")),(16,Some("192.0.2.20")),(1,None)] {
            let stale=station_for_peer(&cached,Duration::from_secs(age),peer);
            assert!(stale["phone"].is_null());assert_eq!(stale["phone_status"],"stale");
        }
        let system=json!({"ip":"192.0.2.20","rssi_dbm":-54});
        assert_eq!(phone_for_peer(&system,Some("192.0.2.20"))["rssi_dbm"],-54);
        assert!(phone_for_peer(&system,Some("192.0.2.21")).is_null());
        assert!(phone_for_peer(&system,None).is_null());
    }
    #[test]
    fn station_dump_and_iw_info_parse() {
        let dump = "Station aa:bb:cc:dd:ee:ff (on wlan1)\n\tsignal:  \t-54 [-56, -57] dBm\n\ttx bitrate:\t866.7 MBit/s VHT-MCS 9\n\ttx retries:\t11\n\ttx packets:\t989\nStation 11:22:33:44:55:66 (on wlan1)\n\tsignal: -70 dBm\n";
        let s = parse_station_dump(dump);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].0, "aa:bb:cc:dd:ee:ff");
        assert_eq!(s[0].1["signal"], -54);
        assert_eq!(s[0].1["rate_mbps"], 866.7);
        assert_eq!(s[0].1["tx_retries"], 11);
        let info = parse_iw_info("Interface wlan1\n\tssid ZTE_B42533\n\ttype AP\n\tchannel 149 (5745 MHz), width: 80 MHz, center1: 5775 MHz\n");
        assert_eq!(info["ssid"], "ZTE_B42533");
        assert_eq!(info["channel"], 149);
        assert_eq!(info["width_mhz"], 80);
        assert_eq!(info["ap"], true);
    }
    #[tokio::test]
    async fn cancel_without_observed_generation_cannot_change_phone_selection() {
        let (manager,_server,hub,dir)=crate::bluetooth::recovery_tests::admin_fixture().await;
        let a=Admin::new(manager,hub.clone(),"https://test.invalid".into(),dir.path().join("cowplay.uci"),dir.path().join("config"),dir.path().to_owned(),"cert".into(),"key".into(),"127.0.0.1:8080".parse().unwrap(),"127.0.0.1:7000".parse().unwrap(),None,(800,480)).unwrap();
        let epoch=hub.phones.epoch();
        let command:Cmd=serde_json::from_value(json!({"cmd":"closePairWindow"})).unwrap();
        assert!(run_cmd(&a,command).await.unwrap_err().contains("刷新页面"));
        assert_eq!(hub.phones.epoch(),epoch);
    }
    #[tokio::test]
    async fn cancelled_pairing_reports_stopped_without_overriding_a_new_selection() {
        let (manager,_server,hub,dir)=crate::bluetooth::recovery_tests::admin_fixture().await;
        let a=Admin::new(manager.clone(),hub.clone(),"https://test.invalid".into(),dir.path().join("cowplay.uci"),dir.path().join("config"),dir.path().to_owned(),"cert".into(),"key".into(),"127.0.0.1:8080".parse().unwrap(),"127.0.0.1:7000".parse().unwrap(),None,(800,480)).unwrap();
        let client=hub.clients.admit_token(&hub.clients.token(),"screen","car","","").unwrap();
        assert!(!a.user_stopped());
        let mut first=0;
        for index in 0..2 {
            run_cmd(&a,Cmd{cmd:"openPairWindow".into(),id:None,generation:None}).await.unwrap();
            let generation=manager.status().generation;
            run_cmd(&a,Cmd{cmd:"closePairWindow".into(),id:None,generation:Some(generation)}).await.unwrap();
            if index==0 {first=hub.phones.epoch();}
            assert_eq!(state(&a,&client).await["stopped"],true);
        }
        a.cancelled_pairing_epoch.fetch_max(first,Ordering::AcqRel);
        assert!(a.user_stopped(),"a late older cancel cannot hide the latest stopped state");
        assert!(!a.stopped.load(Ordering::Acquire),"cancellation must not overwrite the newer explicit stop flag");
        hub.pause_phone().unwrap();
        assert_eq!(state(&a,&client).await["stopped"],false,"a new selection is not stopped by the old cancellation");
    }
    #[tokio::test]
    async fn default_connect_defers_to_a_freshly_paired_phone() {
        let (manager, _server, hub, dir) = crate::bluetooth::recovery_tests::admin_fixture().await;
        manager.set_fresh_for_test("/org/bluez/hci0/dev_00_11_22_33_44_66");
        let a = Admin::new(manager.clone(), hub.clone(), "https://test.invalid".into(), dir.path().join("cowplay.uci"), dir.path().join("config"), dir.path().to_owned(), "cert".into(), "key".into(), "127.0.0.1:8080".parse().unwrap(), "127.0.0.1:7000".parse().unwrap(), None, (800, 480)).expect("valid synthetic policy");
        let epoch = hub.phones.epoch();
        let r = run_cmd(&a, Cmd { cmd: "connect".into(), id: None, generation:None }).await.unwrap();
        assert_eq!(r["accepted"], true);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(hub.phones.epoch(), epoch, "the preferred older phone must not pre-empt the new one");
        assert_eq!(manager.fresh_pairing().as_deref(), Some("/org/bluez/hci0/dev_00_11_22_33_44_66"));
        let state = state(&a, &hub.clients.admit_token(&hub.clients.token(), "screen", "car", "", "").unwrap()).await;
        let phones = state["phones"].as_array().unwrap();
        assert_eq!(phones.iter().filter(|p| p["selected"] == true).count(), 1, "the car can name the phone being called");
    }
    #[tokio::test]
    async fn a_disconnect_from_any_page_is_shared_until_someone_connects() {
        let (manager, _server, hub, dir) = crate::bluetooth::recovery_tests::admin_fixture().await;
        let a = Admin::new(manager, hub.clone(), "https://test.invalid".into(), dir.path().join("cowplay.uci"), dir.path().join("config"), dir.path().to_owned(), "cert".into(), "key".into(), "127.0.0.1:8080".parse().unwrap(), "127.0.0.1:7000".parse().unwrap(), None, (800, 480)).expect("valid synthetic policy");
        let car = hub.clients.admit_token(&hub.clients.token(), "screen", "car", "", "").unwrap();
        assert_eq!(state(&a, &car).await["stopped"], false);
        run_cmd(&a, Cmd { cmd: "disconnect".into(), id: None, generation:None }).await.unwrap();
        assert_eq!(state(&a, &car).await["stopped"], true, "the car must not call the phone back after a phone-page disconnect");
        run_cmd(&a, Cmd { cmd: "connect".into(), id: Some("/org/bluez/hci0/dev_00_11_22_33_44_66".into()), generation:None }).await.unwrap();
        assert_eq!(state(&a, &car).await["stopped"], false);
    }
    #[tokio::test]
    async fn a_default_connect_during_a_disconnect_cannot_call_the_phone_back() {
        let (manager, _server, hub, dir, disconnects) = crate::bluetooth::recovery_tests::admin_fixture_slow_disconnect(400).await;
        let a = Admin::new(manager, hub.clone(), "https://test.invalid".into(), dir.path().join("cowplay.uci"), dir.path().join("config"), dir.path().to_owned(), "cert".into(), "key".into(), "127.0.0.1:8080".parse().unwrap(), "127.0.0.1:7000".parse().unwrap(), None, (800, 480)).expect("valid synthetic policy");
        let car = hub.clients.admit_token(&hub.clients.token(), "screen", "car", "", "").unwrap();
        let stop = tokio::spawn({ let a = a.clone(); async move { run_cmd(&a, Cmd { cmd: "disconnect".into(), id: None, generation:None }).await } });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(state(&a, &car).await["stopped"], true, "screens must see the stop while the old link is still closing");
        let epoch = hub.phones.epoch();
        run_cmd(&a, Cmd { cmd: "connect".into(), id: None, generation:None }).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(hub.phones.epoch(), epoch, "the car's automatic connect must not supersede the disconnect");
        stop.await.unwrap().expect("the disconnect completes instead of being replaced");
        assert!(disconnects() >= 1, "the race window was real: the phone's Disconnect was in flight");
        assert_eq!(state(&a, &car).await["stopped"], true);
        run_cmd(&a, Cmd { cmd: "connect".into(), id: None, generation:None }).await.unwrap();
        assert_eq!(state(&a, &car).await["stopped"], true, "only an explicit choice ends a stop");
    }
    #[test]
    fn handoff_guard_covers_a_waiting_iphone_but_not_an_ended_session() {
        let base = diag::Link { bluetooth: "ok", wifi: "active", carplay: "idle", error: None, updated_ms: 1_000_000, ..Default::default() };
        assert!(handoff(true, false, &base, 1_060_000), "iAP2 up, waiting at the CarPlay prompt");
        assert!(!handoff(true, false, &base, 1_130_000), "bounded: a declined prompt cannot block recovery");
        assert!(!handoff(false, false, &base, 1_000_000), "no channel, nothing to protect");
        assert!(!handoff(true, true, &base, 1_000_000), "a session already exists");
        let ended = diag::Link { wifi: "idle", ..base.clone() };
        assert!(!handoff(true, false, &ended, 1_000_000), "iAP2 can outlive an ended session; recovery must still run");
        let failed = diag::Link { error: Some(diag::LinkError { step: "carplay", title: "x".into(), hint: "y".into(), ts_ms: 1 }), ..base.clone() };
        assert!(!handoff(true, false, &failed, 1_000_000));
    }
    #[tokio::test]
    async fn automatic_reconnect_cannot_end_a_stop() {
        let (manager, _server, hub, dir) = crate::bluetooth::recovery_tests::admin_fixture().await;
        let a = Admin::new(manager, hub.clone(), "https://test.invalid".into(), dir.path().join("cowplay.uci"), dir.path().join("config"), dir.path().to_owned(), "cert".into(), "key".into(), "127.0.0.1:8080".parse().unwrap(), "127.0.0.1:7000".parse().unwrap(), None, (800, 480)).expect("valid synthetic policy");
        let car = hub.clients.admit_token(&hub.clients.token(), "screen", "car", "", "").unwrap();
        run_cmd(&a, Cmd { cmd: "disconnect".into(), id: None, generation:None }).await.unwrap();
        let epoch = hub.phones.epoch();
        run_cmd(&a, Cmd { cmd: "reconnect".into(), id: None, generation:None }).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(state(&a, &car).await["stopped"], true);
        assert_eq!(hub.phones.epoch(), epoch, "stall recovery must not call the phone back");
    }
    #[test]
    fn iphone_mac_comes_from_complete_neighbour_entries() {
        let arp = "IP address       HW type     Flags       HW address            Mask     Device\n192.0.2.20       0x1         0x2         AA:BB:CC:DD:EE:01     *        wlan1\n192.0.2.21       0x1         0x0         00:00:00:00:00:00     *        wlan1\n";
        assert_eq!(arp_mac(arp, "192.0.2.20").as_deref(), Some("aa:bb:cc:dd:ee:01"));
        assert_eq!(arp_mac(arp, "192.0.2.21"), None, "incomplete entries are not a station");
        assert_eq!(arp_mac(arp, "192.0.2.2"), None, "no prefix matches");
    }
    #[test]
    fn bt_address_is_masked() {
        assert_eq!(masked_bt("/org/bluez/hci0/dev_DA_1E_C4_12_34_16"), "DA:1E:C4:••:••:16");
        assert_eq!(masked_bt("bogus"), "—");
    }
    #[test]
    fn check_ratio_puts_limit_in_the_middle() {
        let c = Check { group: "", name: "抖动", value: Some(10.0), text: String::new(), limit: 10.0, upper: true, unit: "ms" };
        assert!((c.ratio() - 0.5).abs() < 1e-9 && !c.bad());
        let c = Check { value: Some(22.0), ..c };
        assert!(c.bad() && c.ratio() > 0.9);
        let r = Check { group: "", name: "信号", value: Some(-70.0), text: String::new(), limit: -70.0, upper: false, unit: "dBm" };
        assert!((r.ratio() - 0.5).abs() < 1e-9 && !r.bad());
        assert!(Check { value: Some(-80.0), ..r }.bad());
    }
    fn frame_sample(input: f64, painted: f64) -> Value {
        json!({"server":{"fps_in":input},"spec":{"fps":60},"car":{"fps":painted,"visible":true,"decode_ms":3,"rtt_ms":8,"jitter_ms":2,"loss_pct":0},"stalls_10m":0})
    }
    #[test]
    fn frame_rate_follows_actual_input_instead_of_the_negotiated_ceiling() {
        let link = f50_server::diag::Link::default();
        let idle = diagnosis_samples(&frame_sample(0.0,0.0), "auto", &link, true);
        assert_eq!(idle["checks"][6]["known"], false);
        assert_eq!(idle["checks"][7]["known"], false);
        assert_eq!(idle["verdict"]["title"], "已连接，等待画面更新");
        for (input, painted, ok) in [(5.0,5.0,true),(30.0,29.0,true),(60.0,10.0,false)] {
            let d = diagnosis_samples(&frame_sample(input,painted), "auto", &link, true);
            assert_eq!(d["checks"][6]["ok"], ok, "input={input}, painted={painted}");
        }
    }
    #[test]
    fn missing_metrics_do_not_become_an_all_links_healthy_verdict() {
        let d = diagnosis_samples(&frame_sample(60.0,60.0), "auto", &f50_server::diag::Link::default(), true);
        assert_eq!(d["verdict"]["level"],"unknown");
        assert_eq!(d["checks"][3]["known"],false);
        assert!(d["checks"][3]["ok"].is_null());
    }
    #[test]
    fn hidden_or_stale_browser_samples_do_not_prove_a_frame_rate_fault() {
        let link = f50_server::diag::Link::default();
        let mut s = frame_sample(60.0,0.0);
        s["car"]["visible"] = json!(false);
        assert_eq!(diagnosis_samples(&s,"auto",&link,true)["checks"][6]["known"], false);
        s["car"] = Value::Null;
        assert_eq!(diagnosis_samples(&s,"auto",&link,true)["checks"][6]["known"], false);
    }
    #[test]
    fn jitter_alone_does_not_identify_wifi_obstruction_or_prescribe_reconnection() {
        let mut s = frame_sample(60.0,60.0);
        s["car"]["jitter_ms"] = json!(24);
        let d = diagnosis_samples(&s,"auto",&f50_server::diag::Link::default(),true);
        assert_eq!(d["checks"][1]["ok"], false, "keep the existing jitter threshold");
        assert_eq!(d["verdict"]["title"], "车机到接收端的往返响应波动较大");
        assert!(!d["verdict"]["detail"].as_str().unwrap().contains("金属"));
        assert_eq!(d["verdict"]["action"], Value::Null);
    }
}
