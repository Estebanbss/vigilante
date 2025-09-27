use crate::{auth::RequireAuth, AppState};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
// use axum::response::sse::{Sse, Event};
// use axum::response::Html;
use axum::body::Body;
use bytes::Bytes;
// use futures::Stream;
// use futures::StreamExt;
// use gstreamer as gst;
// use gstreamer::prelude::*;
// use std::pin::Pin;
use std::sync::Arc;
// use std::path::PathBuf;
// use std::fs;
use serde::Deserialize;
use std::time::{Duration, Instant};
use tokio::fs::File;
use tokio_util::io::ReaderStream;

// Autenticación: por header Authorization global y opcionalmente por query `?token=` en rutas de streaming si STREAM_TOKEN_IN_QUERY=true

pub async fn stream_hls_handler(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(path): Path<String>,
) -> impl IntoResponse {
    // Sirve archivos HLS desde STORAGE_PATH/hls
    let mut rel = path.trim_start_matches('/').to_string();
    if rel.is_empty() {
        rel = "stream.m3u8".to_string();
    }
    let hls_root = state.storage_path.join("hls");
    let candidate = hls_root.join(&rel);
    let Ok(full_path) = candidate.canonicalize() else {
        return (StatusCode::NOT_FOUND, "").into_response();
    };
    let Ok(root) = hls_root.canonicalize() else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "").into_response();
    };
    if !full_path.starts_with(&root) {
        return (StatusCode::FORBIDDEN, "").into_response();
    }

    let file = match File::open(&full_path).await {
        Ok(f) => f,
        Err(_) => return (StatusCode::NOT_FOUND, "").into_response(),
    };
    let stream = ReaderStream::new(file);
    let mut resp = Response::new(Body::from_stream(stream));
    let headers = resp.headers_mut();
    let ctype = if rel.ends_with(".m3u8") {
        "application/vnd.apple.mpegurl"
    } else if rel.ends_with(".ts") {
        "video/mp2t"
    } else {
        "application/octet-stream"
    };
    headers.insert(axum::http::header::CONTENT_TYPE, ctype.parse().unwrap());
    // CORS headers for streaming
    headers.insert("Access-Control-Allow-Origin", "*".parse().unwrap());
    headers.insert(
        "Access-Control-Allow-Methods",
        "GET, POST, OPTIONS".parse().unwrap(),
    );
    headers.insert("Access-Control-Allow-Headers", "*".parse().unwrap());
    resp
}

// Alias para /hls sin path, devuelve stream.m3u8
pub async fn stream_hls_index(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Response {
    // Reutiliza la misma lógica, sirviendo el playlist por defecto
    let path = Path("".to_string());
    let res = stream_hls_handler(RequireAuth, State(state), path).await;
    axum::response::IntoResponse::into_response(res)
}

pub async fn stream_webrtc_handler(
    RequireAuth: RequireAuth,
    State(_state): State<Arc<AppState>>,
) -> impl IntoResponse {
    // Placeholder para la lógica de GStreamer
    let mut resp = (StatusCode::OK, "WebRTC stream handler is working!").into_response();
    let headers = resp.headers_mut();
    // CORS headers for streaming
    headers.insert("Access-Control-Allow-Origin", "*".parse().unwrap());
    headers.insert(
        "Access-Control-Allow-Methods",
        "GET, POST, OPTIONS".parse().unwrap(),
    );
    headers.insert("Access-Control-Allow-Headers", "*".parse().unwrap());
    resp
}

// Simple MJPEG live endpoint (separado de la grabación)
// GET /api/live/mjpeg
pub async fn stream_mjpeg_handler(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
    Query(params): Query<MjpegParams>,
) -> Result<Response, StatusCode> {
    // Suscribimos al broadcast de JPEGs (high o low) producido por el pipeline principal
    let mut rx = if params.preset.as_deref() == Some("low") {
        state.mjpeg_low_tx.subscribe()
    } else {
        state.mjpeg_tx.subscribe()
    };

    // Creamos un body streaming multipart/x-mixed-replace
    let boundary = "frame";
    let mut first = true;
    let target_fps = params.fps.unwrap_or(0);
    let frame_interval = if target_fps > 0 {
        Some(Duration::from_secs_f64(1.0 / target_fps as f64))
    } else {
        None
    };
    let mut last_sent: Option<Instant> = None;
    let stream = async_stream::stream! {
        while let Ok(jpeg) = rx.recv().await {
            // FPS throttling per-request
            if let Some(iv) = frame_interval {
                let now = Instant::now();
                if let Some(last) = last_sent {
                    if now.duration_since(last) < iv { continue; }
                }
                last_sent = Some(now);
            }
            let mut chunk = Vec::with_capacity(jpeg.len() + 256);
            if first {
                first = false;
                // Send initial boundary for multipart stream
                chunk.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
            } else {
                chunk.extend_from_slice(format!("\r\n--{}\r\n", boundary).as_bytes());
            }
            chunk.extend_from_slice(format!("Content-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", jpeg.len()).as_bytes());
            chunk.extend_from_slice(&jpeg);
            yield Ok::<Bytes, std::io::Error>(Bytes::from(chunk));
        }
        // Send closing boundary when stream ends
        yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("\r\n--{}--\r\n", boundary)));
    };

    let mut resp = Response::new(Body::from_stream(stream));
    let headers = resp.headers_mut();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        format!("multipart/x-mixed-replace; boundary={}", boundary)
            .parse()
            .unwrap(),
    );
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        "no-cache".parse().unwrap(),
    );
    // CORS headers for streaming
    headers.insert("Access-Control-Allow-Origin", "*".parse().unwrap());
    headers.insert(
        "Access-Control-Allow-Methods",
        "GET, POST, OPTIONS".parse().unwrap(),
    );
    headers.insert("Access-Control-Allow-Headers", "*".parse().unwrap());
    Ok(resp)
}

#[derive(Debug, Deserialize)]
pub struct MjpegParams {
    pub fps: Option<u32>,
    pub preset: Option<String>, // "high" (default) o "low"
                                // Futuro: calidad/resolución. Por ahora no re-comprimimos para evitar alto CPU.
                                // pub quality: Option<u8>,
                                // pub width: Option<u32>,
                                // pub height: Option<u32>,
}

// Live audio endpoint over chunked HTTP (MP3)
// GET /api/live/audio
pub async fn stream_audio_handler(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Result<Response, StatusCode> {
    // Verificar si el audio está disponible
    if !*state.audio_available.lock().await {
        eprintln!("❌ Audio no disponible en el stream RTSP");
        return Err(StatusCode::NOT_FOUND);
    }

    // Suscripción al canal de audio
    let mut rx = state.audio_mp3_tx.subscribe();

    let stream = async_stream::stream! {
        let mut consecutive_errors = 0;
        let max_consecutive_errors = 5;

        loop {
            match tokio::time::timeout(Duration::from_secs(30), rx.changed()).await {
                Ok(Ok(_)) => {
                    let chunk = rx.borrow().clone();
                    if !chunk.is_empty() {
                        consecutive_errors = 0; // Reset error counter on success
                        yield Ok::<Bytes, std::io::Error>(chunk);
                    } else {
                        eprintln!("⚠️  Chunk de audio vacío recibido");
                    }
                }
                Ok(Err(_)) => {
                    eprintln!("❌ Audio watch channel closed");
                    break;
                }
                Err(_) => {
                    consecutive_errors += 1;
                    eprintln!("⏰ Timeout esperando datos de audio (error #{})", consecutive_errors);

                    if consecutive_errors >= max_consecutive_errors {
                        eprintln!("❌ Demasiados timeouts consecutivos, cerrando stream de audio");
                        break;
                    }

                    // Enviar un chunk vacío para mantener la conexión viva
                    yield Ok::<Bytes, std::io::Error>(Bytes::new());
                }
            }
        }
    };

    let mut resp = Response::new(Body::from_stream(stream));
    let headers = resp.headers_mut();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        "audio/mpeg".parse().unwrap(),
    );
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        "no-cache".parse().unwrap(),
    );
    headers.insert(axum::http::header::PRAGMA, "no-cache".parse().unwrap());
    headers.insert(axum::http::header::EXPIRES, "0".parse().unwrap());
    // CORS headers for streaming
    headers.insert("Access-Control-Allow-Origin", "*".parse().unwrap());
    headers.insert(
        "Access-Control-Allow-Methods",
        "GET, POST, OPTIONS".parse().unwrap(),
    );
    headers.insert("Access-Control-Allow-Headers", "*".parse().unwrap());
    headers.insert("X-Content-Type-Options", "nosniff".parse().unwrap());
    headers.insert("Transfer-Encoding", "chunked".parse().unwrap());
    Ok(resp)
}
// HLS ahora es generado dentro del pipeline principal (camera.rs)
