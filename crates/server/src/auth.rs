//! Explicit first-use access choice and per-browser authorization.
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
    Router::new().route("/api/auth", get(status).post(admit)).route("/api/auth/setup", post(setup))
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
    if !hub.clients.available() { return no_store(StatusCode::SERVICE_UNAVAILABLE.into_response()); }
    let client = hub.clients.from_headers(&headers);
    no_store(
        Json(serde_json::json!({
            "authorized": client.is_some(),
            "client": client.map(|c| c.public()),
            "required": hub.clients.policy().required,
            "setup_required": hub.clients.setup_required(),
            "locked_s": hub.clients.locked_s(),
        }))
        .into_response(),
    )
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Admit {
    #[serde(default)]
    mode: Option<SetupMode>,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    confirmation: Option<String>,
    kind: String,
    #[serde(default)]
    name: String,
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum SetupMode { Code, Open }
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
    match hub.clients.admit_browser(a.token.as_deref(), crate::clients::cookie_id(&headers).as_deref(), &a.kind, &a.name, ua, &ip) {
        Ok(c) => {
            crate::diag::diag().event("ok", "web", format!("{}已进入 CowPlay", if c.kind == "phone" { "手机" } else { "这块屏幕" }));
            admitted(&hub, &c)
        }
        Err(Denied::Wrong { remaining }) => {
            crate::diag::diag().event("warn", "web", format!("访问码错误，还可再试 {remaining} 次"));
            (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error":format!("访问码不对，还可以再试 {remaining} 次"),"remaining":remaining}))).into_response()
        }
        Err(Denied::Locked { retry_after_s }) => {
            crate::diag::diag().event("warn", "web", "访问码连续错误，已锁定 10 分钟");
            (StatusCode::LOCKED, Json(serde_json::json!({"error":format!("错误次数太多，{} 分钟后再试", retry_after_s.div_ceil(60)),"retry_after_s":retry_after_s}))).into_response()
        }
        Err(Denied::Invalid) => StatusCode::BAD_REQUEST.into_response(),
    }
}
async fn setup(State(hub): State<Arc<Hub>>, headers: HeaderMap, ext: Extensions, body: Bytes) -> Response {
    if !same_origin(&hub, &headers) { return StatusCode::FORBIDDEN.into_response(); }
    let Ok(a) = serde_json::from_slice::<Admit>(&body) else { return StatusCode::BAD_REQUEST.into_response() };
    let token = match a.mode.unwrap_or(SetupMode::Code) {
        SetupMode::Open if a.token.is_none() && a.confirmation.is_none() => crate::clients::NO_CODE,
        SetupMode::Code if a.token.as_deref().is_some_and(crate::clients::valid_token) && a.token == a.confirmation => a.token.as_deref().unwrap(),
        _ => return no_store((StatusCode::BAD_REQUEST, Json(serde_json::json!({"error":"请选择不设访问码，或输入两次相同的六位数字访问码"}))).into_response()),
    };
    let ua = headers.get(header::USER_AGENT).and_then(|v|v.to_str().ok()).unwrap_or("");
    match hub.clients.setup(token, &a.kind, &a.name, ua, &peer_ip(&ext)) {
        Ok(c) => admitted(&hub, &c),
        Err(message) => no_store((StatusCode::CONFLICT, Json(serde_json::json!({"error":message}))).into_response()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    async fn post(hub: &Arc<Hub>, path: &str, origin: &str, body: serde_json::Value) -> Response {
        router().with_state(hub.clone()).oneshot(Request::post(path).header("origin",origin).header("content-type","application/json").body(Body::from(body.to_string())).unwrap()).await.unwrap()
    }
    #[tokio::test]
    async fn explicit_open_choice_admits_new_browsers_but_is_not_a_setup_or_code_bypass() {
        let (hub,_)=Hub::new("".into(),"https://test.invalid".into());
        let body=serde_json::json!({"mode":"open","kind":"screen"});
        assert_eq!(post(&hub,"/api/auth/setup","https://foreign.invalid",body.clone()).await.status(),StatusCode::FORBIDDEN);
        assert_eq!(post(&hub,"/api/auth/setup",hub.origin(),serde_json::json!({"mode":"open","kind":"screen","token":"234567"})).await.status(),StatusCode::BAD_REQUEST);
        let first=post(&hub,"/api/auth/setup",hub.origin(),body.clone()).await;assert_eq!(first.status(),StatusCode::OK);
        let cookie=first.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap().split(';').next().unwrap().to_owned();
        assert_eq!(post(&hub,"/api/auth/setup",hub.origin(),body).await.status(),StatusCode::CONFLICT);
        assert_eq!(post(&hub,"/api/auth",hub.origin(),serde_json::json!({"kind":"phone"})).await.status(),StatusCode::OK);
        hub.clients.set_token("234567").unwrap();
        assert_eq!(post(&hub,"/api/auth",hub.origin(),serde_json::json!({"kind":"phone","mode":"open"})).await.status(),StatusCode::UNAUTHORIZED);
        let verified=router().with_state(hub.clone()).oneshot(Request::post("/api/auth").header("origin",hub.origin()).header("cookie",&cookie).body(Body::from(r#"{"kind":"screen","token":"234567"}"#)).unwrap()).await.unwrap();
        assert_eq!(verified.status(),StatusCode::OK);assert!(verified.headers()[header::SET_COOKIE].to_str().unwrap().starts_with(&cookie));
    }
    #[tokio::test]
    async fn first_use_and_each_new_browser_require_the_receiver_code() {
        let (hub,_)=Hub::new("".into(),"https://test.invalid".into());
        let setup=serde_json::json!({"kind":"screen","token":"234567","confirmation":"234567"});
        assert_eq!(post(&hub,"/api/auth/setup","https://foreign.invalid",setup.clone()).await.status(),StatusCode::FORBIDDEN);
        assert_eq!(post(&hub,"/api/auth/setup",hub.origin(),serde_json::json!({"kind":"screen","token":"234567","confirmation":"345678"})).await.status(),StatusCode::BAD_REQUEST);
        let first=post(&hub,"/api/auth/setup",hub.origin(),setup.clone()).await;
        assert_eq!(first.status(),StatusCode::OK);assert!(first.headers().contains_key(header::SET_COOKIE));
        assert_eq!(post(&hub,"/api/auth/setup",hub.origin(),setup).await.status(),StatusCode::CONFLICT);
        hub.clients.set_policy(crate::clients::Policy{required:false,remember_days:None},Default::default());
        for body in [serde_json::json!({"kind":"phone"}),serde_json::json!({"kind":"phone","token":"345678"})] {
            assert_eq!(post(&hub,"/api/auth",hub.origin(),body).await.status(),StatusCode::UNAUTHORIZED);
        }
        assert_eq!(post(&hub,"/api/auth",hub.origin(),serde_json::json!({"kind":"phone","ticket":"old-ticket"})).await.status(),StatusCode::BAD_REQUEST);
        assert_eq!(post(&hub,"/api/auth",hub.origin(),serde_json::json!({"kind":"phone","token":"234567"})).await.status(),StatusCode::OK);
        assert_eq!(post(&hub,"/api/auth/ticket",hub.origin(),serde_json::json!({})).await.status(),StatusCode::NOT_FOUND);
    }
}
