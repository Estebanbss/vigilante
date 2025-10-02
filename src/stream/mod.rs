//! Módulo de streaming para Vigilante.
//!
//! Maneja transmisión de audio y video en tiempo real.

pub mod depends;

pub use depends::audio::AudioStreamer;
pub use depends::webrtc::WebRTCManager;

use crate::error::VigilanteError;
use crate::AppState;
use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use std::sync::Arc;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;

/// Manager principal para streaming.
#[derive(Debug)]
pub struct StreamManager {
    audio_streamer: AudioStreamer,
    webrtc_manager: WebRTCManager,
}

impl StreamManager {
    pub fn new(streaming_state: Arc<crate::state::StreamingState>) -> Result<Self, VigilanteError> {
        Ok(Self {
            audio_streamer: AudioStreamer::new(streaming_state.clone()),
            webrtc_manager: WebRTCManager::new(streaming_state)?,
        })
    }

    pub async fn start_streaming(&self, rtsp_url: &str) -> Result<(), VigilanteError> {
        // Start audio streaming
        self.audio_streamer.start_stream().await?;

        // Initialize WebRTC RTP pipeline
        self.webrtc_manager
            .initialize_rtp_pipeline(rtsp_url)
            .await?;

        log::info!("🎥 Streaming manager initialized with WebRTC support");
        Ok(())
    }

    pub fn get_audio_streamer(&self) -> &AudioStreamer {
        &self.audio_streamer
    }

    pub fn get_webrtc_manager(&self) -> &WebRTCManager {
        &self.webrtc_manager
    }

    pub async fn get_stream_status(&self) -> serde_json::Value {
        serde_json::json!({
            "audio_available": self.audio_streamer.is_audio_available(),
            "video_streaming": true, // WebRTC is always available if camera is connected
            "webrtc_enabled": true
        })
    }
}

#[axum::debug_handler]
pub async fn stream_audio_handler(State(state): State<Arc<crate::AppState>>) -> impl IntoResponse {
    use axum::body::Body;

    let mut audio_rx = state.streaming.audio_mp3_tx.subscribe();

    // Create a stream that yields audio chunks
    let stream = async_stream::stream! {
        loop {
            match audio_rx.recv().await {
                Ok(chunk) => {
                    if !chunk.is_empty() {
                        yield Ok::<_, std::io::Error>(chunk);
                    }
                }
                Err(_) => break, // Channel closed
            }
        }
    };

    let body = Body::from_stream(stream);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "audio/mpeg")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "close")
        .body(body)
        .unwrap()
}

// ===== ENDPOINT COMBINADO AUDIO+VIDEO PARA TESTING =====

/// Endpoint para stream combinado de audio + video (MPEG-TS para ffplay)
pub async fn stream_combined_av(State(state): State<Arc<crate::AppState>>) -> impl IntoResponse {
    use axum::body::Body;

    // Usar el WebRTC manager para obtener datos de audio y video
    // Por ahora devolver audio, pero preparado para multiplexar A+V
    let mut audio_rx = state.streaming.audio_mp3_tx.subscribe();

    // TODO: Obtener datos RTP de video del pipeline WebRTC y multiplexar
    let stream = async_stream::stream! {
        // MPEG-TS header básico
        let ts_header = create_mpeg_ts_header();
        yield Ok::<_, std::io::Error>(ts_header);

        loop {
            match audio_rx.recv().await {
                Ok(chunk) => {
                    if !chunk.is_empty() {
                        // Convertir audio MP3 a packets MPEG-TS
                        let ts_packets = audio_to_mpeg_ts(&chunk);
                        for packet in ts_packets {
                            yield Ok(packet);
                        }
                    }
                }
                Err(_) => break,
            }
        }
    };

    let body = Body::from_stream(stream);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "video/MP2T") // MPEG-TS
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "close")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .body(body)
        .unwrap()
}

fn create_mpeg_ts_header() -> bytes::Bytes {
    // PAT (Program Association Table) - indica qué programas están disponibles
    let pat = vec![
        0x47, 0x40, 0x00, 0x10, // Header TS
        0x00, // Pointer field
        0x00, 0xB0, 0x0D, 0x00, 0x01, 0xC1, 0x00, 0x00, // PAT header
        0x00, 0x01, 0xF0, 0x01, // Program 1 -> PMT PID 0x1001
        0x2E, 0x59, 0x6F, 0xFF, 0xFF, // CRC
    ];

    // PMT (Program Map Table) - describe los streams del programa
    let pmt = vec![
        0x47, 0x50, 0x01, 0x10, // Header TS (PID 0x1001)
        0x00, // Pointer field
        0x02, 0xB0, 0x17, 0x00, 0x01, 0xC1, 0x00, 0x00, // PMT header
        0xE1, 0x00, 0xF0, 0x00, // PCR PID
        0x1B, 0xE1, 0x00, 0xF0, 0x00, // H.264 video
        0x0F, 0xE1, 0x01, 0xF0, 0x00, // MP3 audio
        0xFF, 0xFF, 0xFF, 0xFF, // CRC placeholder
    ];

    let mut combined = Vec::new();
    combined.extend_from_slice(&pat);
    combined.extend_from_slice(&pmt);
    bytes::Bytes::from(combined)
}

fn audio_to_mpeg_ts(audio_data: &[u8]) -> Vec<bytes::Bytes> {
    let mut packets = Vec::new();

    // Dividir audio en chunks de ~184 bytes (tamaño payload TS)
    for chunk in audio_data.chunks(184) {
        let mut packet = vec![0u8; 188]; // MPEG-TS packet size

        // TS header
        packet[0] = 0x47; // Sync byte
        packet[1] = 0x41; // PID 0x101 (audio)
        packet[2] = 0x01;
        packet[3] = 0x10; // Payload start indicator

        // Adaptation field (si es necesario)
        if chunk.len() < 184 {
            packet[3] |= 0x20; // Adaptation field present
            packet[4] = (183 - chunk.len()) as u8; // Adaptation field length
                                                   // Rellenar con stuffing bytes
            for i in 5..(5 + packet[4] as usize) {
                packet[i] = 0xFF;
            }
        }

        // Copiar datos de audio
        let data_start = if chunk.len() < 184 {
            5 + packet[4] as usize
        } else {
            4
        };
        packet[data_start..data_start + chunk.len()].copy_from_slice(chunk);

        packets.push(bytes::Bytes::from(packet));
    }

    packets
}

// ===== ENDPOINTS WEBRTC =====

/// Endpoint para iniciar conexión WebRTC - recibe offer del cliente
pub async fn webrtc_offer(
    State(state): State<Arc<AppState>>,
    Json(offer): Json<RTCSessionDescription>,
) -> impl IntoResponse {
    log::info!("📡 Recibida offer WebRTC del cliente");

    // Generar ID único para el cliente
    let client_id = uuid::Uuid::new_v4().to_string();

    match state
        .stream
        .get_webrtc_manager()
        .process_offer(&client_id, offer)
        .await
    {
        Ok(answer) => {
            log::info!("✅ Answer WebRTC creada para cliente: {}", client_id);
            Json(serde_json::json!({
                "client_id": client_id,
                "sdp": answer.sdp
            }))
        }
        Err(e) => {
            log::error!("❌ Error creando answer WebRTC: {:?}", e);
            Json(serde_json::json!({
                "error": "Failed to create WebRTC answer",
                "details": e.to_string()
            }))
        }
    }
}

/// Endpoint para completar conexión WebRTC - recibe answer del cliente
pub async fn webrtc_answer(
    State(state): State<Arc<AppState>>,
    Path(client_id): Path<String>,
    Json(answer): Json<RTCSessionDescription>,
) -> impl IntoResponse {
    log::info!("📡 Recibida answer WebRTC para cliente: {}", client_id);

    match state
        .stream
        .get_webrtc_manager()
        .process_answer(&client_id, answer)
        .await
    {
        Ok(_) => {
            log::info!("✅ Conexión WebRTC completada para cliente: {}", client_id);
            Json(serde_json::json!({
                "status": "connected"
            }))
        }
        Err(e) => {
            log::error!("❌ Error procesando answer WebRTC: {:?}", e);
            Json(serde_json::json!({
                "error": "Failed to process WebRTC answer",
                "details": e.to_string()
            }))
        }
    }
}

/// Endpoint para cerrar conexión WebRTC
pub async fn webrtc_close(
    State(state): State<Arc<AppState>>,
    Path(client_id): Path<String>,
) -> impl IntoResponse {
    log::info!("🔌 Cerrando conexión WebRTC para cliente: {}", client_id);

    match state
        .stream
        .get_webrtc_manager()
        .close_connection(&client_id)
        .await
    {
        Ok(_) => {
            log::info!("✅ Conexión WebRTC cerrada para cliente: {}", client_id);
            Json(serde_json::json!({
                "status": "disconnected"
            }))
        }
        Err(e) => {
            log::error!("❌ Error cerrando conexión WebRTC: {:?}", e);
            Json(serde_json::json!({
                "error": "Failed to close WebRTC connection",
                "details": e.to_string()
            }))
        }
    }
}
