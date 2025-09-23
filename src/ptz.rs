use crate::{auth::RequireAuth, AppState};
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use chrono::Utc;
use quick_xml::events::Event;
use quick_xml::Reader;
use rand::RngCore;
use sha1::{Digest, Sha1};
use std::sync::Arc;
use url::Url;

#[derive(serde::Serialize)]
pub struct ApiResponse {
    pub status: String,
    pub message: String,
}

const SOAP_ENV: &str = "http://www.w3.org/2003/05/soap-envelope"; // SOAP 1.2
const NS_MEDIA: &str = "http://www.onvif.org/ver10/media/wsdl";
const NS_PTZ: &str = "http://www.onvif.org/ver20/ptz/wsdl";
const NS_WSSE: &str =
    "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd";
const NS_WSU: &str =
    "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd";
const NS_WSU_PWD_DIGEST: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordDigest";
const NS_WSSE_NONCE_ENCODING: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-soap-message-security-1.0#Base64Binary";

fn build_soap_envelope(username: &str, password: &str, inner_body: &str) -> String {
    // Build WS-Security UsernameToken with PasswordDigest as per ONVIF requirements
    let mut nonce_bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce_b64 = BASE64.encode(nonce_bytes);
    let created = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut sha = Sha1::new();
    sha.update(&nonce_bytes);
    sha.update(created.as_bytes());
    sha.update(password.as_bytes());
    let digest = sha.finalize();
    let digest_b64 = BASE64.encode(digest);

    format!(
        r#"<s:Envelope xmlns:s="{soap}" xmlns:wsse="{wsse}" xmlns:wsu="{wsu}">
    <s:Header>
        <wsse:Security s:mustUnderstand="1">
            <wsse:UsernameToken>
                <wsse:Username>{user}</wsse:Username>
                <wsse:Password Type="{pwd_type}">{pwd}</wsse:Password>
                <wsse:Nonce EncodingType="{nonce_enc}">{nonce}</wsse:Nonce>
                <wsu:Created>{created}</wsu:Created>
            </wsse:UsernameToken>
        </wsse:Security>
    </s:Header>
    <s:Body>
        {inner}
    </s:Body>
</s:Envelope>"#,
        soap = SOAP_ENV,
        wsse = NS_WSSE,
        wsu = NS_WSU,
        pwd_type = NS_WSU_PWD_DIGEST,
        pwd = digest_b64,
        nonce_enc = NS_WSSE_NONCE_ENCODING,
        nonce = nonce_b64,
        user = xml_escape(username),
        created = created,
        inner = inner_body
    )
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

async fn onvif_soap_request(
    endpoint: &str,
    username: &str,
    password: &str,
    body_xml: &str,
) -> Result<String, String> {
    let envelope = build_soap_envelope(username, password, body_xml);

    println!("🔄 PTZ ONVIF Request:");
    println!("   Endpoint: {}", endpoint);
    println!("   Username: {}", username);
    println!(
        "   Password: {}***",
        &password[..std::cmp::min(3, password.len())]
    );
    println!("   SOAP Body: {}", body_xml);

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true) // por si la cámara usa certificados self-signed (https); no afecta http
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| {
            eprintln!("❌ Error creando cliente HTTP: {}", e);
            e.to_string()
        })?;

    let resp = client
        .post(endpoint)
        .header("Content-Type", "application/soap+xml; charset=utf-8")
        .body(envelope.clone())
        .basic_auth(username, Some(password))
        .send()
        .await
        .map_err(|e| {
            eprintln!("❌ Error enviando petición ONVIF: {}", e);
            e.to_string()
        })?;

    let status = resp.status();
    let headers = resp.headers().clone();
    let text = resp.text().await.map_err(|e| {
        eprintln!("❌ Error leyendo respuesta: {}", e);
        e.to_string()
    })?;

    println!("📡 PTZ ONVIF Response:");
    println!("   Status: {}", status);
    println!("   Headers: {:?}", headers);
    println!("   Body: {}", text);

    if !status.is_success() {
        let error_msg = format!("ONVIF HTTP {}: {}", status, text);
        eprintln!("❌ {}", error_msg);
        return Err(error_msg);
    }

    println!("✅ PTZ Request successful");
    Ok(text)
}

async fn get_first_profile_token(
    endpoint: &str,
    username: &str,
    password: &str,
) -> Result<String, String> {
    println!("🔍 Buscando ProfileToken...");
    let body = format!("<GetProfiles xmlns=\"{NS_MEDIA}\"/>");
    let xml = onvif_soap_request(endpoint, username, password, &body).await?;

    println!("🔍 Parseando XML para ProfileToken...");
    // Parse XML to find attribute token on any element whose local name ends with "Profiles" or "Profile".
    let mut reader = Reader::from_str(&xml);
    reader.trim_text(true);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let local = String::from_utf8_lossy(e.name().as_ref()).to_string();
                println!("   Elemento XML: {}", local);
                if local.ends_with("Profiles") || local.ends_with("Profile") {
                    println!("   ✅ Encontrado elemento de perfil: {}", local);
                    for a in e.attributes().flatten() {
                        let attr_name = String::from_utf8_lossy(a.key.as_ref());
                        println!("     Atributo: {} = {:?}", attr_name, a.value);
                        if a.key.as_ref() == b"token" {
                            let v = a.unescape_value().map_err(|e| e.to_string())?.to_string();
                            if !v.is_empty() {
                                println!("   🎯 ProfileToken encontrado: {}", v);
                                return Ok(v);
                            }
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                let error_msg = format!("XML parse error: {}", e);
                eprintln!("❌ {}", error_msg);
                return Err(error_msg);
            }
            _ => {}
        }
        buf.clear();
    }
    let error_msg = "No se encontró ProfileToken en la respuesta ONVIF".to_string();
    eprintln!("❌ {}", error_msg);
    Err(error_msg)
}

async fn with_profile_token<F, Fut>(onvif_url: &str, f: F) -> Result<(), String>
where
    F: FnOnce(String, String, String, String) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    println!("🚀 Iniciando operación PTZ con URL: {}", onvif_url);

    // Extrae user/pass del URL y deja un endpoint sin credenciales
    let url = Url::parse(onvif_url).map_err(|e| {
        let error_msg = format!("Error parseando URL ONVIF: {}", e);
        eprintln!("❌ {}", error_msg);
        error_msg
    })?;

    let username = url.username().to_string();
    let password = url.password().unwrap_or("").to_string();
    let mut endpoint_url = url.clone();
    endpoint_url.set_username("").ok();
    endpoint_url.set_password(None).ok();
    let endpoint = endpoint_url.as_str().to_string();

    println!("📋 Credenciales extraídas:");
    println!("   Usuario: {}", username);
    println!(
        "   Password: {}***",
        &password[..std::cmp::min(3, password.len())]
    );
    println!("   Endpoint: {}", endpoint);

    if username.is_empty() {
        let error_msg = "El URL ONVIF debe incluir usuario: http://user:pass@host/...".to_string();
        eprintln!("❌ {}", error_msg);
        return Err(error_msg);
    }

    // Obtiene el token de perfil (usando el mismo endpoint). Si falla, intenta un endpoint 'media'.
    println!("🔄 Intentando obtener ProfileToken desde endpoint principal...");
    let token_res = get_first_profile_token(&endpoint, &username, &password).await;
    let (endpoint_final, token) = match token_res {
        Ok(tok) => {
            println!("✅ ProfileToken obtenido desde endpoint principal: {}", tok);
            (endpoint.clone(), tok)
        }
        Err(e) => {
            println!("⚠️ Fallo en endpoint principal: {}", e);
            println!("🔄 Intentando fallback a endpoint /media...");
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
            println!("🔄 Probando endpoint alternativo: {}", alt_endpoint);
            let tok2 = get_first_profile_token(&alt_endpoint, &username, &password)
                .await
                .map_err(|e| {
                    let error_msg = format!("GetProfiles fallback(media) error: {}", e);
                    eprintln!("❌ {}", error_msg);
                    error_msg
                })?;
            println!("✅ ProfileToken obtenido desde endpoint /media: {}", tok2);
            (alt_endpoint, tok2)
        }
    };

    println!("🎯 Ejecutando comando PTZ con ProfileToken: {}", token);
    f(endpoint_final, username, password, token).await
}

async fn ptz_continuous_move(
    onvif_url: &str,
    pan: f32,
    tilt: f32,
    zoom: Option<f32>,
) -> Result<(), String> {
    println!(
        "🎮 PTZ ContinuousMove: pan={}, tilt={}, zoom={:?}",
        pan, tilt, zoom
    );
    with_profile_token(onvif_url, |endpoint, user, pass, token| async move {
        let vel_pt = if pan != 0.0 || tilt != 0.0 {
            format!("<PanTilt x=\"{:.3}\" y=\"{:.3}\"/>", pan, tilt)
        } else {
            String::new()
        };
        let vel_zoom = if let Some(z) = zoom {
            format!("<Zoom x=\"{:.3}\"/>", z)
        } else {
            String::new()
        };
        let body = format!(
            "<ContinuousMove xmlns=\"{ns}\">\
                <ProfileToken>{token}</ProfileToken>\
                <Velocity>{vel_pt}{vel_zoom}</Velocity>\
                <Timeout>PT1S</Timeout>\
            </ContinuousMove>",
            ns = NS_PTZ,
            token = xml_escape(&token),
            vel_pt = vel_pt,
            vel_zoom = vel_zoom
        );
        println!("📤 Enviando comando ContinuousMove...");
        onvif_soap_request(&endpoint, &user, &pass, &body)
            .await
            .map(|_| {
                println!("✅ ContinuousMove completado");
            })
    })
    .await
}

async fn ptz_stop_all(onvif_url: &str) -> Result<(), String> {
    println!("🛑 PTZ Stop");
    with_profile_token(onvif_url, |endpoint, user, pass, token| async move {
        let body = format!(
            "<Stop xmlns=\"{ns}\">\
                <ProfileToken>{token}</ProfileToken>\
                <PanTilt>true</PanTilt>\
                <Zoom>true</Zoom>\
            </Stop>",
            ns = NS_PTZ,
            token = xml_escape(&token)
        );
        println!("📤 Enviando comando Stop...");
        onvif_soap_request(&endpoint, &user, &pass, &body)
            .await
            .map(|_| {
                println!("✅ Stop completado");
            })
    })
    .await
}

use axum::response::Response;
fn ok() -> Response {
    (
        StatusCode::OK,
        Json(ApiResponse {
            status: "success".into(),
            message: "ok".into(),
        }),
    )
        .into_response()
}
fn err(msg: String) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiResponse {
            status: "error".into(),
            message: msg,
        }),
    )
        .into_response()
}

pub async fn pan_left(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    println!("🎮 API Call: pan_left");
    match ptz_continuous_move(&state.camera_onvif_url, -0.5, 0.0, None).await {
        Ok(_) => {
            println!("✅ pan_left exitoso");
            ok()
        }
        Err(e) => {
            eprintln!("❌ pan_left falló: {}", e);
            err(e)
        }
    }
}
pub async fn pan_right(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    println!("🎮 API Call: pan_right");
    match ptz_continuous_move(&state.camera_onvif_url, 0.5, 0.0, None).await {
        Ok(_) => {
            println!("✅ pan_right exitoso");
            ok()
        }
        Err(e) => {
            eprintln!("❌ pan_right falló: {}", e);
            err(e)
        }
    }
}
pub async fn tilt_up(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    println!("🎮 API Call: tilt_up");
    match ptz_continuous_move(&state.camera_onvif_url, 0.0, 0.5, None).await {
        Ok(_) => {
            println!("✅ tilt_up exitoso");
            ok()
        }
        Err(e) => {
            eprintln!("❌ tilt_up falló: {}", e);
            err(e)
        }
    }
}
pub async fn tilt_down(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    println!("🎮 API Call: tilt_down");
    match ptz_continuous_move(&state.camera_onvif_url, 0.0, -0.5, None).await {
        Ok(_) => {
            println!("✅ tilt_down exitoso");
            ok()
        }
        Err(e) => {
            eprintln!("❌ tilt_down falló: {}", e);
            err(e)
        }
    }
}
pub async fn zoom_in(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    println!("🎮 API Call: zoom_in");
    match ptz_continuous_move(&state.camera_onvif_url, 0.0, 0.0, Some(0.5)).await {
        Ok(_) => {
            println!("✅ zoom_in exitoso");
            ok()
        }
        Err(e) => {
            eprintln!("❌ zoom_in falló: {}", e);
            err(e)
        }
    }
}
pub async fn zoom_out(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    println!("🎮 API Call: zoom_out");
    match ptz_continuous_move(&state.camera_onvif_url, 0.0, 0.0, Some(-0.5)).await {
        Ok(_) => {
            println!("✅ zoom_out exitoso");
            ok()
        }
        Err(e) => {
            eprintln!("❌ zoom_out falló: {}", e);
            err(e)
        }
    }
}
pub async fn ptz_stop(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    println!("🎮 API Call: ptz_stop");
    match ptz_stop_all(&state.camera_onvif_url).await {
        Ok(_) => {
            println!("✅ ptz_stop exitoso");
            ok()
        }
        Err(e) => {
            eprintln!("❌ ptz_stop falló: {}", e);
            err(e)
        }
    }
}
