//! Local control socket for the LuCI app (rpcd plugin → `cowplay ctl METHOD`).
//! Root-only Unix socket; LuCI's own login and ACLs decide who may call rpcd.
use crate::admin::Admin;
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path as UrlPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use f50_server::diag::{self, LogQuery, diag};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

pub const SOCKET: &str = "/var/run/cowplay.sock";

pub async fn serve(admin: Arc<Admin>, path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::remove_file(path);
    let listener = tokio::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    let app = Router::new().route("/ctl/{method}", post(call)).with_state(admin);
    axum::serve(listener, app).await
}

fn err(msg: impl Into<String>) -> Response {
    (StatusCode::CONFLICT, Json(json!({"error": msg.into()}))).into_response()
}
fn arg<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

async fn call(State(a): State<Arc<Admin>>, UrlPath(method): UrlPath<String>, body: Bytes) -> Response {
    let mut args: Value = if body.is_empty() { json!({}) } else { serde_json::from_slice(&body).unwrap_or(json!({})) };
    if let Some(args) = args.as_object_mut() {
        args.remove("ubus_rpc_session");
    }
    match dispatch(&a, &method, &args).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(e),
    }
}

pub async fn dispatch(a: &Arc<Admin>, method: &str, args: &Value) -> Result<Value, String> {
    Ok(match method {
        "status" => status(a).await,
        "network_info" => {
            if !a.openwrt { return Err("无线配置读取仅适用于 OpenWrt".into()); }
            network_info(&crate::uci::Package::read(Path::new("/etc/config/wireless"))?, &crate::uci::Package::read(Path::new("/etc/config/network"))?, &a.policy().wifi_iface, a.wifi.as_ref().map(|w|w.0.as_str()).unwrap_or(""))?
        },
        #[cfg(feature = "tesla")]
        "tesla_status" | "tesla_configure" | "tesla_connect" | "tesla_disconnect" | "tesla_pair" | "tesla_cancel" | "tesla_retry" | "tesla_climate_pair" | "tesla_climate_query" | "tesla_climate_command" | "tesla_ultra_control" => crate::admin::tesla::dispatch(a, method, args).await?,
        #[cfg(not(feature = "tesla"))]
        "tesla_status" => return Err("此构建未启用 Tesla 支持".into()),
        "diag" => json!({"diag": crate::console::diagnosis(a), "stats": crate::console::stats(a), "context":crate::console::connection_context(a)}),
        "mfi_upload" => {
            let _permit = a.ddns_busy.clone().try_acquire_owned().map_err(|_|"证书任务正在进行")?;
            let public=crate::identity::select_pair(&a.config_path,&a.state_dir,arg(args,"certificate").ok_or("缺少 CarPlay 证书")?,arg(args,"private_key").ok_or("缺少 CarPlay 私钥")?)?;
            *a.pending_public_mfi.lock().unwrap()=Some(vec![public]);
            a.identity_pending.store(true,std::sync::atomic::Ordering::Release);
            diag().event("info","web","已导入第三方 CarPlay 配件身份，等待手动重启 CowPlay");
            json!({"ok":true,"restart_required":true})
        },
        "network_check" => {
            if !a.openwrt { return Err("端口迁移仅适用于 OpenWrt；请修改主机配置并手动重启服务".into()); }
            let port = args["port"].as_u64().filter(|p|(1..=65535).contains(p)).ok_or("HTTPS 端口应为 1–65535")? as u16;
            let mut paths=crate::network::Paths::live();paths.cowplay=a.uci_path.clone();paths.config=a.config_path.clone();
            serde_json::to_value(crate::network::inspect(&paths,a.web_bind,Some(a.web_bind.port()),port).await?).map_err(|e|e.to_string())?
        },
        // Private local context for the independent network CLI; not an rpcd method.
        "network_context" => json!({"bind":a.web_bind.to_string(),"pid":std::process::id()}),
        "phones" => {
            let p = a.policy();
            json!({"phones": crate::console::phones(a).await?, "preferred": p.preferred, "ask_when_multi": p.ask_when_multi, "ask_timeout": p.ask_timeout, "pairing": a.manager.status()})
        }
        "screens" => {
            let viewer = a.viewer().map(|v| v.id);
            json!({"screens": a.hub.clients.list().iter().filter(|c| c.kind == "screen").map(|c| { let mut v = c.public(); v["viewing"] = json!(viewer.as_deref() == Some(c.id.as_str())); v }).collect::<Vec<_>>(),
                   "phones": a.hub.clients.list().iter().filter(|c| c.kind == "phone").map(|c| c.public()).collect::<Vec<_>>()})
        }
        "client_remove" => {
            let id = arg(args, "id").ok_or("缺少 id")?;
            if !a.hub.clients.remove(id).map_err(|e| format!("无法保存授权变更：{e}"))? {
                return Err("没有这台设备".into());
            }
            diag().event("info", "web", "管理页移除了一台已授权设备");
            json!({"ok": true})
        }
        "cmd" => {
            let cmd = crate::console::Cmd { cmd: arg(args, "cmd").ok_or("缺少 cmd")?.to_owned(), id: arg(args, "id").map(str::to_owned), generation: args["generation"].as_u64() };
            crate::console::run_cmd(a, cmd).await?
        }
        // Root-only Unix control socket; this method is not exposed by the rpcd whitelist.
        "bluetooth_reset" => {
            // Share the user-stop state so the car's periodic connect cannot replace this reset.
            a.stopped.store(true, std::sync::atomic::Ordering::Release);
            crate::phone_flow::reset_adapter(&a.manager, &a.hub).await.map_err(|e| e.to_string())?;
            json!({"ok": true})
        }
        "token_set" => {
            a.hub.clients.set_token(arg(args, "token").ok_or("缺少访问码")?).map_err(|e|e.to_string())?;
            json!({"ok":true})
        },
        "token_disable" => {
            a.hub.clients.disable_token().map_err(|e| e.to_string())?;
            json!({"ok":true})
        },
        "token" => match arg(args, "action").unwrap_or("show") {
            "reset" => {
                let t = a.hub.clients.reset_token().map_err(|e| e.to_string())?;
                diag().event("info", "web", "访问令牌已重置");
                json!({"token": t})
            }
            _ => a.hub.clients.access_status(),
        },
        "reload" => {
            let before = a.policy();
            let after = a.reload()?;
            a.apply_display(true);
            let wifi_changed = a.openwrt && a.wifi.as_ref().is_some_and(|(ssid, channel)| {
                crate::uci::Package::read(Path::new("/etc/config/wireless")).ok()
                    .and_then(|p| crate::service::uci_hotspot(&p, &after.wifi_iface, ssid).ok().flatten())
                    .is_some_and(|w| w.ssid != *ssid || w.channel.is_some_and(|c| c != *channel) || a.wifi_password().is_some_and(|pw| *w.password != pw))
            });
            json!({"ok": true, "restart_required": a.web_bind.port() != after.port || before.domain != after.domain || before.redirect_http != after.redirect_http || before.wifi_iface != after.wifi_iface || wifi_changed,
                "network_restart_note": "热点的 SSID、密码和信道修改后，还需重启 CowPlay 以更新手机交接信息"})
        }
        "logs" => {
            let q = LogQuery { limit: Some(args.get("limit").and_then(Value::as_u64).unwrap_or(300).min(5000) as usize), level: arg(args, "level").map(str::to_owned), module: arg(args, "module").map(str::to_owned), ..Default::default() };
            let page = diag().logs(&q);
            json!({"entries":page.entries.iter().map(|e|json!({"seq":e.seq,"ts_ms":e.ts_ms,"level":e.level,"module":e.module,"msg":diag::mask_macs(&e.msg)})).collect::<Vec<_>>(),
                "lines": page.entries.iter().map(|e| diag::mask_macs(&diag::format_line(e))).collect::<Vec<_>>(), "total": page.total,"matched":page.matched,
                "memory":{"bytes":page.bytes,"cap_bytes":page.cap_bytes,"evicted":page.evicted},"storage":diag().storage_status(),"retention_days":a.policy().log_keep,"persistent_enabled":a.prefs().log.usb,"syslog_mirror":a.prefs().log.syslog,"source":"current_process_ram","limit":q.limit})
        }
        "logs_export" => a.logs_export().await?,
        "timeline" => {
            let (session, _) = diag().sessions();
            let all = arg(args, "scope") == Some("all");
            let events = if all { diag().recent(60) } else { session.as_ref().map(|s|s.events.iter().rev().take(60).cloned().collect()).unwrap_or_default() };
            json!({"scope":if all {"all"} else {"current"},"session":session.as_ref().map(|s|json!({"id":s.id,"phone":s.phone,"started_ms":s.started_ms})),"events":events,"context":crate::console::connection_context(a)})
        },
        "proc" => json!({"process": a.process(), "config": std::fs::read_to_string(&a.uci_path).unwrap_or_default(), "system": a.system().await}),
        "bundle" => {
            bound_luci_bundle(a.bundle().await)?
        },
        "selftest" => crate::console::segments(a, None).await,
        "ddns_sync" => json!({"result": a.ddns_update().await?}),
        "ddns_token" => {
            let t = arg(args, "token").unwrap_or("").trim();
            if t.len() > 256 || t.chars().any(|c| c.is_control() || c.is_whitespace()) {
                return Err("令牌格式无效".into());
            }
            if t.is_empty() {
                let _ = std::fs::remove_file(a.token_path());
            } else {
                f50_server::settings::write_private(&a.token_path(), t.as_bytes()).map_err(|e| e.to_string())?;
            }
            json!({"ok": true, "token_set": !t.is_empty()})
        }
        "cert" => cert(a, args).await?,
        "certificate_export" => {
            let (name,chain)=match arg(args,"kind") {
                Some("tls") => ("cowplay-tls-public.pem",a.public_tls.lock().unwrap().clone()),
                Some("carplay") => ("cowplay-carplay-public.pem",a.public_mfi.lock().unwrap().clone()),
                _ => return Err("证书种类只支持 tls 或 carplay".into()),
            };
            json!({"name":name,"pem":crate::certificates::public_pem(&chain.ok_or("尚无运行中公开证书快照；请核对服务版本与加载状态")?)?,"contains_private_key":false})
        }
        "key_export" => {
            let _permit = a.ddns_busy.clone().try_acquire_owned().map_err(|_| "证书任务正在进行")?;
            let (name,path,chain,carplay) = match arg(args,"kind") {
                Some("tls") => ("cowplay-tls-private.pem",std::path::PathBuf::from(&a.tls.lock().unwrap().1),a.public_tls.lock().unwrap().clone(),false),
                Some("carplay") => ("cowplay-carplay-private.pk8",a.running_mfi_key.lock().unwrap().clone().ok_or("当前没有可导出的 CarPlay 私钥")?,a.public_mfi.lock().unwrap().clone(),true),
                _ => return Err("私钥种类只支持 tls 或 carplay".into()),
            };
            let bytes = zeroize::Zeroizing::new(crate::certificates::export_key(&path,&chain.ok_or("当前没有运行中证书快照")?,carplay)?);
            let encoded = zeroize::Zeroizing::new(der::pem::encode_string("PRIVATE KEY EXPORT",der::pem::LineEnding::LF,&bytes).map_err(|_|"私钥编码失败")?);
            json!({"name":name,"data":encoded.lines().filter(|s|!s.starts_with("-----")).collect::<String>()})
        }
        "cert_upload" => {
            let mut args = args.clone();
            args["action"] = json!("upload");
            cert(a, &args).await?
        }
        "config_export" => json!({"text": std::fs::read_to_string(&a.uci_path).unwrap_or_default()}),
        "config_import" => {
            let text = arg(args, "text").ok_or("缺少配置内容")?;
            if text.len() > 32768 {
                return Err("文件过大".into());
            }
            let pkg = match a.policy_store {
                crate::policy::Store::Json { .. } => crate::policy::Store::json_package(text.as_bytes())?,
                crate::policy::Store::Uci { .. } => crate::uci::Package::parse(text)?,
            };
            if pkg.section("main").is_none_or(|s| s.kind != "cowplay") {
                return Err("不是 CowPlay 配置（缺少 config cowplay 'main'）".into());
            }
            crate::policy::Policy::validate_display(&pkg)?;
            let policy = crate::policy::Policy::from_uci(&pkg);
            let mut out = pkg.clone();
            policy.to_uci(&mut out);
            a.policy_store.write(&out)?;
            a.reload()?;
            json!({"ok": true})
        }
        "factory_reset" => factory_reset(a).await?,
        _ => return Err(format!("unknown method {method}")),
    })
}

fn network_info(wireless: &crate::uci::Package, network: &crate::uci::Package, iface: &str, ssid: &str) -> Result<Value, String> {
    let aps: Vec<_> = wireless.sections.iter().filter(|s| s.kind == "wifi-iface" && s.get("mode") == Some("ap") && s.get("disabled") != Some("1"))
        .filter(|s| if iface.is_empty() { s.get("ssid") == Some(ssid) } else { s.name.as_deref() == Some(iface) || s.get("ifname") == Some(iface) }).collect();
    let [ap] = aps.as_slice() else { return Err("无法唯一定位 CowPlay 热点，请在 Wi-Fi 设置中核对".into()); };
    let values = |s: &crate::uci::Section, key: &str| -> Vec<String> {
        match s.options.iter().find(|(k,_)|k == key).map(|(_,v)|v) {
            Some(crate::uci::Value::Single(v)) => v.split_whitespace().map(str::to_owned).collect(),
            Some(crate::uci::Value::List(v)) => v.clone(),
            None => Vec::new(),
        }
    };
    let interfaces: Vec<_> = values(ap, "network").iter().map(|name| {
        let section = network.section(name);
        json!({"name":name,"addresses":section.map(|s|values(s,"ipaddr")).unwrap_or_default(),"netmask":section.and_then(|s|s.get("netmask"))})
    }).collect();
    let device = ap.get("device").and_then(|name|wireless.section(name));
    Ok(json!({"ssid":ap.get("ssid"),"channel":device.and_then(|s|s.get("channel")),"htmode":device.and_then(|s|s.get("htmode")),"interfaces":interfaces}))
}

async fn status(a: &Arc<Admin>) -> Value {
    let stats = crate::console::stats(a);
    let d = crate::console::diagnosis(a);
    let net = crate::console::network(a);
    let (current, history) = diag().sessions();
    let viewer = a.viewer();
    let phones = crate::console::phones(a).await.unwrap_or_default();
    let cur = phones.iter().find(|p| p["current"] == true).cloned();
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "build": f50_server::build_info::BUILD_ID,
        "vehicle": a.hub.vehicle.snapshot(),
        "pid": std::process::id(),
        "uptime_s": diag().started.elapsed().as_secs(),
        "diag_code": diag().diag_code(),
        "policy": a.policy(),
        "link": diag().link(),
        "hub": a.hub.observation(),
        "spec": a.hub.display_spec(),
        "display_limits":crate::console::display_limits(a),
        "viewer": viewer.map(|v| v.public()),
        "phone": cur,
        "stats": stats,
        "verdict": d["verdict"],
        "problems": d["problems"],
        "network": net,
        "identity_backed_up": a.state_dir.join(".identity-exported").exists(),
        "identity": {"loaded":a.public_mfi.lock().unwrap().is_some(),"pending_restart":a.identity_pending.load(std::sync::atomic::Ordering::Acquire)},
        "log_retention_supported": true,
        "auto_fps_supported": true,
        "automatic_fps_limit": a.auto_fps_limit(),
        "context":crate::console::connection_context(a),
        "sessions": current.iter().chain(history.iter()).take(6).map(crate::admin::summary).collect::<Vec<_>>(),
    })
}

/// Keep rpcd's result below ubus' message size, without cutting UTF-8 or hiding truncation.
fn bound_luci_bundle(mut bundle: Value) -> Result<Value, String> {
    const LOG_BYTES:usize=64*1024;
    let stored = bundle.pointer("/stored_logs/tail").and_then(Value::as_str).is_some_and(|s| !s.is_empty());
    if let Some(logs)=bundle["logs"].as_str() {
        let original=logs.len();
        let budget=if stored {LOG_BYTES/2} else {LOG_BYTES};
        let mut from=original.saturating_sub(budget);
        while !logs.is_char_boundary(from) {from+=1;}
        let tail=logs[from..].to_owned();
        bundle["logs"]=json!(tail);
        bundle["log_export"]=json!({"truncated":from>0,"original_bytes":original,"included_bytes":original-from,"note":"LuCI 最多包含64KiB日志；启用持久化时与当前RAM尾部分配额度，截断均明确标注"});
    }
    if let Some(logs)=bundle.pointer("/stored_logs/tail").and_then(Value::as_str) {
        let original=logs.len();
        let budget=LOG_BYTES.saturating_sub(bundle["logs"].as_str().map_or(0,str::len));
        let mut from=original.saturating_sub(budget);
        while !logs.is_char_boundary(from) {from+=1;}
        let tail=logs[from..].to_owned();
        let previous=bundle["stored_logs"]["truncated"]==true;
        bundle["stored_logs"]["tail"]=json!(tail);
        bundle["stored_logs"]["truncated"]=json!(previous||from>0);
        bundle["stored_logs"]["luci_original_bytes"]=json!(original);
        bundle["stored_logs"]["luci_included_bytes"]=json!(original-from);
    }
    if serde_json::to_vec(&bundle).map_err(|e|e.to_string())?.len()>512*1024 {return Err("诊断包超过 LuCI 安全传输大小；请按日志级别/来源分别导出".into());}
    Ok(bundle)
}
fn validate_certificate_host(path: &Path, origin: &str) -> Result<Vec<String>, String> {
    let url = reqwest::Url::parse(origin).map_err(|_|"当前运行地址无效")?;
    let host = url.host_str().ok_or("当前运行地址缺少域名")?;
    let names = leaf_names(path.to_str().ok_or("证书路径无效")?);
    let matches = names.iter().any(|n|n.eq_ignore_ascii_case(host) || n.strip_prefix("*.").is_some_and(|suffix|host.split_once('.').is_some_and(|(_,rest)|rest.eq_ignore_ascii_case(suffix))));
    if !matches { return Err(format!("证书域名（{}）与当前访问域名 {host} 不一致；当前证书保持不变",names.join(", "))); }
    Ok(names)
}
fn validate_certificate_dates(path:&Path)->Result<(),String> {
    use der::Decode;
    use rustls_pki_types::{CertificateDer,pem::PemObject};
    let pem=std::fs::read(path).map_err(|e|e.to_string())?;
    let leaf=CertificateDer::from_pem_slice(&pem).map_err(|e|e.to_string())?;
    let cert=x509_cert::Certificate::from_der(&leaf).map_err(|e|e.to_string())?;
    let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e|e.to_string())?;
    if now<cert.tbs_certificate.validity.not_before.to_unix_duration() || now>=cert.tbs_certificate.validity.not_after.to_unix_duration() {return Err("证书尚未生效或已过期，已保留旧证书；请同时核对设备时间".into());}
    Ok(())
}

async fn cert(a: &Arc<Admin>, args: &Value) -> Result<Value, String> {
    let previous = a.state_dir.join("tls-previous.json");
    match arg(args, "action").unwrap_or("info") {
        "info" => {
            let (cert, _) = a.tls.lock().unwrap().clone();
            let details=|chain:&Option<Vec<Vec<u8>>>, source:&str|chain.as_deref().map(|chain|{
                let mut value=crate::certificates::metadata(chain).unwrap_or_else(|e|json!({"error":e}));
                value["source"]=json!(source); value
            });
            let tls=details(&a.public_tls.lock().unwrap(),"running_certificate_snapshot");
            let value=tls.as_ref().unwrap_or(&Value::Null);
            let days=value["not_after_ms"].as_u64().map(|end|(i128::from(end)-i128::from(diag::now_ms()))/86_400_000);
            Ok(json!({"path": cert, "days": days, "chain": value["chain_count"], "rollback": previous.is_file(), "valid":value["date_valid"], "subject": value["dns_names"], "task":a.ddns_state.lock().unwrap().clone(), "dns_provider":a.policy().ddns_provider,"domain":a.prefs().ddns.domain,"token_set":a.token_path().is_file(),
                "details":tls,"carplay":details(&a.public_mfi.lock().unwrap(),"running_certificate_snapshot"),"pending_carplay":details(&a.pending_public_mfi.lock().unwrap(),"saved_pending_restart")}))
        }
        "upload" => {
            if serde_json::to_vec(&json!({"cert":args["cert"],"key":args["key"]})).map_err(|e|e.to_string())?.len()>49152 {return Err("证书链与私钥合计 JSON 编码后须小于 48 KiB".into());}
            let _permit = a.ddns_busy.clone().try_acquire_owned().map_err(|_|"证书任务正在进行")?;
            let issued = crate::acme::Issued { chain_pem: arg(args, "cert").ok_or("缺少证书")?.to_owned(), key_pem: arg(args, "key").ok_or("缺少私钥")?.to_owned() };
            let (cert, key) = crate::certificates::store_pair(&a.state_dir, &issued, false)?;
            let names = validate_certificate_host(&cert, &a.origin)?;
            if crate::admin::tls_info(cert.to_str().ok_or("证书路径无效")?).is_err() { return Err("无法读取证书有效期，已保留旧证书".into()); }
            validate_certificate_dates(&cert)?;
            a.activate_certificate_pair(&cert, &key, "upload").await?;
            diag().event("ok", "web", "已替换 HTTPS 证书（上传）");
            Ok(json!({"ok": true, "names": names, "days": crate::admin::tls_info(cert.to_str().unwrap_or("")).ok().map(|i| i.0)}))
        }
        "rollback" => {
            let _permit = a.ddns_busy.clone().try_acquire_owned().map_err(|_|"证书任务正在进行")?;
            let v: Value = serde_json::from_slice(&std::fs::read(&previous).map_err(|_| "没有可回滚的证书")?).map_err(|e| e.to_string())?;
            let (cert, key) = (v["cert"].as_str().ok_or("记录无效")?.to_owned(), v["key"].as_str().ok_or("记录无效")?.to_owned());
            validate_certificate_dates(Path::new(&cert))?;
            validate_certificate_host(Path::new(&cert), &a.origin)?;
            a.activate_certificate_pair(Path::new(&cert), Path::new(&key), v["source"].as_str().unwrap_or("upload")).await?;
            let _ = std::fs::remove_file(&previous);
            diag().event("info", "web", "HTTPS 证书已回滚到上一张");
            Ok(json!({"ok": true}))
        }
        "renew" => {
            a.start_certificate_renewal()?;
            Ok(json!({"accepted": true}))
        }
        _ => Err("未知证书操作".into()),
    }
}
/// DNS names in the leaf certificate subjectAltName.
fn leaf_names(path: &str) -> Vec<String> {
    use der::Decode;
    use rustls_pki_types::{CertificateDer, pem::PemObject};
    let Ok(pem) = std::fs::read(path) else { return vec![] };
    let Some(Ok(leaf)) = CertificateDer::pem_slice_iter(&pem).next() else { return vec![] };
    let Ok(cert) = x509_cert::Certificate::from_der(&leaf) else { return vec![] };
    let mut names = Vec::new();
    if let Ok(Some((_, san))) = cert.tbs_certificate.get::<x509_cert::ext::pkix::SubjectAltName>() {
        for n in san.0 {
            if let x509_cert::ext::pkix::name::GeneralName::DnsName(d) = n {
                names.push(d.to_string());
            }
        }
    }
    names
}

async fn factory_reset(a: &Arc<Admin>) -> Result<Value, String> {
    crate::phone_flow::disconnect(&a.manager, &a.hub).await.map_err(|e| format!("恢复默认未开始：无法断开当前连接（{e}）"))?;
    let mut forgotten = 0;
    let list = a.manager.saved_phones().await.map_err(|e| format!("恢复默认未完成：无法读取已保存手机（{e}）"))?;
    for p in list {
        a.manager.forget(&p.id).await.map_err(|e| format!("恢复默认未完成：已忘记 {forgotten} 部手机，其余手机未处理（{e}）"))?;
        a.hub.phones.forget(&p.id).map_err(|e| format!("恢复默认未完成：蓝牙已忘记，但 CarPlay 配对记录未能保存（{e}）"))?;
        forgotten += 1;
    }
    for c in a.hub.clients.list() {
        a.hub.clients.remove(&c.handle()).map_err(|e| e.to_string())?;
    }
    let token = a.hub.clients.reset_token().map_err(|e| e.to_string())?;
    let mut pkg = crate::uci::Package::default();
    crate::policy::Policy::default().to_uci(&mut pkg);
    a.policy_store.write(&pkg)?;
    a.reload()?;
    diag().event("warn", "web", "已恢复 CowPlay 默认设置（保留配件身份）");
    Ok(json!({"ok": true, "phones_forgotten": forgotten, "token": token}))
}

/// `cowplay ctl METHOD` — JSON arguments on stdin, JSON result on stdout (rpcd exec protocol).
pub fn client(method: &str, socket: &Path) -> Result<String, String> {
    use std::io::Read;
    let mut body = String::new();
    let _ = std::io::stdin().read_to_string(&mut body);
    send(method,&body,socket,30)
}
pub(crate) fn request(method:&str,args:&Value,socket:&Path)->Result<Value,String> {
    let value:Value=serde_json::from_str(&send(method,&serde_json::to_string(args).map_err(|e|e.to_string())?,socket,2)?).map_err(|e|e.to_string())?;
    if let Some(error)=value["error"].as_str(){return Err(error.to_owned());}Ok(value)
}
fn send(method:&str,body:&str,socket:&Path,seconds:u64)->Result<String,String> {
    use std::io::{Read,Write};
    if method.is_empty() || !method.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Err("invalid method".into());
    }
    let mut s = std::os::unix::net::UnixStream::connect(socket).map_err(|e| format!("CowPlay 服务未运行（{e}）"))?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(seconds))).ok();
    s.set_write_timeout(Some(std::time::Duration::from_secs(seconds))).ok();
    write!(s, "POST /ctl/{method} HTTP/1.1\r\nHost: cowplay\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&raw);
    let (head, rest) = text.split_once("\r\n\r\n").ok_or("bad response")?;
    let body = if head.to_ascii_lowercase().contains("transfer-encoding: chunked") { dechunk(rest) } else { rest.to_owned() };
    Ok(body)
}
fn dechunk(mut s: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = s.split_once("\r\n") {
        let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if n == 0 || rest.len() < n {
            break;
        }
        out.push_str(&rest[..n]);
        s = rest[n..].trim_start_matches("\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn luci_network_facts_include_only_the_selected_hotspot_and_lan_addresses() {
        let wifi = crate::uci::Package::parse("config wifi-device 'radio1'\n option channel '149'\n option htmode 'VHT80'\n config wifi-iface 'car'\n option mode 'ap'\n option device 'radio1'\n option ssid 'Synthetic-Car'\n option key 'SYNTHETIC-SECRET'\n list network 'lan'\n config wifi-iface 'other'\n option mode 'ap'\n option ssid 'Synthetic-Other'\n option key 'OTHER-SECRET'\n").unwrap();
        let lan = crate::uci::Package::parse("config interface 'lan'\n list ipaddr '192.0.2.1'\n list ipaddr '198.51.100.1'\n option netmask '255.255.255.0'\n option password 'UNRELATED-SECRET'\n config interface 'wan'\n option username 'UNRELATED-ACCOUNT'\n").unwrap();
        let facts = super::network_info(&wifi, &lan, "car", "").unwrap();
        assert_eq!(facts["ssid"], "Synthetic-Car");
        assert_eq!(facts["channel"], "149");
        assert_eq!(facts["htmode"], "VHT80");
        assert_eq!(facts["interfaces"][0]["addresses"], serde_json::json!(["192.0.2.1", "198.51.100.1"]));
        assert_eq!(facts["interfaces"][0]["netmask"], "255.255.255.0");
        let text = facts.to_string();
        for excluded in ["SECRET", "ACCOUNT", "Synthetic-Other", "wan", "key"] { assert!(!text.contains(excluded)); }
        assert!(super::network_info(&wifi, &lan, "missing", "").is_err());
        let mut ambiguous = wifi.clone();
        ambiguous.sections.push(wifi.section("car").unwrap().clone());
        assert!(super::network_info(&ambiguous, &lan, "car", "").is_err());
    }

    #[tokio::test]
    #[cfg(feature = "tesla")]
    async fn luci_tesla_accepts_rpc_session_metadata_and_preserves_strict_arguments() {
        let (manager, _server, hub, dir) = crate::bluetooth::recovery_tests::admin_fixture().await;
        let a = crate::admin::Admin::new(manager, hub, "https://test.invalid".into(), dir.path().join("cowplay.uci"), dir.path().join("config"), dir.path().to_owned(), "cert".into(), "key".into(), "127.0.0.1:18443".parse().unwrap(), "127.0.0.1:7000".parse().unwrap(), None, (800, 480)).unwrap();
        let args = serde_json::json!({"vin":"SYNTH000000000001", "enabled":false, "ubus_rpc_session":"synthetic-session"});
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(a.tesla.clone().run("unix:path=/nonexistent-cowplay-test-bus".into(), 0, stopped));
        let response = super::call(axum::extract::State(a.clone()), axum::extract::Path("tesla_configure".into()), args.to_string().into()).await;
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(status, axum::http::StatusCode::OK, "{saved}");
        assert_eq!(saved["configured"], true);
        assert_eq!(saved["key_ready"], true);
        let fingerprint = saved["public_key_fingerprint"].clone();
        let response = super::call(axum::extract::State(a.clone()), axum::extract::Path("tesla_configure".into()), serde_json::json!({"vin":null,"enabled":false,"ubus_rpc_session":"synthetic-session"}).to_string().into()).await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let response = super::call(axum::extract::State(a.clone()), axum::extract::Path("tesla_disconnect".into()), serde_json::json!({"ubus_rpc_session":"synthetic-session"}).to_string().into()).await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let response = super::call(axum::extract::State(a.clone()), axum::extract::Path("tesla_configure".into()), serde_json::json!({"enabled":false,"unexpected":true,"ubus_rpc_session":"synthetic-session"}).to_string().into()).await;
        assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
        assert!(super::dispatch(&a, "tesla_configure", &args).await.is_err(), "remote management keeps strict arguments");
        let current = super::dispatch(&a, "tesla_status", &serde_json::json!({})).await.unwrap();
        assert_eq!(current["public_key_fingerprint"], fingerprint);
        let persisted: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.path().join("tesla/config.json")).unwrap()).unwrap();
        assert_eq!(persisted["vin"], args["vin"]);
        assert!(persisted.get("ubus_rpc_session").is_none());
        let socket = dir.path().join("audit.sock");
        let server_admin = a.clone(); let server_socket = socket.clone();
        let server = tokio::spawn(async move { super::serve(server_admin, &server_socket).await.unwrap(); });
        tokio::time::timeout(std::time::Duration::from_secs(1), async { while !socket.exists() { tokio::task::yield_now().await; } }).await.unwrap();
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for method in ["status", "phones", "screens", "logs", "token", "config_export"] {
            let body = serde_json::json!({"ubus_rpc_session":"synthetic-session"}).to_string();
            let mut connection = tokio::net::UnixStream::connect(&socket).await.unwrap();
            connection.write_all(format!("POST /ctl/{method} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            let mut response = String::new();connection.read_to_string(&mut response).await.unwrap();
            assert!(response.starts_with("HTTP/1.1 200"), "{method}: {response}");
        }
        server.abort();
        stop.send(true).unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn port_restart_notice_tracks_the_running_listener_across_reloads() {
        let (manager, _server, hub, dir) = crate::bluetooth::recovery_tests::admin_fixture().await;
        let uci = dir.path().join("cowplay.uci");
        std::fs::write(&uci, "config cowplay 'main'\n option port '18443'\n").unwrap();
        let a = crate::admin::Admin::new(manager, hub, "https://test.invalid:18443".into(), uci.clone(), dir.path().join("config"), dir.path().to_owned(), "cert".into(), "key".into(), "127.0.0.1:18443".parse().unwrap(), "127.0.0.1:7000".parse().unwrap(), None, (800, 480)).expect("valid synthetic policy");
        std::fs::write(&uci, "config cowplay 'main'\n option port '443'\n").unwrap();
        for _ in 0..2 {
            assert_eq!(super::dispatch(&a, "reload", &serde_json::json!({})).await.unwrap()["restart_required"], true, "reload must not hide a port change before the listener is restarted");
        }
        std::fs::write(&uci, "config cowplay 'main'\n option port '18443'\n").unwrap();
        assert_eq!(super::dispatch(&a, "reload", &serde_json::json!({})).await.unwrap()["restart_required"], false, "reverting to the running port needs no restart");
    }

    #[test]
    fn luci_bounds_persistent_and_memory_tails_without_losing_failure_metadata() {
        let bundle=serde_json::json!({"logs":"现".repeat(100_000),"stored_logs":{"tail":"史".repeat(100_000),"truncated":false},"log_storage":{"failures":2},"metrics":{"server":{"frames_in":42}}});
        let result=super::bound_luci_bundle(bundle).unwrap();
        let memory=result["logs"].as_str().unwrap();
        let stored=result["stored_logs"]["tail"].as_str().unwrap();
        assert!(memory.len()+stored.len()<=64*1024);
        assert!(!memory.contains('�')&&!stored.contains('�'));
        assert_eq!(result["stored_logs"]["truncated"],true);
        assert_eq!(result["log_export"]["truncated"],true);
        assert_eq!(result["log_storage"]["failures"],2);
        assert_eq!(result["metrics"]["server"]["frames_in"],42);
    }
    #[test]
    fn luci_bundle_bounds_multibyte_logs_and_reports_truncation() {
        let logs = "连接日志 😀\n".repeat(200_000);
        let original = logs.len();
        let result = super::bound_luci_bundle(serde_json::json!({"kind":"cowplay-diagnostics","logs":logs,"metrics":{"server":{"frames_in":42}}})).unwrap();
        assert!(result["logs"].as_str().unwrap().len() <= 65536);
        assert_eq!(result["log_export"]["truncated"], true);
        assert_eq!(result["log_export"]["original_bytes"], original);
        assert_eq!(result["metrics"]["server"]["frames_in"], 42);
        assert!(serde_json::to_vec(&result).unwrap().len() < 512 * 1024);
        assert!(super::bound_luci_bundle(serde_json::json!({"other":"x".repeat(600_000)})).is_err());
    }

    #[tokio::test]
    async fn maintenance_rejects_busy_ports_invalid_renewal_and_oversized_uploads() {
        let (manager, _server, hub, dir) = crate::bluetooth::recovery_tests::admin_fixture().await;
        let a = crate::admin::Admin::new(manager, hub, "https://test.invalid".into(), dir.path().join("cowplay.uci"), dir.path().join("config"), dir.path().to_owned(), "cert".into(), "key".into(), "127.0.0.1:8080".parse().unwrap(), "127.0.0.1:7000".parse().unwrap(), None, (800, 480)).expect("valid synthetic policy");
        f50_server::diag::record(f50_server::diag::Level::Info, "luci_order_test", "synthetic older log".into(), false);
        f50_server::diag::record(f50_server::diag::Level::Warn, "luci_order_test", "synthetic newer log".into(), false);
        let logs = super::dispatch(&a, "logs", &serde_json::json!({"module":"luci_order_test"})).await.unwrap();
        assert_eq!(logs["entries"][0]["msg"], "synthetic newer log", "LuCI and its filtered export must put the newest log first");
        assert!(logs["lines"][0].as_str().unwrap().contains("synthetic newer log"));
        assert!(logs["entries"][0]["seq"].as_u64().unwrap() > logs["entries"][1]["seq"].as_u64().unwrap());
        let export = super::dispatch(&a, "logs_export", &serde_json::json!({})).await.unwrap();
        let text = export["text"].as_str().unwrap();
        assert!(text.starts_with("CowPlay 运行日志\n"));
        assert!(text.find("synthetic older log").unwrap() < text.find("synthetic newer log").unwrap());
        assert!(text.contains("WARN") && text.contains("luci_order_test"));
        assert!(!text.contains("BEGIN PRIVATE KEY"));
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let conflict = super::dispatch(&a, "network_check", &serde_json::json!({"port":occupied.local_addr().unwrap().port()})).await.unwrap();
        assert_eq!(conflict["available"],false,"occupied ports must return a report, not prevent saving");
        assert_eq!(super::dispatch(&a, "network_check", &serde_json::json!({"port":8080})).await.unwrap()["ok"], true);
        assert!(super::dispatch(&a, "network_check", &serde_json::json!({"port":0})).await.is_err());
        assert!(super::dispatch(&a, "cert", &serde_json::json!({"action":"renew"})).await.unwrap_err().contains("DNS API"));
        let cert = super::dispatch(&a, "cert", &serde_json::json!({"action":"info"})).await.unwrap();
        assert_eq!(cert["domain"], "test.invalid", "empty UCI domain must use the runtime origin");
        assert_eq!(cert["task"]["running"], false);
        assert_eq!(cert["task"]["cert_ok"], false);
        assert!(cert["task"]["cert_result"].as_str().unwrap().contains("DNS API"));
        let error = super::dispatch(&a, "cert_upload", &serde_json::json!({"cert":"x".repeat(49_153),"key":"synthetic"})).await.unwrap_err();
        assert!(error.contains("48 KiB"));
        assert_eq!(*a.tls.lock().unwrap(), ("cert".into(), "key".into()));
        let generated = rcgen::generate_simple_self_signed(vec!["old.example.test".into()]).unwrap();
        let issued = crate::acme::Issued {chain_pem:generated.cert.pem(),key_pem:generated.signing_key.serialize_pem()};
        let (old_cert,old_key)=crate::certificates::store_pair(&a.state_dir,&issued,false).unwrap();
        let previous=a.state_dir.join("tls-previous.json");
        let record=serde_json::to_vec(&serde_json::json!({"cert":old_cert,"key":old_key,"source":"upload"})).unwrap();
        std::fs::write(&previous,&record).unwrap();
        assert!(super::dispatch(&a,"cert",&serde_json::json!({"action":"rollback"})).await.unwrap_err().contains("域名"));
        assert_eq!(*a.tls.lock().unwrap(),("cert".into(),"key".into()),"a previous certificate for another domain must not replace the current pair");
        assert_eq!(std::fs::read(previous).unwrap(),record);
        assert_eq!(a.policy().cert_source,"acme");
    }

    #[tokio::test]
    async fn root_reset_cannot_be_superseded_by_automatic_connect() {
        use std::sync::atomic::Ordering;
        let (manager, _server, hub, dir, disconnects) = crate::bluetooth::recovery_tests::admin_fixture_slow_disconnect(400).await;
        let a = crate::admin::Admin::new(manager, hub.clone(), "https://test.invalid".into(), dir.path().join("cowplay.uci"), dir.path().join("config"), dir.path().to_owned(), "cert".into(), "key".into(), "127.0.0.1:8080".parse().unwrap(), "127.0.0.1:7000".parse().unwrap(), None, (800, 480)).expect("valid synthetic policy");
        let reset = tokio::spawn({ let a = a.clone(); async move { super::dispatch(&a, "bluetooth_reset", &serde_json::json!({})).await } });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let epoch = hub.phones.epoch();
        crate::console::run_cmd(&a, crate::console::Cmd { cmd: "connect".into(), id: None, generation:None }).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(hub.phones.epoch(), epoch, "automatic connection must not replace a root-requested reset");
        assert_eq!(reset.await.unwrap().unwrap()["ok"], true);
        assert!(disconnects() >= 1, "the request released the old phone");
        assert!(a.stopped.load(Ordering::Acquire), "explicit phone selection resumes after a reset");
    }

    #[test]
    fn dechunk_joins_chunks() {
        assert_eq!(super::dechunk("4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n"), "{\"a\":1}");
    }
}
