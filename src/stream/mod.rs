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
use tokio_stream::{wrappers::BroadcastStream, StreamExt};

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
        log::info!("📺 MP4 stream requested but pipeline not running, starting recording automatically");
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
    let init_complete = {
        let complete_guard = state.streaming.mp4_init_complete.lock().unwrap();
        *complete_guard
    };

    let mut broadcast_stream = BroadcastStream::new(state.streaming.mjpeg_tx.subscribe());
    log::info!("📺 MP4 stream handler: subscribed to broadcast channel");

    let stream = async_stream::stream! {
        if !init_segments.is_empty() {
            log::debug!(
                "📺 Sending {} cached MP4 init fragments to new client (total {} bytes, complete: {})",
                init_segments.len(),
                init_segments.iter().map(|seg| seg.len()).sum::<usize>(),
                init_complete
            );
            for segment in init_segments {
                yield Ok::<_, Infallible>(segment);
            }
            if !init_complete {
                log::warn!(
                    "📺 MP4 init sequence sent but marked incomplete; client may see delayed start"
                );
            }
        } else {
            log::warn!("📺 Client connected before MP4 init fragments were available");
        }

        while let Some(result) = broadcast_stream.next().await {
            match result {
                Ok(bytes) => {
                    log::debug!(
                        "📺 MP4 fragment received in stream handler, size: {} bytes",
                        bytes.len()
                    );
                    yield Ok::<_, Infallible>(bytes);
                }
                Err(e) => {
                    log::warn!("📺 MP4 broadcast channel error: {:?}", e);
                }
            }
        }
    };

    let body = axum::body::Body::from_stream(stream);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "video/mp4")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "close")
        .body(body)
        .unwrap()
}
