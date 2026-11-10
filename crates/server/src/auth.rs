//! Browser authorization endpoints: token keypad, QR tickets, and the open (no-token) policy.
use crate::{
    clients::{Client, Denied},
    Hub,
};
use axum::{
    body::Bytes,
    extract::{ConnectInfo, State},
    http::{header, Extensions, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use std::{net::SocketAddr, sync::Arc};

pub fn router() -> Router<Arc<Hub>> {
    Router::new().route("/api/auth", get(status).post(admit)).route("/api/auth/ticket", post(ticket))
}
/// Same-origin check for state-changing requests (cookies are SameSite=Strict as well).
pub fn same_origin(hub: &Hub, headers: &HeaderMap) -> bool {
    headers.get("origin").and_then(|v| v.to_str().ok()) == Some(hub.origin())
}
pub fn peer_ip(ext: &Extensions) -> String {
    ext.get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip().to_string()).unwrap_or_default()
}
fn no_store(mut r: Response) -> Response {
    r.headers_mut().insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    r
}
fn admitted(hub: &Hub, c: &Client) -> Response {
    let mut r = Json(serde_json::json!({"authorized":true,"client":c.public()})).into_response();
    r.headers_mut().insert(header::SET_COOKIE, hub.clients.cookie(c).parse().unwrap());
    no_store(r)
}
async fn status(State(hub): State<Arc<Hub>>, headers: HeaderMap) -> Response {
    let client = hub.clients.from_headers(&headers);
    no_store(
        Json(serde_json::json!({
            "authorized": client.is_some(),
            "client": client.map(|c| c.public()),
            "required": hub.clients.policy().required,
            "locked_s": hub.clients.locked_s(),
        }))
        .into_response(),
    )
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Admit {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    ticket: Option<String>,
    kind: String,
    #[serde(default)]
    name: String,
}
async fn admit(State(hub): State<Arc<Hub>>, headers: HeaderMap, ext: Extensions, body: Bytes) -> Response {
    if !same_origin(&hub, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(c) = hub.clients.from_headers(&headers) {
        return admitted(&hub, &c);
    }
    let Ok(a) = serde_json::from_slice::<Admit>(&body) else { return StatusCode::BAD_REQUEST.into_response() };
    let ua = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("");
    let ip = peer_ip(&ext);
    if let Some(t) = a.ticket.as_deref() {
        return match hub.clients.admit_ticket(t, &a.kind, &a.name, ua, &ip) {
            Some(c) => {
                crate::diag::diag().event("ok", "web", format!("{} 已通过扫码授权", if c.kind == "phone" { "手机" } else { "车机屏幕" }));
                admitted(&hub, &c)
            }
            None => (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error":"二维码已过期，请在车机上重新打开二维码。"}))).into_response(),
        };
    }
    if let Some(c) = hub.clients.admit_open(&a.kind, &a.name, ua, &ip) {
        return admitted(&hub, &c);
    }
    let Some(token) = a.token.as_deref() else {
        return no_store((StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error":"需要访问令牌","required":true}))).into_response());
    };
    match hub.clients.admit_token(token, &a.kind, &a.name, ua, &ip) {
        Ok(c) => {
            crate::diag::diag().event("ok", "web", format!("{}已授权 · 令牌验证通过", if c.kind == "phone" { "手机" } else { "这块屏幕" }));
            admitted(&hub, &c)
        }
        Err(Denied::Wrong { remaining }) => {
            crate::diag::diag().event("warn", "web", format!("访问令牌错误，还可再试 {remaining} 次"));
            (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error":format!("令牌不对，还可以再试 {remaining} 次"),"remaining":remaining}))).into_response()
        }
        Err(Denied::Locked { retry_after_s }) => {
            crate::diag::diag().event("warn", "web", "访问令牌连续错误，已锁定 10 分钟");
            (StatusCode::LOCKED, Json(serde_json::json!({"error":format!("错误次数太多，{} 分钟后再试", retry_after_s.div_ceil(60)),"retry_after_s":retry_after_s}))).into_response()
        }
        Err(Denied::Invalid) => StatusCode::BAD_REQUEST.into_response(),
    }
}
async fn ticket(State(hub): State<Arc<Hub>>, headers: HeaderMap) -> Response {
    if !same_origin(&hub, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if hub.clients.from_headers(&headers).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match hub.clients.ticket() {
        Ok(t) => no_store(Json(serde_json::json!({"ticket":t,"expires_s":300})).into_response()),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
