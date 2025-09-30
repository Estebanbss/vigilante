//! Módulo de streaming para Vigilante.
//!
//! Maneja transmisión de audio y video en tiempo real.

pub mod depends;

pub use depends::audio::AudioStreamer;
pub use depends::websocket::WebSocketHandler;

use crate::error::VigilanteError;
use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::time::{timeout, Instant as TokioInstant};
use tokio_stream::wrappers::ReceiverStream;

/// Manager principal para streaming.
pub struct StreamManager {
    audio_streamer: AudioStreamer,
    websocket_handler: WebSocketHandler,
}

impl StreamManager {
    pub fn new(context: Arc<crate::AppState>) -> Self {
        Self {
            audio_streamer: AudioStreamer::new(context.clone()),
            websocket_handler: WebSocketHandler::new(context),
        }
    }

    pub async fn start_streaming(&self) -> Result<(), VigilanteError> {
        // Start audio streaming
        self.audio_streamer.start_stream().await?;

        // WebSocket handling is done per connection in handlers
        log::info!("Stream manager initialized successfully");
        Ok(())
    }

    pub fn get_audio_streamer(&self) -> &AudioStreamer {
        &self.audio_streamer
    }

    pub fn get_websocket_handler(&self) -> &WebSocketHandler {
        &self.websocket_handler
    }

    pub async fn get_stream_status(&self) -> serde_json::Value {
        serde_json::json!({
            "audio_available": self.audio_streamer.is_audio_available(),
            "video_streaming": true, // MJPEG is always available if camera is connected
            "websocket_enabled": true
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

#[axum::debug_handler]
pub async fn stream_mjpeg_handler(
    method: axum::http::Method,
    State(state): State<Arc<crate::AppState>>,
) -> impl IntoResponse {
    // For HEAD requests, just return headers without streaming
    if method == axum::http::Method::HEAD {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "video/mp4")
            .header(header::CACHE_CONTROL, "no-cache")
            .header(header::CONNECTION, "close")
            .body(axum::body::Body::empty())
            .unwrap();
    }

    // Check if pipeline is running, start it if not
    let pipeline_running = *state.gstreamer.pipeline_running.lock().unwrap();
    if !pipeline_running {
        log::info!(
            "📺 MP4 stream requested but pipeline not running, starting recording automatically"
        );
        let camera_pipeline = {
            let guard = state.camera_pipeline.lock().unwrap();
            guard.as_ref().cloned()
        };
        if let Some(camera_pipeline) = camera_pipeline {
            if let Err(e) = camera_pipeline.start_recording().await {
                log::error!("📺 Failed to start recording: {:?}", e);
                return Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .header(header::CONTENT_TYPE, "text/plain")
                    .body(axum::body::Body::from("Failed to start camera pipeline"))
                    .unwrap();
            }
            log::info!("📺 Pipeline started successfully for streaming");
        } else {
            log::error!("📺 Camera pipeline not initialized");
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header(header::CONTENT_TYPE, "text/plain")
                .body(axum::body::Body::from("Camera pipeline not initialized"))
                .unwrap();
        }
    }

    let init_segments = {
        let segments_guard = state.streaming.mp4_init_segments.lock().unwrap();
        segments_guard.clone()
    };


    let mut receiver = state.streaming.mjpeg_tx.subscribe();
    const MAX_FRAGMENT_BUNDLE: usize = 10;
    const INIT_MAX_BUFFERED_FRAGMENTS: usize = 64;
    const INIT_WAIT_TIMEOUT: Duration = Duration::from_millis(1800);
    const INIT_RECV_TIMEOUT: Duration = Duration::from_millis(200);
    log::info!("📺 MP4 stream handler: subscribed to broadcast channel");

    // Create a bounded channel for streaming data
    let (tx, rx) = mpsc::channel::<Result<bytes::Bytes, Infallible>>(32);

    // Spawn a task to handle the streaming logic
    tokio::spawn(async move {
        let mut init_detector = Mp4InitDetector::default();
        let mut handshake_segments: Vec<bytes::Bytes> = Vec::new();
        let mut delivered_fragments: usize = 0;

        if !init_segments.is_empty() {
            let total_bytes = init_segments.iter().map(|seg| seg.len()).sum::<usize>();
            log::debug!(
                "📺 Cached {} MP4 init fragments for new client (total {} bytes, complete: {})",
                init_segments.len(),
                total_bytes,
                init_detector.is_complete()
            );
            for segment in init_segments {
                init_detector.ingest(segment.as_ref());
                handshake_segments.push(segment);
            }
        } else {
            log::warn!("📺 Client connected before MP4 init fragments were available");
        }

        log::info!("📦 Init detector status: ftyp={}, moov={}, moof={}, complete={}", init_detector.has_ftyp(), init_detector.has_moov(), init_detector.has_moof(), init_detector.is_complete());

        if !init_detector.is_complete() {
            let deadline = TokioInstant::now() + INIT_WAIT_TIMEOUT;
            log::info!(
                "📦 Waiting for MP4 init completion (ftyp: {}, moov: {}, buffered: {})",
                init_detector.has_ftyp(),
                init_detector.has_moov(),
                handshake_segments.len()
            );

            while !init_detector.is_complete()
                && TokioInstant::now() < deadline
                && handshake_segments.len() < INIT_MAX_BUFFERED_FRAGMENTS
            {
                match timeout(INIT_RECV_TIMEOUT, receiver.recv()).await {
                    Ok(Ok(bytes)) => {
                        init_detector.ingest(bytes.as_ref());
                        log::debug!(
                            "📦 Buffered live fragment during init wait (size {} bytes)",
                            bytes.len()
                        );
                        handshake_segments.push(bytes);
                    }
                    Ok(Err(broadcast::error::RecvError::Lagged(skipped))) => {
                        log::warn!(
                            "📦 Receiver lagged while waiting for init fragments; skipped {}",
                            skipped
                        );
                    }
                    Ok(Err(broadcast::error::RecvError::Closed)) => {
                        log::info!("📦 Broadcast channel closed while waiting for init fragments");
                        return;
                    }
                    Err(_) => {
                        log::debug!("📦 Timeout while waiting for init fragment; retrying");
                    }
                }
            }

            if init_detector.is_complete() {
                log::info!(
                    "📦 MP4 init completed (ftyp: {}, moov: {}, buffered: {})",
                    init_detector.has_ftyp(),
                    init_detector.has_moov(),
                    handshake_segments.len()
                );
            } else {
                log::warn!(
                    "📦 Proceeding without confirmed MP4 init (ftyp: {}, moov: {}, buffered: {})",
                    init_detector.has_ftyp(),
                    init_detector.has_moov(),
                    handshake_segments.len()
                );
            }
        }

        if handshake_segments.is_empty() {
            log::warn!("📦 No MP4 fragments buffered for client handshake; playback may stall");
        } else {
            log::info!("📦 Sending {} handshake segments to client (total {} bytes)", handshake_segments.len(), handshake_segments.iter().map(|s| s.len()).sum::<usize>());
        }

        // Send handshake segments
        for (i, segment) in handshake_segments.iter().enumerate() {
            log::debug!("📦 Sending handshake segment {} ({} bytes)", i + 1, segment.len());
            if tx.send_timeout(Ok(segment.clone()), Duration::from_millis(100)).await.is_err() {
                log::info!("📺 Client disconnected durante el handshake en el segmento {}", i + 1);
                return;
            }
            delivered_fragments = delivered_fragments.saturating_add(1);
        }

        log::info!("📺 Starting continuous fragment streaming to client");
        loop {
            log::debug!("📺 Waiting for next fragment from broadcast channel");
            match tokio::time::timeout(Duration::from_secs(5), receiver.recv()).await {
                Ok(recv_result) => match recv_result {
                    Ok(first_bytes) => {
                        use tokio::sync::broadcast::error::TryRecvError;

                        let mut bundle: Vec<bytes::Bytes> = vec![first_bytes];

                        // Try to accumulate more fragments quickly
                        for _ in 0..(MAX_FRAGMENT_BUNDLE - 1) {
                            match receiver.try_recv() {
                                Ok(next_bytes) => {
                                    bundle.push(next_bytes);
                                }
                                Err(TryRecvError::Empty) => {
                                    break;
                                }
                                Err(TryRecvError::Lagged(skipped)) => {
                                    log::warn!(
                                        "📺 Receiver lagged while accumulating bundle; skipped {} fragments",
                                        skipped
                                    );
                                    break;
                                }
                                Err(TryRecvError::Closed) => {
                                    log::info!("📺 Broadcast channel closed while accumulating bundle");
                                    break;
                                }
                            }
                        }

                        log::debug!("📺 Sending bundle of {} fragments to client", bundle.len());
                        for fragment in bundle {
                            log::debug!(
                                "📺 MP4 fragment dispatched to client, size: {} bytes",
                                fragment.len()
                            );
                            if tx.send_timeout(Ok(fragment), Duration::from_millis(100)).await.is_err() {
                                log::info!("📺 Client disconnected during streaming, sent {} fragments", delivered_fragments);
                                return;
                            }
                            delivered_fragments = delivered_fragments.saturating_add(1);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        log::warn!(
                            "📺 Receiver lagged behind broadcast channel; skipped {} fragments",
                            skipped
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        log::info!("📺 Broadcast channel closed, ending stream after {} fragments", delivered_fragments);
                        break;
                    }
                }
                Err(_) => {
                    log::info!("📺 Timeout waiting for fragment, client likely disconnected after {} fragments", delivered_fragments);
                    break;
                }
            }
        }

        log::info!("📺 Stream ended, total fragments delivered: {}", delivered_fragments);
    });

    let stream = ReceiverStream::new(rx);

    let body = axum::body::Body::from_stream(stream);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "video/mp4")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "close")
        .body(body)
        .unwrap()
}

#[derive(Default, Debug)]
struct Mp4InitDetector {
    has_ftyp: bool,
    has_moov: bool,
    has_moof: bool,
    tail: Vec<u8>,
}

impl Mp4InitDetector {
    fn ingest(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        if !self.has_ftyp {
            self.has_ftyp = data.windows(4).any(|w| w == b"ftyp");
        }
        if !self.has_moov {
            self.has_moov = data.windows(4).any(|w| w == b"moov");
        }
        if !self.has_moof {
            self.has_moof = data.windows(4).any(|w| w == b"moof");
        }

        if !(self.has_ftyp && self.has_moov) && !self.tail.is_empty() {
            let mut combined = Vec::with_capacity(self.tail.len() + data.len());
            combined.extend_from_slice(&self.tail);
            combined.extend_from_slice(data);
            if !self.has_ftyp {
                self.has_ftyp = combined.windows(4).any(|w| w == b"ftyp");
            }
            if !self.has_moov {
                self.has_moov = combined.windows(4).any(|w| w == b"moov");
            }
            if !self.has_moof {
                self.has_moof = combined.windows(4).any(|w| w == b"moof");
            }
        }

        self.tail.clear();
        let tail_len = data.len().min(3);
        if tail_len > 0 {
            self.tail
                .extend_from_slice(&data[data.len().saturating_sub(tail_len)..]);
        }
    }

    fn has_ftyp(&self) -> bool {
        self.has_ftyp
    }

    fn has_moov(&self) -> bool {
        self.has_moov
    }

    fn has_moof(&self) -> bool {
        self.has_moof
    }

    fn is_complete(&self) -> bool {
        self.has_ftyp && self.has_moov && !self.has_moof
    }
}
