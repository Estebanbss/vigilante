//! WebRTC streaming para Vigilante.
//!
//! Maneja conexiones de transmisión de video/audio en tiempo real
//! con baja latencia y alta calidad.

use crate::error::VigilanteError;
use crate::state::StreamingState;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer::ClockTime;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use serde_json;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::setting_engine::SettingEngine;
use webrtc::api::APIBuilder;
use webrtc::ice::network_type::NetworkType;
use webrtc::ice_transport::ice_credential_type::RTCIceCredentialType;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecParameters;
use webrtc::rtp_transceiver::rtp_codec::{RTCRtpCodecCapability, RTPCodecType};
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;
use webrtc::track::track_local::TrackLocalWriter;

/// Manager principal para conexiones WebRTC.
pub struct WebRTCManager {
    api: Arc<webrtc::api::API>,
    peer_connections: Arc<RwLock<HashMap<String, Arc<RTCPeerConnection>>>>,
    streaming_state: Arc<StreamingState>,
    rtp_pipeline: Arc<RwLock<Option<gstreamer::Pipeline>>>,
    video_track: Arc<RwLock<Option<Arc<TrackLocalStaticRTP>>>>,
    audio_track: Arc<RwLock<Option<Arc<TrackLocalStaticRTP>>>>,
}

impl std::fmt::Debug for WebRTCManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebRTCManager")
            .field("peer_connections", &self.peer_connections)
            .field("streaming_state", &self.streaming_state)
            .field("rtp_pipeline", &self.rtp_pipeline)
            .finish()
    }
}

impl WebRTCManager {
    pub fn new(streaming_state: Arc<StreamingState>) -> Result<Self, VigilanteError> {
        // Crear API con codecs e interceptores explícitos
        // Force IPv4 only to avoid IPv6 resolution issues
        let mut setting_engine = SettingEngine::default();
        setting_engine.set_network_types(vec![NetworkType::Udp4, NetworkType::Tcp4]);

        let mut media_engine = MediaEngine::default();
        let mut h264_params = RTCRtpCodecParameters::default();
        h264_params.capability = RTCRtpCodecCapability {
            mime_type: "video/H264".to_string(),
            clock_rate: 90000,
            channels: 0,
            sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42001f"
                .to_string(),
            rtcp_feedback: vec![],
        };
        h264_params.payload_type = 102;
        media_engine
            .register_codec(h264_params, RTPCodecType::Video)
            .map_err(|e| VigilanteError::WebRTC(format!("Error registrando codec H264: {e}")))?;

        let mut opus_params = RTCRtpCodecParameters::default();
        opus_params.capability = RTCRtpCodecCapability {
            mime_type: "audio/opus".to_string(),
            clock_rate: 48000,
            channels: 2,
            sdp_fmtp_line: String::new(),
            rtcp_feedback: vec![],
        };
        opus_params.payload_type = 111;
        media_engine
            .register_codec(opus_params, RTPCodecType::Audio)
            .map_err(|e| VigilanteError::WebRTC(format!("Error registrando codec Opus: {e}")))?;

        let mut registry = Registry::new();
        registry = register_default_interceptors(registry, &mut media_engine)
            .map_err(|e| VigilanteError::WebRTC(format!("Error registrando interceptores: {e}")))?;

        let api = Arc::new(
            APIBuilder::new()
                .with_setting_engine(setting_engine)
                .with_media_engine(media_engine)
                .with_interceptor_registry(registry)
                .build(),
        );

        Ok(Self {
            api,
            peer_connections: Arc::new(RwLock::new(HashMap::new())),
            streaming_state,
            rtp_pipeline: Arc::new(RwLock::new(None)),
            video_track: Arc::new(RwLock::new(None)),
            audio_track: Arc::new(RwLock::new(None)),
        })
    }

    /// Procesar offer del cliente y retornar answer
    pub async fn process_offer(
        &self,
        client_id: &str,
        offer: RTCSessionDescription,
    ) -> Result<RTCSessionDescription, VigilanteError> {
        log::info!("📡 Procesando offer WebRTC del cliente: {}", client_id);

        // Fetch TURN credentials from Metered API
        let turn_api_key = std::env::var("TURN_API_KEY")
            .unwrap_or_else(|_| "574f9c4d65b7f555ba53016bfd08ad26033e".to_string());
        let turn_api_url = std::env::var("TURN_API_URL")
            .unwrap_or_else(|_| "https://metered.live/api/v1/turn/credentials".to_string());
        let client = reqwest::Client::new();
        let response = client
            .get(format!("{}?apiKey={}", turn_api_url, turn_api_key))
            .send()
            .await
            .map_err(|e| {
                log::error!("❌ Error fetching TURN credentials: {:?}", e);
                VigilanteError::WebRTC(format!("Failed to fetch TURN credentials: {:?}", e))
            })?;
        let ice_servers_json: Vec<serde_json::Value> = response.json().await.map_err(|e| {
            log::error!("❌ Error parsing TURN credentials: {:?}", e);
            VigilanteError::WebRTC(format!("Failed to parse TURN credentials: {:?}", e))
        })?;
        log::info!(
            "📡 Fetched {} ICE servers from Metered",
            ice_servers_json.len()
        );

        // Crear configuración con los ICE servers fetched
        let mut config = RTCConfiguration::default();
        let mut ice_servers: Vec<RTCIceServer> = ice_servers_json
            .into_iter()
            .filter_map(|v| {
                // Try to deserialize as RTCIceServer (object format)
                match serde_json::from_value::<RTCIceServer>(v.clone()) {
                    Ok(ice_server) => Self::sanitize_ice_server(ice_server),
                    Err(_) => {
                        // If that fails, try to parse as JSON object manually
                        if let Some(obj) = v.as_object() {
                            if let Some(urls) = obj.get("urls") {
                                let mut parsed_urls: Vec<String> = Vec::new();

                                match urls {
                                    serde_json::Value::String(url_str) => {
                                        parsed_urls.push(url_str.to_string());
                                    }
                                    serde_json::Value::Array(array) => {
                                        for url_value in array {
                                            if let Some(url_str) = url_value.as_str() {
                                                parsed_urls.push(url_str.to_string());
                                            } else {
                                                log::warn!(
                                                    "Failed to parse ICE server: url is not a string in {:?}",
                                                    url_value
                                                );
                                            }
                                        }
                                    }
                                    _ => {
                                        log::warn!(
                                            "Failed to parse ICE server: urls is not string or array in {:?}",
                                            obj
                                        );
                                    }
                                }

                                let mut sanitized_urls: BTreeSet<String> = BTreeSet::new();
                                for url in parsed_urls {
                                    match Self::sanitize_ice_url(&url) {
                                        Some(clean_url) => {
                                            if clean_url != url {
                                                log::info!(
                                                    "Sanitized ICE server url '{}' -> '{}'",
                                                    url, clean_url
                                                );
                                            }
                                            sanitized_urls.insert(clean_url);
                                        }
                                        None => {
                                            log::warn!(
                                                "Skipping ICE server url: unsupported format {}",
                                                url
                                            );
                                        }
                                    }
                                }

                                if sanitized_urls.is_empty() {
                                    log::warn!(
                                        "Failed to parse ICE server: no supported urls in {:?}",
                                        obj
                                    );
                                    None
                                } else {
                                    let username = obj
                                        .get("username")
                                        .and_then(|u| u.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    let credential = obj
                                        .get("credential")
                                        .and_then(|c| c.as_str())
                                        .unwrap_or("")
                                        .to_string();

                                    Self::sanitize_ice_server(RTCIceServer {
                                        urls: sanitized_urls.into_iter().collect(),
                                        username,
                                        credential,
                                        ..Default::default()
                                    })
                                }
                            } else {
                                log::warn!(
                                    "Failed to parse ICE server: missing urls field in {:?}",
                                    obj
                                );
                                None
                            }
                        } else if let Some(url_str) = v.as_str() {
                            // Handle string format
                            Self::sanitize_ice_url(url_str).and_then(|clean| {
                                Self::sanitize_ice_server(RTCIceServer {
                                    urls: vec![clean],
                                    username: String::new(),
                                    credential: String::new(),
                                    ..Default::default()
                                })
                            })
                        } else {
                            log::warn!("Failed to parse ICE server: unsupported format {:?}", v);
                            None
                        }
                    }
                }
            })
            .collect();

        // Eliminar duplicados exactos de servidor (urls + credenciales)
        let mut seen_servers: HashSet<(Vec<String>, String, String)> = HashSet::new();
        ice_servers.retain(|server| {
            let key = (
                server.urls.clone(),
                server.username.clone(),
                server.credential.clone(),
            );
            seen_servers.insert(key)
        });

        ice_servers.retain(|server| {
            let has_turn = server
                .urls
                .iter()
                .any(|url| url.starts_with("turn:") || url.starts_with("turns:"));

            if has_turn && (server.username.is_empty() || server.credential.is_empty()) {
                log::warn!(
                    "Skipping TURN server {:?}: missing username or credential",
                    server.urls
                );
                false
            } else {
                true
            }
        });

        for server in ice_servers.iter_mut() {
            let has_turn = server
                .urls
                .iter()
                .any(|url| url.starts_with("turn:") || url.starts_with("turns:"));

            if has_turn {
                server.credential_type = RTCIceCredentialType::Password;
            }
        }

        config.ice_servers = ice_servers;

        log::info!(
            "📡 Configured {} ICE servers: {:?}",
            config.ice_servers.len(),
            config.ice_servers
        );

        // Crear peer connection
        let peer_connection =
            Arc::new(self.api.new_peer_connection(config).await.map_err(|e| {
                log::error!("❌ Error creando peer connection: {:?}", e);
                VigilanteError::WebRTC(format!("Failed to create peer connection: {:?}", e))
            })?);

        // Establecer la offer del cliente como descripción remota
        peer_connection
            .set_remote_description(offer)
            .await
            .map_err(|e| {
                log::error!("❌ Error estableciendo descripción remota: {:?}", e);
                VigilanteError::WebRTC(format!("Failed to set remote description: {:?}", e))
            })?;

        // Obtener tracks de video y audio compartidos por el pipeline RTP
        let video_track = self.ensure_video_track().await?;
        let audio_track = self.ensure_audio_track().await?;

        // Agregar tracks a la conexión
        peer_connection.add_track(video_track.clone()).await?;
        peer_connection.add_track(audio_track.clone()).await?;

        // Crear answer
        let answer = peer_connection.create_answer(None).await.map_err(|e| {
            log::error!("❌ Error creando answer: {:?}", e);
            VigilanteError::WebRTC(format!("Failed to create answer: {:?}", e))
        })?;
        peer_connection
            .set_local_description(answer.clone())
            .await
            .map_err(|e| {
                log::error!("❌ Error estableciendo descripción local: {:?}", e);
                VigilanteError::WebRTC(format!("Failed to set local description: {:?}", e))
            })?;

        // Esperar a que se complete la recolección de candidatos ICE
        use webrtc::ice_transport::ice_gatherer_state::RTCIceGathererState;
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let tx = Arc::new(tx);
        peer_connection.on_ice_gathering_state_change(Box::new(move |state| {
            log::info!("ICE gathering state changed to: {:?}", state);
            if state == RTCIceGathererState::Complete {
                // Gathering complete
                let _ = tx.try_send(());
            }
            Box::pin(async {})
        }));

        // Esperar hasta 10 segundos para que se complete la recolección
        tokio::time::timeout(tokio::time::Duration::from_secs(10), rx.recv())
            .await
            .map_err(|_| {
                VigilanteError::WebRTC("ICE gathering timeout after 10 seconds".to_string())
            })?;

        // Guardar la conexión
        {
            let mut connections = self.peer_connections.write().await;
            connections.insert(client_id.to_string(), peer_connection.clone());
        }

        log::info!("✅ Answer WebRTC creada para cliente: {}", client_id);
        Ok(answer)
    }

    fn sanitize_ice_url(raw_url: &str) -> Option<String> {
        let trimmed = raw_url.trim();
        if trimmed.is_empty() {
            return None;
        }

        let supported_scheme = trimmed.starts_with("stun:")
            || trimmed.starts_with("turn:")
            || trimmed.starts_with("turns:");

        if !supported_scheme {
            return None;
        }

        let clean = trimmed
            .split('?')
            .next()
            .unwrap_or(trimmed)
            .trim_end_matches('&');
        if clean.is_empty() {
            return None;
        }

        Some(clean.to_string())
    }

    fn sanitize_ice_server(mut server: RTCIceServer) -> Option<RTCIceServer> {
        let mut sanitized_urls: BTreeSet<String> = BTreeSet::new();

        for url in server.urls.into_iter() {
            match Self::sanitize_ice_url(&url) {
                Some(clean) => {
                    if clean != url {
                        log::info!("Sanitized ICE server url '{}' -> '{}'", url, clean);
                    }
                    sanitized_urls.insert(clean);
                }
                None => {
                    log::warn!("Skipping ICE server url: unsupported format {}", url);
                }
            }
        }

        if sanitized_urls.is_empty() {
            log::warn!("Skipping ICE server: no usable urls after sanitization");
            return None;
        }

        server.urls = sanitized_urls.into_iter().collect();
        Some(server)
    }

    /// Procesar answer del cliente y completar la conexión
    pub async fn process_answer(
        &self,
        client_id: &str,
        answer: RTCSessionDescription,
    ) -> Result<(), VigilanteError> {
        log::info!("📡 Procesando answer WebRTC para cliente: {}", client_id);

        let connections = self.peer_connections.read().await;
        if let Some(peer_connection) = connections.get(client_id) {
            peer_connection.set_remote_description(answer).await?;
            log::info!("✅ Conexión WebRTC completada para cliente: {}", client_id);
            Ok(())
        } else {
            Err(VigilanteError::WebRTC(format!(
                "No se encontró conexión WebRTC para cliente: {}",
                client_id
            )))
        }
    }

    /// Cerrar conexión WebRTC
    pub async fn close_connection(&self, client_id: &str) -> Result<(), VigilanteError> {
        log::info!("🔌 Cerrando conexión WebRTC para cliente: {}", client_id);

        let mut connections = self.peer_connections.write().await;
        if let Some(peer_connection) = connections.remove(client_id) {
            peer_connection.close().await?;
            log::info!("✅ Conexión WebRTC cerrada para cliente: {}", client_id);
        }

        Ok(())
    }

    /// Crear track de video desde el pipeline GStreamer
    async fn create_video_track(&self) -> Result<TrackLocalStaticRTP, VigilanteError> {
        // Crear track con configuración H.264 básica
        let video_track = TrackLocalStaticRTP::new(
            webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                mime_type: "video/H264".to_string(),
                clock_rate: 90000,
                channels: 0,
                sdp_fmtp_line:
                    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42001f"
                        .to_string(),
                rtcp_feedback: vec![],
            },
            "video".to_string(),
            "vigilante-video".to_string(),
        );

        Ok(video_track)
    }

    /// Crear track de audio desde el pipeline GStreamer
    async fn create_audio_track(&self) -> Result<TrackLocalStaticRTP, VigilanteError> {
        let audio_track = TrackLocalStaticRTP::new(
            webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability {
                mime_type: "audio/opus".to_string(),
                clock_rate: 48000,
                channels: 2,
                sdp_fmtp_line: "".to_string(),
                rtcp_feedback: vec![],
            },
            "audio".to_string(),
            "vigilante-audio".to_string(),
        );

        Ok(audio_track)
    }

    async fn ensure_video_track(&self) -> Result<Arc<TrackLocalStaticRTP>, VigilanteError> {
        let mut video_guard = self.video_track.write().await;
        if let Some(track) = video_guard.as_ref() {
            return Ok(track.clone());
        }

        let track = Arc::new(self.create_video_track().await?);
        *video_guard = Some(track.clone());
        Ok(track)
    }

    async fn ensure_audio_track(&self) -> Result<Arc<TrackLocalStaticRTP>, VigilanteError> {
        let mut audio_guard = self.audio_track.write().await;
        if let Some(track) = audio_guard.as_ref() {
            return Ok(track.clone());
        }

        let track = Arc::new(self.create_audio_track().await?);
        *audio_guard = Some(track.clone());
        Ok(track)
    }

    /// Inicializar pipeline RTP para WebRTC
    pub async fn initialize_rtp_pipeline(&self, rtsp_url: &str) -> Result<(), VigilanteError> {
        log::info!(
            "🎥 Inicializando pipeline RTP para WebRTC (audio + video) desde: {}",
            rtsp_url
        );

        // Crear pipeline RTP para audio y video
        let pipeline = gst::Pipeline::new();

        // Elementos principales
        let rtspsrc = gst::ElementFactory::make("rtspsrc")
            .name("rtspsrc")
            .property("location", rtsp_url)
            .property("latency", 0u32)
            .build()?;

        // Elementos para video
        let rtph264depay = gst::ElementFactory::make("rtph264depay")
            .name("rtph264depay")
            .build()?;
        let h264parse = gst::ElementFactory::make("h264parse")
            .name("h264parse")
            .build()?;
        let rtph264pay = gst::ElementFactory::make("rtph264pay")
            .name("rtph264pay")
            .property("config-interval", 1i32)
            .property("pt", 103u32)
            .build()?;
        let video_appsink = gst::ElementFactory::make("appsink")
            .name("video_appsink")
            .property("sync", false)
            .property("async", false)
            .build()?;

        // Elementos para audio
        let rtpopusdepay = gst::ElementFactory::make("rtpopusdepay")
            .name("rtpopusdepay")
            .build()?;
        let opusparse = gst::ElementFactory::make("opusparse")
            .name("opusparse")
            .build()?;
        let rtpopuspay = gst::ElementFactory::make("rtpopuspay")
            .name("rtpopuspay")
            .property("pt", 111u32)
            .build()?;
        let audio_appsink = gst::ElementFactory::make("appsink")
            .name("audio_appsink")
            .property("sync", false)
            .property("async", false)
            .build()?;

        // Agregar elementos al pipeline
        pipeline.add_many(&[
            &rtspsrc,
            &rtph264depay,
            &h264parse,
            &rtph264pay,
            &video_appsink,
            &rtpopusdepay,
            &opusparse,
            &rtpopuspay,
            &audio_appsink,
        ])?;

        // Conectar pipelines de video y audio
        rtph264depay.link(&h264parse)?;
        h264parse.link(&rtph264pay)?;
        rtph264pay.link(&video_appsink)?;

        rtpopusdepay.link(&opusparse)?;
        opusparse.link(&rtpopuspay)?;
        rtpopuspay.link(&audio_appsink)?;

        // Conectar rtspsrc dinámicamente a ambos pipelines
        let rtph264depay_weak = rtph264depay.downgrade();
        let rtph264pay_weak = rtph264pay.downgrade();
        let rtpopusdepay_weak = rtpopusdepay.downgrade();

        rtspsrc.connect_pad_added(move |_, src_pad| {
            let Some(caps) = src_pad.current_caps() else {
                log::warn!("⚠️ Pad añadido sin caps actuales, ignorando");
                return;
            };

            let Some(structure) = caps.structure(0) else {
                log::warn!("⚠️ Pad añadido sin estructura en caps, ignorando");
                return;
            };

            let media_type = structure.name().to_string();
            let encoding_name = structure
                .get::<String>("encoding-name")
                .unwrap_or_else(|_| String::new());
            let payload = structure.get::<i32>("payload").unwrap_or(-1);

            log::info!(
                "📡 Pad añadido: media_type={}, encoding={}, payload={}",
                media_type,
                encoding_name,
                payload
            );

            if !media_type.starts_with("application/x-rtp") {
                log::debug!("🔎 Ignorando pad con media_type no RTP: {}", media_type);
                return;
            }

            let encoding_upper = encoding_name.to_uppercase();

            match encoding_upper.as_str() {
                "H264" | "H265" => {
                    if let Some(rtph264depay) = rtph264depay_weak.upgrade() {
                        if let Some(sink_pad) = rtph264depay.static_pad("sink") {
                            if !sink_pad.is_linked() {
                                src_pad.link(&sink_pad).unwrap();
                                log::info!("🎥 Conectado stream de video RTP ({})", encoding_name);
                            }

                            let upstream_event = gst_video::UpstreamForceKeyUnitEvent::builder()
                                .all_headers(true)
                                .running_time(ClockTime::NONE)
                                .count(0)
                                .build();

                            if !sink_pad.send_event(upstream_event) {
                                log::warn!(
                                    "⚠️ No se pudo solicitar keyframe upstream al RTSP src"
                                );
                            } else {
                                log::info!(
                                    "📩 Solicitud de keyframe enviada upstream al RTSP src"
                                );
                            }

                            if let Some(rtph264pay) = rtph264pay_weak.upgrade() {
                                if let Some(pay_src_pad) = rtph264pay.static_pad("src") {
                                    let downstream_event =
                                        gst_video::DownstreamForceKeyUnitEvent::builder()
                                            .all_headers(true)
                                            .timestamp(ClockTime::NONE)
                                            .stream_time(ClockTime::NONE)
                                            .running_time(ClockTime::NONE)
                                            .count(0)
                                            .build();

                                    if !pay_src_pad.send_event(downstream_event) {
                                        log::warn!(
                                            "⚠️ No se pudo propagar keyframe downstream en el pipeline"
                                        );
                                    }
                                } else {
                                    log::warn!(
                                        "⚠️ No se pudo obtener pad src de rtph264pay para propagar keyframe"
                                    );
                                }
                            } else {
                                log::warn!("⚠️ No fue posible acceder a rtph264pay para propagar keyframe");
                            }
                        }
                    }
                }
                "OPUS" => {
                    if let Some(rtpopusdepay) = rtpopusdepay_weak.upgrade() {
                        if let Some(sink_pad) = rtpopusdepay.static_pad("sink") {
                            if !sink_pad.is_linked() {
                                src_pad.link(&sink_pad).unwrap();
                                log::info!("🎵 Conectado stream de audio RTP ({})", encoding_name);
                            }
                        }
                    }
                }
                "PCMA" | "PCMU" => {
                    log::warn!(
                        "🎵 Stream RTP con encoding {} detectado, pero aún no se convierte a Opus",
                        encoding_name
                    );
                }
                other => {
                    log::warn!(
                        "⚠️ Encoding RTP desconocido: {} (payload {})",
                        other,
                        payload
                    );
                }
            }
        });

        // Configurar appsinks para video y audio
        let video_appsink = video_appsink.dynamic_cast::<gst_app::AppSink>().unwrap();
        let audio_appsink = audio_appsink.dynamic_cast::<gst_app::AppSink>().unwrap();

        // Canales separados para video y audio (unbounded para uso desde callbacks sync)
        let (video_tx, mut video_rx) = tokio::sync::mpsc::unbounded_channel();
        let (audio_tx, mut audio_rx) = tokio::sync::mpsc::unbounded_channel();

        // Configurar callback para video
        let video_tx_clone = video_tx.clone();
        video_appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |appsink| {
                    let sample = appsink.pull_sample().unwrap();
                    let buffer = sample.buffer().unwrap();
                    let data = buffer.map_readable().unwrap();
                    let rtp_data = bytes::Bytes::copy_from_slice(&data);

                    if let Err(e) = video_tx_clone.send(rtp_data) {
                        log::warn!("⚠️ No se pudo encolar RTP de video: {}", e);
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        // Configurar callback para audio
        let audio_tx_clone = audio_tx.clone();
        audio_appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |appsink| {
                    let sample = appsink.pull_sample().unwrap();
                    let buffer = sample.buffer().unwrap();
                    let data = buffer.map_readable().unwrap();
                    let rtp_data = bytes::Bytes::copy_from_slice(&data);

                    if let Err(e) = audio_tx_clone.send(rtp_data) {
                        log::warn!("⚠️ No se pudo encolar RTP de audio: {}", e);
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        // Obtener tracks compartidos (se crean si aún no existen)
        let _video_track = self.ensure_video_track().await?;
        let _audio_track = self.ensure_audio_track().await?;

        {
            let mut pipeline_guard = self.rtp_pipeline.write().await;
            *pipeline_guard = Some(pipeline.clone());
        }

        // Iniciar pipeline
        pipeline.set_state(gst::State::Playing)?;

        // Spawn task para manejar datos RTP de video
        let video_track_ref = self.video_track.clone();
        tokio::spawn(async move {
            while let Some(rtp_data) = video_rx.recv().await {
                if let Some(video_track) = video_track_ref.read().await.as_ref() {
                    // Enviar datos RTP directamente
                    if let Err(e) = video_track.write(&rtp_data).await {
                        log::warn!("⚠️ Error enviando RTP video: {}", e);
                    } else {
                        log::debug!("📹 Enviado {} bytes RTP video", rtp_data.len());
                    }
                }
            }
        });

        // Spawn task para manejar datos RTP de audio
        let audio_track_ref = self.audio_track.clone();
        tokio::spawn(async move {
            while let Some(rtp_data) = audio_rx.recv().await {
                if let Some(audio_track) = audio_track_ref.read().await.as_ref() {
                    // Enviar datos RTP directamente
                    if let Err(e) = audio_track.write(&rtp_data).await {
                        log::warn!("⚠️ Error enviando RTP audio: {}", e);
                    } else {
                        log::debug!("🎵 Enviado {} bytes RTP audio", rtp_data.len());
                    }
                }
            }
        });

        log::info!("✅ Pipeline RTP WebRTC inicializado (audio + video)");
        Ok(())
    }

    /// Detener pipeline RTP
    pub async fn stop_rtp_pipeline(&self) -> Result<(), VigilanteError> {
        log::info!("🛑 Deteniendo pipeline RTP WebRTC");

        let mut rtp_pipeline = self.rtp_pipeline.write().await;
        if let Some(pipeline) = rtp_pipeline.take() {
            pipeline.set_state(gst::State::Null)?;
            log::info!("✅ Pipeline RTP WebRTC detenido");
        }

        Ok(())
    }
}
