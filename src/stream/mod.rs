//! Módulo de streaming para Vigilante.
//!
//! Maneja transmisión de audio y video en tiempo real.

pub mod depends;

pub use depends::audio::AudioStreamer;
pub use depends::websocket::WebSocketHandler;

use axum::{
    extract::State,
    response::{IntoResponse, Response},
    http::{header, StatusCode},
};
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt as TokioStreamExt;
use crate::error::VigilanteError;

/// Manager principal para streaming.
#[derive(Clone)]
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
pub async fn stream_audio_handler(
    State(state): State<Arc<crate::AppState>>,
) -> impl IntoResponse {
    use axum::body::Body;

    let mut audio_rx = state.streaming.audio_mp3_tx.subscribe();

    // Create a stream that yields audio chunks
    let stream = async_stream::stream! {
        while audio_rx.changed().await.is_ok() {
            let chunk = audio_rx.borrow().clone();
            if !chunk.is_empty() {
                yield Ok::<_, std::io::Error>(chunk);
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
            .header(header::CONTENT_TYPE, "multipart/x-mixed-replace; boundary=frame")
            .header(header::CACHE_CONTROL, "no-cache")
            .header(header::CONNECTION, "close")
            .body(axum::body::Body::empty())
            .unwrap();
    }

    // Check if pipeline is running before attempting to stream
    let pipeline_running = *state.gstreamer.pipeline_running.lock().unwrap();
    if !pipeline_running {
        log::warn!("MJPEG stream requested but pipeline is not running");
        return Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(axum::body::Body::from("Camera pipeline not running"))
            .unwrap();
    }

    let mjpeg_rx = state.streaming.mjpeg_tx.subscribe();
    log::info!("📺 MJPEG stream handler: subscribed to broadcast channel");

    let stream = TokioStreamExt::map(BroadcastStream::new(mjpeg_rx), |result| match result {
        Ok(bytes) => {
            log::debug!("📺 MJPEG frame received in stream handler, size: {} bytes", bytes.len());
            Ok::<_, Infallible>(bytes)
        },
        Err(e) => {
            log::warn!("📺 MJPEG broadcast channel error: {:?}", e);
            Ok(bytes::Bytes::new())
        }
    });

    let body = axum::body::Body::from_stream(stream);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "multipart/x-mixed-replace; boundary=frame")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "close")
        .body(body)
        .unwrap()
}