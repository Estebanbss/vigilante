use crate::AppState;
use axum::{extract::State, response::IntoResponse, Json};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use chrono::{Local, Utc};
use quick_xml::events::Event;
use quick_xml::Reader;
use rand::RngCore;
use sha1::{Digest, Sha1};
use std::io::{BufWriter, Write};
use std::sync::Arc;
use tokio::sync::broadcast;
use url::Url;

#[derive(serde::Serialize)]
pub struct ApiResponse {
    pub status: String,
    pub message: String,
}

const SOAP_ENV: &str = "http://www.w3.org/2003/05/soap-envelope";
const NS_EVENTS: &str = "http://www.onvif.org/ver10/events/wsdl";
const NS_WSSE: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd";
const NS_WSU: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd";

fn log_to_file(writer: &mut BufWriter<std::fs::File>, emoji: &str, msg: String) {
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
    let line = format!("[{}] {} {}", timestamp, emoji, msg);
    println!("{}", line);
    writeln!(&mut *writer, "{}", line).unwrap();
    writer.flush().unwrap();
}

/// Estructura para eventos de movimiento
#[derive(Debug, Clone)]
pub struct MotionEvent {
    pub timestamp: chrono::DateTime<Utc>,
    pub topic: String,
    pub source: String,
    pub state: bool, // true = movimiento detectado, false = movimiento terminado
}

/// Canal para broadcast de eventos de movimiento
pub type MotionEventSender = broadcast::Sender<MotionEvent>;

fn build_soap_envelope(username: &str, password: &str, inner_body: &str) -> String {
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
                <wsse:Password Type="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordDigest">{pwd}</wsse:Password>
                <wsse:Nonce EncodingType="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-soap-message-security-1.0#Base64Binary">{nonce}</wsse:Nonce>
                <wsu:Created>{created}</wsu:Created>
            </wsse:UsernameToken>
        </wsse:Security>
    </s:Header>
    <s:Body>{body}</s:Body>
</s:Envelope>"#,
        soap = SOAP_ENV,
        wsse = NS_WSSE,
        wsu = NS_WSU,
        user = username,
        pwd = digest_b64,
        nonce = nonce_b64,
        created = created,
        body = inner_body
    )
}

async fn onvif_soap_request(endpoint: &str, username: &str, password: &str, body: &str) -> Result<String, String> {
    let envelope = build_soap_envelope(username, password, body);

    println!("📡 ONVIF Events Request:");
    println!("{}", envelope);

    let client = reqwest::Client::new();
    let response = client
        .post(endpoint)
        .header("Content-Type", "application/soap+xml; charset=utf-8")
        .body(envelope)
        .send()
        .await
        .map_err(|e| format!("Error HTTP: {}", e))?;

    let status = response.status();
    let text = response.text().await.map_err(|e| format!("Error leyendo respuesta: {}", e))?;

    println!("📡 ONVIF Events Response ({}):", status);
    println!("{}", text);

    if !status.is_success() {
        return Err(format!("ONVIF HTTP {}: {}", status, text));
    }

    Ok(text)
}

/// Obtiene las capacidades de eventos de la cámara
pub async fn get_event_capabilities(onvif_url: &str) -> Result<(), String> {
    let url = Url::parse(onvif_url).map_err(|e| format!("Error parseando URL: {}", e))?;

    let username = url.username();
    let password = url.password().unwrap_or("");
    let host = url.host_str().ok_or("No host in URL")?;
    let port = url.port().unwrap_or(80);

    let endpoint = format!("http://{}:{}/onvif/events", host, port);

    let body = format!(
        r#"<GetEventProperties xmlns="{events_ns}" />"#,
        events_ns = NS_EVENTS
    );

    let _response = onvif_soap_request(&endpoint, username, password, &body).await?;
    println!("🎯 Capacidades de eventos obtenidas");
    Ok(())
}

/// Crea una suscripción a eventos de movimiento
pub async fn create_motion_subscription(onvif_url: &str, callback_url: &str) -> Result<String, String> {
    let url = Url::parse(onvif_url).map_err(|e| format!("Error parseando URL: {}", e))?;

    let username = url.username();
    let password = url.password().unwrap_or("");
    let host = url.host_str().ok_or("No host in URL")?;
    let port = url.port().unwrap_or(80);

    let endpoint = format!("http://{}:{}/onvif/events", host, port);

    // Suscripción a eventos de movimiento
    let body = format!(
        r#"<Subscribe xmlns="{events_ns}">
        <ConsumerReference>
            <Address>{callback}</Address>
        </ConsumerReference>
        <Filter>
            <TopicExpression Dialect="http://www.onvif.org/ver10/tev/topicExpression/ConcreteSet">
                tns1:RuleEngine/CellMotionDetector/Motion
            </TopicExpression>
        </Filter>
        <InitialTerminationTime>PT1H</InitialTerminationTime>
    </Subscribe>"#,
        events_ns = NS_EVENTS,
        callback = callback_url
    );

    let response = onvif_soap_request(&endpoint, username, password, &body).await?;

    // Parsear la respuesta para obtener el SubscriptionReference
    let mut reader = Reader::from_str(&response);
    let mut buf = Vec::new();
    let mut subscription_ref = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                if e.name().as_ref() == b"SubscriptionReference" {
                    // Leer el contenido
                    if let Ok(Event::Text(t)) = reader.read_event_into(&mut buf) {
                        subscription_ref = String::from_utf8_lossy(&t).to_string();
                        break;
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("Error parseando XML: {}", e)),
            _ => {}
        }
        buf.clear();
    }

    if subscription_ref.is_empty() {
        return Err("No se encontró SubscriptionReference en la respuesta".to_string());
    }

    println!("✅ Suscripción a eventos creada: {}", subscription_ref);
    Ok(subscription_ref)
}

/// Endpoint HTTP para recibir eventos de movimiento de la cámara
pub async fn motion_event_callback(
    State(state): State<Arc<AppState>>,
    body: String,
) -> impl IntoResponse {
    println!("📨 Evento de movimiento recibido:");
    println!("{}", body);

    // Parsear el evento SOAP
    let mut reader = Reader::from_str(&body);
    let mut buf = Vec::new();
    let mut is_motion_start = false;
    let mut is_motion_end = false;
    let mut topic = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                match name.as_ref() {
                    "SimpleItem" => {
                        if let Some(attr) = e.attributes().find(|a| a.as_ref().map(|a| String::from_utf8_lossy(&a.key.0)).unwrap_or_default() == "Name") {
                            let attr_value = String::from_utf8_lossy(&attr.as_ref().unwrap().value);
                            if attr_value == "IsMotion" {
                                if let Ok(Event::Text(t)) = reader.read_event_into(&mut buf) {
                                    let value = String::from_utf8_lossy(&t).to_string();
                                    is_motion_start = value == "true";
                                    is_motion_end = value == "false";
                                }
                            }
                        }
                    }
                    "Topic" => {
                        if let Ok(Event::Text(t)) = reader.read_event_into(&mut buf) {
                            topic = String::from_utf8_lossy(&t).to_string();
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    // Crear evento de movimiento
    let _motion_event = MotionEvent {
        timestamp: Utc::now(),
        topic: topic.clone(),
        source: "camera".to_string(),
        state: is_motion_start,
    };

    // Integrar con sistema de logging existente
    let mut log_writer_guard = state.log_writer.lock().await;
    if let Some(ref mut log_writer) = *log_writer_guard {
        if is_motion_start {
            log_to_file(log_writer, "🚶", format!("MOVIMIENTO DETECTADO por cámara nativa - Topic: {}", topic));
        } else if is_motion_end {
            log_to_file(log_writer, "✅", format!("Movimiento terminado - Topic: {}", topic));
        }
    } else {
        // Fallback a console si no hay log_writer
        if is_motion_start {
            println!("🚶 MOVIMIENTO DETECTADO por cámara nativa - Topic: {}", topic);
        } else if is_motion_end {
            println!("✅ Movimiento terminado - Topic: {}", topic);
        }
    }

    Json(ApiResponse {
        status: "ok".to_string(),
        message: "Evento procesado".to_string(),
    })
}

/// Inicia el servicio de eventos ONVIF
pub async fn start_onvif_events_service(state: Arc<AppState>) -> Result<(), String> {
    let onvif_url = &state.camera_onvif_url;

    if onvif_url.is_empty() {
        println!("⚠️  No se configuró URL ONVIF, omitiendo eventos nativos");
        return Ok(());
    }

    println!("🎯 Iniciando servicio de eventos ONVIF...");

    // Verificar capacidades de eventos
    match get_event_capabilities(onvif_url).await {
        Ok(_) => println!("✅ Cámara soporta eventos ONVIF"),
        Err(e) => {
            println!("⚠️  Error obteniendo capacidades de eventos: {}", e);
            println!("📝 Continuando sin eventos nativos, usando detección manual");
            return Ok(());
        }
    }

    // Crear suscripción (esto requiere un servidor HTTP público para callbacks)
    // Nota: Para desarrollo local, necesitarías ngrok o similar
    let callback_url = "http://tu-servidor-publico:3000/api/events/motion";

    match create_motion_subscription(onvif_url, &callback_url).await {
        Ok(sub_ref) => {
            println!("✅ Suscripción creada exitosamente");
            println!("🔗 Subscription Reference: {}", sub_ref);
            println!("📡 Eventos serán enviados a: {}", callback_url);
        }
        Err(e) => {
            println!("❌ Error creando suscripción: {}", e);
            println!("💡 Posibles causas:");
            println!("   - La cámara no soporta eventos de movimiento");
            println!("   - URL de callback no accesible desde la cámara");
            println!("   - Credenciales incorrectas");
        }
    }

    Ok(())
}