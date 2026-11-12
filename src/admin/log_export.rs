//! Administrator log downloads are plain text; shareable diagnostic snapshots stay separate.
use super::Admin;
use f50_server::diag::{self, LogQuery, diag};
use serde_json::{Value, json};
use std::fmt::Write;

const LOG_BYTES: usize = 64 * 1024;

// Preserve whole log lines and UTF-8 when dropping older records.
fn tail(text: &str, budget: usize) -> (&str, bool) {
    if text.len() <= budget { return (text, false); }
    let mut from = text.len() - budget;
    while !text.is_char_boundary(from) { from += 1; }
    if from > 0 && !text[..from].ends_with('\n') {
        from = text[from..].find('\n').map_or(text.len(), |n| from + n + 1);
    }
    (&text[from..], true)
}

fn text_logs(memory: &diag::LogPage, stored: &Value, storage: &Value) -> String {
    let memory_text = memory.entries.iter().rev().map(diag::format_line).collect::<String>();
    let persistent = stored["tail"].as_str().unwrap_or("");
    let (current, memory_cut) = tail(&memory_text, if persistent.is_empty() { LOG_BYTES } else { LOG_BYTES / 2 });
    let (persistent, disk_cut) = tail(persistent, LOG_BYTES - current.len());
    let mut out = format!("CowPlay 运行日志\n版本：{} · 构建：{}\n时间：UTC；每条记录含 ts_ms、service、seq。各来源按时间正序。\n范围：最多 64 KiB 日志正文，不受页面筛选影响；不包含配置、证书和私钥文件。\n当前进程与磁盘可能记录同一事件，请按 service / seq / ts_ms 识别。\n\n", env!("CARGO_PKG_VERSION"), f50_server::build_info::BUILD_ID);
    let _ = writeln!(out, "=== 当前进程内存日志 ===\n内存共 {} 条；已淘汰 {} 条；本次{}。", memory.total, memory.evicted,
        if memory_cut || memory.matched > memory.entries.len() || memory.evicted > 0 { "仅包含最近记录，较早日志已截断或淘汰" } else { "包含全部可用内存记录" });
    out.push_str(if current.is_empty() { "（没有可导出的内存记录）\n" } else { current });
    out.push_str("\n=== 已保留磁盘日志 ===\n");
    if let Some(error) = stored["error"].as_str() {
        let _ = writeln!(out, "读取失败：{error}");
    } else if stored["enabled"] != true {
        out.push_str("未启用持久日志；内存日志不会跨服务重启保留。\n");
    } else {
        let _ = writeln!(out, "本次{}。", if disk_cut || stored["truncated"] == true { "仅包含最近记录，较早日志已截断" } else { "包含当前保留文件中的可用记录" });
        out.push_str(if persistent.is_empty() { "（没有可导出的磁盘记录）\n" } else { persistent });
    }
    let _ = writeln!(out, "\n=== 日志写入状态 ===\n队列丢弃：{} 行；写入失败：{} 次。", storage["queue_dropped"].as_u64().unwrap_or(0), storage["failures"].as_u64().unwrap_or(0));
    // Log writers already mask MACs. Mask again for older on-disk records.
    diag::mask_macs(&out)
}

impl Admin {
    pub async fn logs_export(&self) -> Result<Value, String> {
        let stored = self.stored_logs().await;
        let logs = diag().logs(&LogQuery { limit: Some(5000), ..Default::default() });
        let value = json!({"text": text_logs(&logs, &stored, &diag().storage_status())});
        if serde_json::to_vec(&value).map_err(|e| e.to_string())?.len() > 512 * 1024 {
            return Err("日志超过 LuCI 传输大小，请重试".into());
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_page() -> diag::LogPage {
        diag::LogPage { entries: vec![], matched: 0, total: 0, bytes: 0, cap_bytes: 4 * 1024 * 1024,
            evicted: 0, oldest_seq: None, oldest_ms: None, last_seq: 0 }
    }

    #[test]
    fn text_download_preserves_failures_sources_and_truncation_without_config() {
        let mut page = empty_page();
        page.entries = vec![diag::Entry { service: "synthetic".into(), seq: 2, ts_ms: 1234,
            level: diag::Level::Error, module: "video", msg: "video header timeout aa:bb:cc:dd:ee:ff".into() }];
        page.total = 1; page.matched = 1; page.evicted = 7;
        let stored = json!({"enabled":true,"truncated":true,"tail":"00:00:00.001 WARN bt synthetic reconnect failed\n", "config":"PRIVATE-CONFIG-CANARY", "private_key":"PRIVATE-KEY-CANARY"});
        let text = text_logs(&page, &stored, &json!({"queue_dropped":3,"failures":1}));
        for expected in ["ERROR", "video header timeout", "synthetic reconnect failed", "已淘汰 7 条", "较早日志已截断", "队列丢弃：3 行", "写入失败：1 次", "service=synthetic seq=2"] { assert!(text.contains(expected), "{expected}"); }
        assert!(!text.contains("aa:bb:cc:dd:ee:ff") && text.contains("aa:bb:cc:••:••:ff"));
        assert!(!text.contains("PRIVATE-CONFIG-CANARY") && !text.contains("PRIVATE-KEY-CANARY"));
    }

    #[test]
    fn bounded_tails_keep_complete_utf8_lines_and_report_missing_disk_logs() {
        let large = "连接失败 😀\n".repeat(10_000);
        let (text, cut) = tail(&large, 32 * 1024);
        assert!(cut && text.len() <= 32 * 1024);
        assert!(text.starts_with("连接失败") && text.ends_with('\n'));
        let page = empty_page();
        assert!(text_logs(&page, &json!({"enabled":false}), &json!({})).contains("未启用持久日志"));
        assert!(text_logs(&page, &json!({"enabled":true,"error":"synthetic disk unavailable"}), &json!({})).contains("读取失败：synthetic disk unavailable"));
    }
}
