use super::{Admin, stamp};
use axum::{Router, extract::{Query, State}, http::{HeaderMap, StatusCode, header}, response::{IntoResponse, Response}, routing::get};
use f50_server::diag::{self, LogQuery, diag};
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
async fn logs_download(State(a): State<Arc<Admin>>, h: HeaderMap, Query(mut q): Query<LogQuery>) -> Response {
    guard!(a, h, false);
    q.limit = Some(5000);
    let page = diag().logs(&q);
    let current: String = page.entries.iter().rev().map(diag::format_line).collect();
    let mut stored = a.stored_logs().await;
    let tail = stored.get_mut("tail").map(Value::take).and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
    let metadata = json!({"storage":diag().storage_status(),"stored_export":stored,"memory":{"evicted":page.evicted,"oldest_seq":page.oldest_seq,"last_seq":page.last_seq,"truncated":page.matched>page.entries.len()||page.evicted>0}});
    let text = diag::mask_macs(&format!("# CowPlay log export {}\n# {}\n# 持久日志尾部\n{}\n# 当前进程内存尾部（可能与上段重复）\n{}", diag::now_ms(), metadata, tail, current));
    attachment(&format!("cowplay-{}.log", stamp()), "text/plain; charset=utf-8", text.into_bytes())
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
