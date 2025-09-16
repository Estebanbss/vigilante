use crate::{auth::check_auth, AppState};
use axum::{
    extract::{State, Path, Query},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
// use axum::response::sse::{Sse, Event};
// use axum::response::Html;
use axum::body::Body;
use bytes::Bytes;
// use futures::Stream;
// use futures::StreamExt;
use gstreamer as gst;
use gstreamer::prelude::*;
// use std::pin::Pin;
use std::sync::Arc;
use std::path::PathBuf;
use std::fs;
use tokio::fs::File;
use tokio_util::io::ReaderStream;
use serde::Deserialize;

#[derive(Deserialize, Default)]
pub struct TokenQuery { pub token: Option<String> }

pub async fn stream_hls_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(path): Path<String>,
    Query(q): Query<TokenQuery>,
) -> impl IntoResponse {
    // Auth: acepta header Authorization o query ?token=
    let authorized = q.token.as_deref() == Some(&state.proxy_token);
    if !authorized {
        if let Err(_) = check_auth(&headers, &state.proxy_token).await { return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response(); }
    }

    // Sirve archivos HLS desde STORAGE_PATH/hls
    let mut rel = path.trim_start_matches('/').to_string();
    if rel.is_empty() { rel = "stream.m3u8".to_string(); }
    let hls_root = state.storage_path.join("hls");
    let candidate = hls_root.join(&rel);
    let Ok(full_path) = candidate.canonicalize() else {
        return (StatusCode::NOT_FOUND, "").into_response();
    };
    let Ok(root) = hls_root.canonicalize() else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "").into_response();
    };
    if !full_path.starts_with(&root) { return (StatusCode::FORBIDDEN, "").into_response(); }

    let file = match File::open(&full_path).await { Ok(f)=> f, Err(_)=> return (StatusCode::NOT_FOUND, "").into_response() };
    let stream = ReaderStream::new(file);
    let mut resp = Response::new(Body::from_stream(stream));
    let headers = resp.headers_mut();
    let ctype = if rel.ends_with(".m3u8") { "application/vnd.apple.mpegurl" } else if rel.ends_with(".ts") { "video/mp2t" } else { "application/octet-stream" };
    headers.insert(axum::http::header::CONTENT_TYPE, ctype.parse().unwrap());
    resp
}

// Alias para /hls sin path, devuelve stream.m3u8
pub async fn stream_hls_index(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> impl IntoResponse {
    // Reutiliza la misma lógica, sirviendo el playlist por defecto
    let path = Path("".to_string());
    stream_hls_handler(State(state), headers, path, Query(q)).await
}

pub async fn stream_webrtc_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(status) = check_auth(&headers, &state.proxy_token).await {
        return (status, "Unauthorized").into_response();
    }
    
    // Placeholder para la lógica de GStreamer
    (StatusCode::OK, "WebRTC stream handler is working!").into_response()
}

// Simple MJPEG live endpoint (separado de la grabación)
// GET /api/live/mjpeg
pub async fn stream_mjpeg_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
) -> Result<Response, StatusCode> {
    // Auth: acepta header Authorization o query ?token=
    if q.token.as_deref() != Some(&state.proxy_token) {
        if let Err(_status) = check_auth(&headers, &state.proxy_token).await { return Err(StatusCode::UNAUTHORIZED); }
    }

    // Suscribimos al broadcast de JPEGs producido por el pipeline principal
    let mut rx = state.mjpeg_tx.subscribe();

    // Creamos un body streaming multipart/x-mixed-replace
    let boundary = "frame";
    let mut first = true;
    let stream = async_stream::stream! {
        while let Ok(jpeg) = rx.recv().await {
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
    headers.insert(axum::http::header::CONTENT_TYPE, format!("multipart/x-mixed-replace; boundary={}", boundary).parse().unwrap());
    headers.insert(axum::http::header::CACHE_CONTROL, "no-cache".parse().unwrap());
    Ok(resp)
}

// HLS ahora es generado dentro del pipeline principal (camera.rs)
