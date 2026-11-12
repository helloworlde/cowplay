use super::{Admin, stamp};
use axum::{Router, extract::{Query, State}, http::{HeaderMap, StatusCode, header}, response::{IntoResponse, Response}, routing::get};
use f50_server::diag::{LogQuery, diag};
#[cfg(test)]
use f50_server::diag;
use serde_json::{Value, json};
use std::sync::Arc;

// ---------- HTTP layer ----------

fn no_store(body: impl IntoResponse) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], body).into_response()
}
pub(crate) fn json_ok(v: Value) -> Response {
    no_store(axum::Json(v))
}
/// `{"error": msg}` with the given status; every page shows `error` as the reason.
pub(crate) fn fail(code: StatusCode, msg: impl Into<String>) -> Response {
    no_store((code, axum::Json(json!({"error": msg.into()}))))
}
fn attachment(name: &str, mime: &'static str, body: Vec<u8>) -> Response {
    (
        [
            (header::CONTENT_TYPE, mime.to_owned()),
            (header::CACHE_CONTROL, "no-store".to_owned()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
        ],
        body,
    )
        .into_response()
}
macro_rules! guard {
    ($a:expr, $h:expr, $m:expr) => {
        if !crate::web::authorized(&$a.hub, &$h, $m) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
    };
}

async fn logs(State(a): State<Arc<Admin>>, h: HeaderMap, Query(q): Query<LogQuery>) -> Response {
    guard!(a, h, false);
    json_ok(serde_json::to_value(diag().logs(&q)).unwrap_or(Value::Null))
}
async fn logs_download(State(a): State<Arc<Admin>>, h: HeaderMap) -> Response {
    guard!(a, h, false);
    let body=serde_json::to_vec_pretty(&a.bundle().await).unwrap_or_default();
    attachment(&format!("cowplay-support-{}.json",stamp()),"application/json",body)
}

#[cfg(test)]
fn memory_log_supplement(entries: &[diag::Entry], tail: &str) -> (String, usize) {
    let persistent: std::collections::HashSet<_> = tail.lines().collect();
    let mut current = String::new();
    let mut duplicates = 0;
    for e in entries.iter().rev() {
        let line = diag::mask_macs(&diag::format_line(e));
        if persistent.contains(line.trim_end_matches('\n')) { duplicates += 1; } else { current.push_str(&line); }
    }
    (current,duplicates)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overlap_is_not_a_second_failure_and_independent_events_survive() {
        let mut entry = diag::Entry {service:"synthetic-service".into(),seq:1,ts_ms:1000,level:diag::Level::Warn,module:"test",msg:"phone 00:11:22:33:44:55 timeout".into()};
        let persistent=diag::mask_macs(&diag::format_line(&entry));
        let (extra,duplicates)=memory_log_supplement(&[entry.clone()],&persistent);
        assert!(extra.is_empty()); assert_eq!(duplicates,1);
        entry.seq=2;
        let (extra,duplicates)=memory_log_supplement(&[entry],&persistent);
        assert!(extra.contains("seq=2")); assert_eq!(duplicates,0);
        assert!(!extra.contains("33:44"));
    }
}
async fn system(State(a): State<Arc<Admin>>, h: HeaderMap) -> Response {
    guard!(a, h, false);
    json_ok(a.system().await)
}
async fn bundle(State(a): State<Arc<Admin>>, h: HeaderMap) -> Response {
    guard!(a, h, false);
    let body = serde_json::to_vec_pretty(&a.bundle().await).unwrap_or_default();
    attachment(&format!("cowplay-diag-{}.json", stamp()), "application/json", body)
}
async fn ping(State(a): State<Arc<Admin>>, h: HeaderMap) -> Response {
    guard!(a, h, false);
    no_store(StatusCode::NO_CONTENT)
}
/// Endpoints the car and phone pages use; maintenance lives behind LuCI (`ctl`).
pub fn router(admin: Arc<Admin>) -> Router {
    Router::new()
        .route("/api/logs", get(logs))
        .route("/api/logs/download", get(logs_download))
        .route("/api/system", get(system))
        .route("/api/diag/bundle", get(bundle))
        .route("/api/ping", get(ping))
        .with_state(admin)
}
