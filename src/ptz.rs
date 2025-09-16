use crate::auth::check_auth;
use crate::AppState;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::sync::Arc;
use url::Url;

#[derive(serde::Serialize)]
pub struct ApiResponse { pub status: String, pub message: String }

const SOAP_ENV: &str = "http://www.w3.org/2003/05/soap-envelope"; // SOAP 1.2
const NS_MEDIA: &str = "http://www.onvif.org/ver10/media/wsdl";
const NS_PTZ: &str = "http://www.onvif.org/ver20/ptz/wsdl";
const NS_WSSE: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd";
const NS_WSU_PWD_TEXT: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordText";

fn build_soap_envelope(username: &str, password: &str, inner_body: &str) -> String {
    format!(
        r#"<s:Envelope xmlns:s="{soap}" xmlns:wsse="{wsse}">
  <s:Header>
    <wsse:Security s:mustUnderstand="1">
      <wsse:UsernameToken>
        <wsse:Username>{user}</wsse:Username>
        <wsse:Password Type="{pwd_type}">{pass}</wsse:Password>
      </wsse:UsernameToken>
    </wsse:Security>
  </s:Header>
  <s:Body>
    {inner}
  </s:Body>
</s:Envelope>"#,
        soap = SOAP_ENV,
        wsse = NS_WSSE,
        pwd_type = NS_WSU_PWD_TEXT,
        user = xml_escape(username),
        pass = xml_escape(password),
        inner = inner_body
    )
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

async fn onvif_soap_request(endpoint: &str, username: &str, password: &str, body_xml: &str) -> Result<String, String> {
    let envelope = build_soap_envelope(username, password, body_xml);
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true) // por si la cámara usa certificados self-signed (https); no afecta http
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .post(endpoint)
        .header("Content-Type", "application/soap+xml; charset=utf-8")
        .body(envelope)
        .basic_auth(username, Some(password))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("ONVIF HTTP {}: {}", status, text));
    }
    Ok(text)
}

async fn get_first_profile_token(endpoint: &str, username: &str, password: &str) -> Result<String, String> {
    let body = format!("<GetProfiles xmlns=\"{NS_MEDIA}\"/>");
    let xml = onvif_soap_request(endpoint, username, password, &body).await?;
    // Parse XML to find attribute token on any element whose local name ends with "Profiles" or "Profile".
    let mut reader = Reader::from_str(&xml);
    reader.trim_text(true);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let local = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if local.ends_with("Profiles") || local.ends_with("Profile") {
                    for a in e.attributes().flatten() {
                        if a.key.as_ref() == b"token" {
                            let v = a.unescape_value().map_err(|e| e.to_string())?.to_string();
                            if !v.is_empty() { return Ok(v); }
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("XML parse error: {}", e)),
            _ => {}
        }
        buf.clear();
    }
    Err("No se encontró ProfileToken en la respuesta ONVIF".into())
}

async fn with_profile_token<F, Fut>(onvif_url: &str, f: F) -> Result<(), String>
where
    F: FnOnce(String, String, String, String) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    // Extrae user/pass del URL y deja un endpoint sin credenciales
    let url = Url::parse(onvif_url).map_err(|e| e.to_string())?;
    let username = url.username().to_string();
    let password = url.password().unwrap_or("").to_string();
    let mut endpoint_url = url.clone();
    endpoint_url.set_username("").ok();
    endpoint_url.set_password(None).ok();
    let endpoint = endpoint_url.as_str().to_string();

    if username.is_empty() {
        return Err("El URL ONVIF debe incluir usuario: http://user:pass@host/...".into());
    }

    // Obtiene el token de perfil (usando el mismo endpoint). Si falla, intenta un endpoint 'media'.
    let token_res = get_first_profile_token(&endpoint, &username, &password).await;
    let (endpoint_final, token) = match token_res {
        Ok(tok) => (endpoint.clone(), tok),
        Err(_) => {
            // fallback: reemplazar último segmento por 'media' o anexarlo
            let mut alt = Url::parse(&endpoint).map_err(|e| e.to_string())?;
            if let Some(mut segs) = alt.path_segments_mut().ok() {
                segs.pop_if_empty(); // normalize trailing empty segment
            }
            let mut path = alt.path().trim_end_matches('/').to_string();
            if path.ends_with("/ptz") {
                path = path.trim_end_matches("ptz").to_string() + "media";
            } else if !path.ends_with("/media") {
                path = path + "/media";
            }
            alt.set_path(&path);
            let alt_endpoint = alt.as_str().to_string();
            let tok2 = get_first_profile_token(&alt_endpoint, &username, &password).await.map_err(|e| format!("GetProfiles fallback(media) error: {}", e))?;
            (alt_endpoint, tok2)
        }
    };
    f(endpoint_final, username, password, token).await
}

async fn ptz_continuous_move(onvif_url: &str, pan: f32, tilt: f32, zoom: Option<f32>) -> Result<(), String> {
    with_profile_token(onvif_url, |endpoint, user, pass, token| async move {
        let vel_pt = if pan != 0.0 || tilt != 0.0 {
            format!("<PanTilt x=\"{:.3}\" y=\"{:.3}\"/>", pan, tilt)
        } else { String::new() };
        let vel_zoom = if let Some(z) = zoom { format!("<Zoom x=\"{:.3}\"/>", z) } else { String::new() };
        let body = format!(
            "<ContinuousMove xmlns=\"{ns}\">\
                <ProfileToken>{token}</ProfileToken>\
                <Velocity>{vel_pt}{vel_zoom}</Velocity>\
                <Timeout>PT1S</Timeout>\
            </ContinuousMove>", ns = NS_PTZ, token = xml_escape(&token), vel_pt = vel_pt, vel_zoom = vel_zoom
        );
        onvif_soap_request(&endpoint, &user, &pass, &body).await.map(|_| ())
    }).await
}

async fn ptz_stop_all(onvif_url: &str) -> Result<(), String> {
    with_profile_token(onvif_url, |endpoint, user, pass, token| async move {
        let body = format!(
            "<Stop xmlns=\"{ns}\">\
                <ProfileToken>{token}</ProfileToken>\
                <PanTilt>true</PanTilt>\
                <Zoom>true</Zoom>\
            </Stop>", ns = NS_PTZ, token = xml_escape(&token)
        );
        onvif_soap_request(&endpoint, &user, &pass, &body).await.map(|_| ())
    }).await
}

use axum::response::Response;
fn ok() -> Response { (StatusCode::OK, Json(ApiResponse{ status: "success".into(), message: "ok".into()})).into_response() }
fn err(msg: String) -> Response { (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse{ status: "error".into(), message: msg})).into_response() }

pub async fn pan_left(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(status) = check_auth(&headers, &state.proxy_token).await { return (status, Json(ApiResponse{ status:"error".into(), message:"Unauthorized".into()})).into_response(); }
    match ptz_continuous_move(&state.camera_onvif_url, -0.5, 0.0, None).await { Ok(_) => ok(), Err(e)=> err(e) }
}
pub async fn pan_right(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(status) = check_auth(&headers, &state.proxy_token).await { return (status, Json(ApiResponse{ status:"error".into(), message:"Unauthorized".into()})).into_response(); }
    match ptz_continuous_move(&state.camera_onvif_url, 0.5, 0.0, None).await { Ok(_) => ok(), Err(e)=> err(e) }
}
pub async fn tilt_up(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(status) = check_auth(&headers, &state.proxy_token).await { return (status, Json(ApiResponse{ status:"error".into(), message:"Unauthorized".into()})).into_response(); }
    match ptz_continuous_move(&state.camera_onvif_url, 0.0, 0.5, None).await { Ok(_) => ok(), Err(e)=> err(e) }
}
pub async fn tilt_down(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(status) = check_auth(&headers, &state.proxy_token).await { return (status, Json(ApiResponse{ status:"error".into(), message:"Unauthorized".into()})).into_response(); }
    match ptz_continuous_move(&state.camera_onvif_url, 0.0, -0.5, None).await { Ok(_) => ok(), Err(e)=> err(e) }
}
pub async fn zoom_in(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(status) = check_auth(&headers, &state.proxy_token).await { return (status, Json(ApiResponse{ status:"error".into(), message:"Unauthorized".into()})).into_response(); }
    match ptz_continuous_move(&state.camera_onvif_url, 0.0, 0.0, Some(0.5)).await { Ok(_) => ok(), Err(e)=> err(e) }
}
pub async fn zoom_out(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(status) = check_auth(&headers, &state.proxy_token).await { return (status, Json(ApiResponse{ status:"error".into(), message:"Unauthorized".into()})).into_response(); }
    match ptz_continuous_move(&state.camera_onvif_url, 0.0, 0.0, Some(-0.5)).await { Ok(_) => ok(), Err(e)=> err(e) }
}
pub async fn ptz_stop(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(status) = check_auth(&headers, &state.proxy_token).await { return (status, Json(ApiResponse{ status:"error".into(), message:"Unauthorized".into()})).into_response(); }
    match ptz_stop_all(&state.camera_onvif_url).await { Ok(_) => ok(), Err(e)=> err(e) }
}