use serde_json::{Value, json};

fn numbers(v: &Value, fields: &[&str]) -> Value {
    let mut out = serde_json::Map::new();
    for &key in fields {
        if v[key].is_number() || v[key].is_boolean() || v[key].is_null() { out.insert(key.into(),v[key].clone()); }
    }
    Value::Object(out)
}
fn choice(v: &Value, allowed: &[&str]) -> Value {
    v.as_str().filter(|s| allowed.contains(s)).map(|s|json!(s)).unwrap_or(Value::Null)
}
fn items(v: &Value) -> impl Iterator<Item=&Value> { v.as_array().into_iter().flatten() }

pub(super) fn shareable(v: &Value) -> Value {
    let ids: Vec<_> = items(&v["sessions"]).filter_map(|s|s["id"].as_str()).collect();
    let session_id = |id:&Value| id.as_str().and_then(|s|ids.iter().position(|id|*id==s)).map(|i|format!("session-{}",i+1));
    let context = |c:&Value| {
        let mut out=numbers(c,&["epoch","viewer_generation"]);
        out["session"]=json!(session_id(&c["session"]));
        out["phone_connected"]=json!(c["phone"].is_string());
        out["viewer_connected"]=json!(c["viewer"].is_string());
        out
    };
    let test = |t:&Value| {
        if t.is_null() { return Value::Null; }
        let mut out=numbers(t,&["ts_ms","started_ms","age_ms","context_current","passed","warned","failed"]);
        out["context"]=context(&t["context"]);
        out["source"]=choice(&t["source"],&["fresh","cached","invalidated"]);
        out["checks"]=json!(items(&t["checks"]).map(|c|json!({
            "id":choice(&c["id"],&["service","bluetooth","wifi","uplink","dns","tls","identity","video","storage","browser","fullscreen","link","time","log_storage"]),
            "status":choice(&c["status"],&["ok","warn","fail","idle","unknown"])
        })).collect::<Vec<_>>());
        out
    };
    let mut check=numbers(&v["connection_check"],&["ts_ms","age_ms","ms","passed","skipped","unknown","total","context_current"]);
    check["context"]=context(&v["connection_check"]["context"]);
    check["steps"]=json!(items(&v["connection_check"]["steps"]).enumerate().map(|(i,c)|json!({"step_index":i,"state":choice(&c["state"],&["ok","failed","skipped","unknown"]),"ok":c["ok"].as_bool()})).collect::<Vec<_>>());
    let sessions:Vec<_>=items(&v["sessions"]).map(|s|{
        let mut out=numbers(s,&["started_ms","ended_ms","first_frame_ms","stalls","warnings"]);
        out["id"]=json!(session_id(&s["id"]));
        out["result"]=choice(&s["end_reason"],&["用户切换或断开手机","协议或传输错误","iPhone 结束会话","服务停止","蓝牙连接失败"]);
        let f=&s["transport_failure"];
        if f.is_object() {
            let mut failure=numbers(f,&["ts_ms","expected_bytes","read_budget_ms","configured","received_frame"]);
            failure["stage"]=choice(&f["stage"],&["header","body"]);
            failure["kind"]=choice(&f["kind"],&["TimedOut","UnexpectedEof","ConnectionReset","ConnectionAborted","BrokenPipe","Other"]);
            failure["partial_bytes_known"]=json!(false);
            out["transport_failure"]=failure;
        }
        out
    }).collect();
    let st=if v["stats"].is_object() {&v["stats"]} else {&v["overview"]["stats"]};
    let metrics=if st.is_object() {st} else {&v["metrics"]};
    let report=if metrics["car"].is_object(){&metrics["car"]}else{&metrics["car_last_report"]};
    let mut car=numbers(report,&["fps","rtt_ms","jitter_ms","loss_pct","decode_ms","decoded","dropped","width","height","visible"]);
    car["ping"]=numbers(&report["ping"],&["rtt_ms","age_ms"]);
    car["sample_age_ms"]=metrics["car_sample_age_ms"].as_u64().map_or(Value::Null,|x|json!(x));
    let mut link=numbers(&v["overview"]["link"],&["updated_ms"]);
    for name in ["bluetooth","wifi","carplay"] {link[name]=choice(&v["overview"]["link"][name],&["idle","active","ok","fail"]);}
    link["error"]=json!({"step":choice(&v["overview"]["link"]["error"]["step"],&["bluetooth","wifi","carplay"]),"ts_ms":v["overview"]["link"]["error"]["ts_ms"].as_u64()});
    let mut hub=numbers(&v["overview"]["hub"],&["phone","viewer","video","verified"]);
    hub["display"]=choice(&v["overview"]["hub"]["display"],&["BROWSER_FRAME_RENDERED","WAITING_BROWSER_REPORT","BROWSER_DISPLAY_ERROR"]);
    json!({
        "kind":"cowplay-diagnostics", "schema":2,
        "build":f50_server::build_info::BUILD_ID,"version":env!("CARGO_PKG_VERSION"),
        "generated_ms":v["generated_ms"].as_u64(),"started_ms":v["started_ms"].as_u64(),
        "context":context(&v["context"]),"context_current":v["context_current"].as_bool(),
        "selftest":test(&v["selftest"]),"connection_check":if v["connection_check"].is_null(){Value::Null}else{check},
        "overview":{"hub":hub,"link":link,"spec":numbers(&v["overview"]["display"]["spec"],&["width","height","fps","hevc"]),"selftest":test(&v["overview"]["selftest"])},
        "diagnosis":{"level":choice(&v["diagnosis"]["verdict"]["level"],&["ok","unknown","warn","error"]),"selftest":test(&v["diagnosis"]["selftest"])},
        "sessions":sessions,
        "metrics":{"car":car,"server":numbers(&metrics["server"],&["frames_in","bytes_in","fps_in","bitrate_bps","dropped","keyframe_requests","since_last_frame_ms","phone_rtt_ms"]),"f50":numbers(&metrics["f50"],&["cpu","temp","mem_free_mb","mem_total_mb","sample_age_ms"])},
        "process":numbers(&v["process"],&["pid","threads","rss_kb","cpu_percent"]),
        "system":{"arch":std::env::consts::ARCH},
        "log_storage":numbers(&v["log_storage"],&["active","queued","written","queue_dropped","failures","sync_interval_ms","last_sync_ms","sync_age_ms","file_limit_bytes","archive_limit_bytes","queue_capacity_lines"]),
        "memory_log_export":numbers(&v["memory_log_export"],&["first_ms","last_ms","evicted","truncated","oldest_seq","last_seq"]),
        "redaction":"仅导出允许的状态、时间和数值；不含设备名称、地址、网络、车辆、证书、私钥、配置或日志自由文本。传输故障见 sessions.transport_failure。"
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_fields_and_free_text_never_enter_shareable_diagnostics() {
        let secret="PRIVATE-CANARY-192.0.2.1-ssid-vin-domain-key";
        let raw=json!({"context":{"phone":secret,"viewer":secret,"session":"secret-id","epoch":2},"selftest":{"context":{"session":"secret-id"},"source":"fresh","checks":[{"id":"tls","status":"ok","detail":secret}]},"overview":{"hub":{"phone":true,"protocol":secret},"link":{"bluetooth":"ok","bluetooth_detail":secret},"stats":{"car":{"fps":0,"rtt_ms":9.3,"decoder":secret},"f50":{"cpu":2,"wifi":{"ssid":secret}}}},"sessions":[{"id":"secret-id","phone":secret,"peer":secret,"model":secret,"events":[{"detail":secret}],"transport_failure":{"stage":"header","kind":"TimedOut","expected_bytes":128,"read_budget_ms":15000,"received_frame":true,"extra":secret}}],"new_unknown_field":secret,"config":secret,"logs":secret,"stored_logs":{"tail":secret},"failure_context":{"memory":secret},"private_key":secret});
        let out=shareable(&raw);let text=out.to_string();
        assert!(!text.contains(secret)&&!text.contains("secret-id"));
        assert_eq!(out["context"]["session"],"session-1");
        assert_eq!(out["sessions"][0]["transport_failure"]["expected_bytes"],128);
        assert_eq!(out["sessions"][0]["transport_failure"]["kind"],"TimedOut");
        assert_eq!(out["metrics"]["car"]["fps"],0);
        assert_eq!(out["metrics"]["car"]["rtt_ms"],9.3);
        assert!(out.get("config").is_none()&&out.get("logs").is_none());
        let mut browser_failure=raw;
        browser_failure["overview"]["hub"]["display"]=json!("BROWSER_DISPLAY_ERROR");
        assert_eq!(shareable(&browser_failure)["overview"]["hub"]["display"],"BROWSER_DISPLAY_ERROR");
    }
}
