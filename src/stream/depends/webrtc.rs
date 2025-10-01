//! WebRTC streaming para Vigilante.
//!
//! Maneja conexiones WebRTC para transmisión de video/audio en tiempo real
//! con baja latencia y alta calidad.

use crate::error::VigilanteError;
use crate::state::StreamingState;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use webrtc::api::APIBuilder;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
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
        // Crear API básica sin configuración avanzada de codecs por ahora
        let api = Arc::new(APIBuilder::new().build());

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
    pub async fn process_offer(&self, client_id: &str, offer: RTCSessionDescription) -> Result<RTCSessionDescription, VigilanteError> {
        log::info!("📡 Procesando offer WebRTC del cliente: {}", client_id);

        // Crear peer connection
        let config = RTCConfiguration::default();
        let peer_connection = Arc::new(self.api.new_peer_connection(config).await?);

        // Establecer la offer del cliente como descripción remota
        peer_connection.set_remote_description(offer).await?;

        // Crear tracks de video y audio para esta conexión
        let video_track = self.create_video_track().await?;
        let audio_track = self.create_audio_track().await?;

        // Agregar tracks a la conexión
        peer_connection.add_track(Arc::new(video_track)).await?;
        peer_connection.add_track(Arc::new(audio_track)).await?;

        // Crear answer
        let answer = peer_connection.create_answer(None).await?;
        peer_connection.set_local_description(answer.clone()).await?;

        // Guardar la conexión
        {
            let mut connections = self.peer_connections.write().await;
            connections.insert(client_id.to_string(), peer_connection.clone());
        }

        log::info!("✅ Answer WebRTC creada para cliente: {}", client_id);
        Ok(answer)
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
                sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42001f".to_string(),
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

    /// Inicializar pipeline RTP para WebRTC
    pub async fn initialize_rtp_pipeline(&self, rtsp_url: &str) -> Result<(), VigilanteError> {
        log::info!("🎥 Inicializando pipeline RTP para WebRTC (audio + video) desde: {}", rtsp_url);

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
            .build()?;
        let audio_appsink = gst::ElementFactory::make("appsink")
            .name("audio_appsink")
            .property("sync", false)
            .property("async", false)
            .build()?;

        // Agregar elementos al pipeline
        pipeline.add_many(&[
            &rtspsrc,
            &rtph264depay, &h264parse, &rtph264pay, &video_appsink,
            &rtpopusdepay, &opusparse, &rtpopuspay, &audio_appsink,
        ])?;

        // Conectar pipelines de video y audio
        rtph264depay.link(&h264parse)?;
        h264parse.link(&rtph264pay)?;
        rtph264pay.link(&video_appsink)?;

        rtpopusdepay.link(&opusparse)?;
        opusparse.link(&rtpopuspay)?;
        rtpopuspay.link(&audio_appsink)?;

        // Conectar rtspsrc dinámicamente a ambos pipelines
        let video_appsink_weak = video_appsink.downgrade();
        let audio_appsink_weak = audio_appsink.downgrade();

        rtspsrc.connect_pad_added(move |_, src_pad| {
            let caps = src_pad.current_caps().unwrap();
            let structure = caps.structure(0).unwrap();
            let media_type = structure.name();

            if media_type.starts_with("application/x-rtp") {
                if let Ok(payload) = structure.get::<i32>("payload") {
                    match payload {
                        96 => { // H.264 video
                            if let Some(video_appsink) = video_appsink_weak.upgrade() {
                                let sink_pad = video_appsink.static_pad("sink").unwrap();
                                if !sink_pad.is_linked() {
                                    src_pad.link(&sink_pad).unwrap();
                                    log::info!("🎥 Conectado stream de video RTP");
                                }
                            }
                        },
                        97 => { // Opus audio
                            if let Some(audio_appsink) = audio_appsink_weak.upgrade() {
                                let sink_pad = audio_appsink.static_pad("sink").unwrap();
                                if !sink_pad.is_linked() {
                                    src_pad.link(&sink_pad).unwrap();
                                    log::info!("🎵 Conectado stream de audio RTP");
                                }
                            }
                        },
                        _ => log::warn!("⚠️ Payload RTP desconocido: {}", payload),
                    }
                }
            }
        });

        // Configurar appsinks para video y audio
        let video_appsink = video_appsink.dynamic_cast::<gst_app::AppSink>().unwrap();
        let audio_appsink = audio_appsink.dynamic_cast::<gst_app::AppSink>().unwrap();

        // Canales separados para video y audio
        let (video_tx, mut video_rx) = tokio::sync::mpsc::channel(100);
        let (audio_tx, mut audio_rx) = tokio::sync::mpsc::channel(100);

        // Configurar callback para video
        let video_tx_clone = video_tx.clone();
        video_appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |appsink| {
                    let sample = appsink.pull_sample().unwrap();
                    let buffer = sample.buffer().unwrap();
                    let data = buffer.map_readable().unwrap();
                    let rtp_data = bytes::Bytes::copy_from_slice(&data);

                    let tx_clone = video_tx_clone.clone();
                    tokio::spawn(async move {
                        let _ = tx_clone.send(rtp_data).await;
                    });
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

                    let tx_clone = audio_tx_clone.clone();
                    tokio::spawn(async move {
                        let _ = tx_clone.send(rtp_data).await;
                    });
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        // Crear y almacenar tracks
        let video_track = Arc::new(self.create_video_track().await?);
        let audio_track = Arc::new(self.create_audio_track().await?);

        {
            let mut vt = self.video_track.write().await;
            *vt = Some(video_track);
            let mut at = self.audio_track.write().await;
            *at = Some(audio_track);
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