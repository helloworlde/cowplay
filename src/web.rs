use crate::bluetooth::Manager;
use axum::{
    Router,
    extract::{Json, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use std::sync::Arc;
pub struct Web {
    pub manager: Arc<Manager>,
    pub hub: Arc<f50_server::Hub>,
}
/// An authorized browser (client cookie); mutations also need the same Origin.
pub(crate) fn authorized(hub: &f50_server::Hub, headers: &HeaderMap, mutation: bool) -> bool {
    hub.clients.from_headers(headers).is_some() && (!mutation || f50_server::auth::same_origin(hub, headers))
}
fn response(result: crate::RuntimeResult<()>) -> Response {
    match result {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (StatusCode::CONFLICT, Json(serde_json::json!({"error":error.to_string()}))).into_response(),
    }
}
async fn status(State(web): State<Arc<Web>>, headers: HeaderMap) -> Response {
    if !authorized(&web.hub, &headers, false) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let (phones,adapter)=tokio::join!(web.manager.device_lists(),web.manager.adapter_info());
    let snapshot=web.manager.status();
    let mut status=serde_json::to_value(&snapshot).unwrap();
    status["adapter_name"]=serde_json::json!(adapter.as_ref().ok().map(|a|&a.controller_name));
    status["adapter_error"]=serde_json::json!(adapter.as_ref().err().map(|e|e.to_string()));
    status["adapter"]=serde_json::json!(adapter.ok());
    match phones {
        Ok((mut phones,nearby))=>{for p in &mut phones {p.selected=snapshot.selected.as_deref()==Some(p.id.as_str());} status["devices"]=serde_json::json!(phones); status["nearby"]=serde_json::json!(nearby);},
        Err(_)=>{status["devices"]=serde_json::json!([]);status["nearby"]=serde_json::json!([]);status["devices_error"]=serde_json::json!("暂时无法读取蓝牙设备列表，请重试。");}
    }
    status["selection_epoch"]=serde_json::json!(web.hub.phones.epoch());
    status["carplay"]=serde_json::json!(if web.hub.phone_verified.load(std::sync::atomic::Ordering::Acquire){"verified"}else if web.hub.has_phone_session(){"connecting"}else{"idle"});
    Json(status).into_response()
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Confirmation {
    generation: u64,
    request: u64,
    accept: bool,
}
async fn confirm(
    State(web): State<Arc<Web>>,
    headers: HeaderMap,
    Json(c): Json<Confirmation>,
) -> Response {
    if !authorized(&web.hub, &headers, true) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let r = web.manager.confirm(c.generation, c.request, c.accept);
    f50_server::diag_log!(Info, "bt", "pairing confirmation accept={} ok={}", c.accept, r.is_ok());
    response(r)
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PairDevice { device:String, generation:u64 }
async fn pair_device(State(web):State<Arc<Web>>,headers:HeaderMap,body:axum::body::Bytes)->Response {
    if !authorized(&web.hub,&headers,true) {return StatusCode::UNAUTHORIZED.into_response()}
    if body.len()>512 {return StatusCode::BAD_REQUEST.into_response()}
    let selection:PairDevice=match serde_json::from_slice(&body) {Ok(v)=>v,Err(_)=>return StatusCode::BAD_REQUEST.into_response()};
    response(crate::phone_flow::pair_device(&web.manager,&web.hub,&selection.device,selection.generation).await)
}

pub fn router(manager: Arc<Manager>, hub: Arc<f50_server::Hub>) -> Router {
    Router::new()
        .route("/api/pairing", get(status))
        .route("/api/pairing/confirm", post(confirm))
        .route("/api/pairing/pair", post(pair_device))
        .with_state(Arc::new(Web { manager, hub }))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn client_cookie_and_origin_are_both_required_for_mutation() {
        let (hub, _) = f50_server::Hub::new("123456".into(), "https://test.invalid".into());
        let client = hub.clients.admit_token("123456", "phone", "", "", "").unwrap();
        let mut h = HeaderMap::new();
        assert!(!authorized(&hub, &h, false));
        h.insert("cookie", format!("{}={}", f50_server::clients::COOKIE, client.id).parse().unwrap());
        assert!(authorized(&hub, &h, false));
        assert!(!authorized(&hub, &h, true));
        h.insert("origin", "https://test.invalid".parse().unwrap());
        assert!(authorized(&hub, &h, true));
        h.insert("origin", "https://foreign.invalid".parse().unwrap());
        assert!(!authorized(&hub, &h, true));
        h.insert("authorization", "Bearer 123456".parse().unwrap());
        h.remove("cookie");
        assert!(!authorized(&hub, &h, false), "the bare token is no longer a credential");
    }
}
