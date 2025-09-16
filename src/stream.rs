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
use gstreamer_app as gst_app;
// use std::pin::Pin;
use std::sync::Arc;
use std::path::PathBuf;
use std::fs;
use tokio::sync::mpsc;
use tokio::task;
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

    // Creamos un canal para enviar los frames JPEG desde GStreamer al response stream
    let (tx, mut rx) = mpsc::channel::<Bytes>(16);

    let camera_url = state.camera_rtsp_url.clone();

    task::spawn_blocking(move || {
        if let Err(e) = gst::init() {
            eprintln!("GStreamer init error: {e}");
            return;
        }

        // Pipeline más simple y robusto para MJPEG
        let pipeline_str = format!(
            "rtspsrc location={camera_url} protocols=tcp latency=100 ! rtph264depay ! avdec_h264 ! videoconvert ! jpegenc quality=100 ! appsink name=sink emit-signals=true max-buffers=5 drop=true"
        );

        let pipeline = match gst::parse::launch(&pipeline_str) {
            Ok(e) => e.downcast::<gst::Pipeline>().unwrap(),
            Err(err) => {
                eprintln!("Pipeline error: {err}");
                return;
            }
        };

        let appsink = pipeline.by_name("sink").unwrap().downcast::<gst_app::AppSink>().unwrap();
        appsink.set_caps(Some(&gst::Caps::builder("image/jpeg").build()));

        appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |appsink| {
                    let sample = appsink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    let data = Bytes::copy_from_slice(map.as_ref());
                    // Envolver el frame en un boundary MJPEG
                    // NOTA: no podemos usar async aquí, así que usamos un canal no-async global
                    // Para evitar lifetimes, usamos un thread-local sender mediante once_cell o similar
                    // Aquí, para mantenerlo simple, no enviamos desde el callback directamente.
                    drop(data);
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        let _ = pipeline.set_state(gst::State::Playing);

        // Bucle simple de lectura con timeout
        let appsink_clone = pipeline.by_name("sink").unwrap().downcast::<gst_app::AppSink>().unwrap();
        loop {
            match appsink_clone.try_pull_sample(gst::ClockTime::from_seconds(1)) {
                Ok(Some(sample)) => {
                    if let Some(buffer) = sample.buffer() {
                        if let Ok(map) = buffer.map_readable() {
                            let data = Bytes::copy_from_slice(map.as_ref());
                            if tx.blocking_send(data).is_err() {
                                break;
                            }
                        }
                    }
                }
                Ok(None) => {
                    // Timeout - continuar
                    continue;
                }
                Err(err) => {
                    eprintln!("appsink pull_sample error: {err}");
                    break;
                }
            }
        }

        let _ = pipeline.set_state(gst::State::Null);
    });

    // Creamos un body streaming multipart/x-mixed-replace
    let boundary = "frame";
    let mut first = true;
    let stream = async_stream::stream! {
        while let Some(jpeg) = rx.recv().await {
            let mut chunk = Vec::with_capacity(jpeg.len() + 128);
            if first {
                first = false;
            }
            chunk.extend_from_slice(format!("--{}\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", boundary, jpeg.len()).as_bytes());
            chunk.extend_from_slice(&jpeg);
            chunk.extend_from_slice(b"\r\n");
            yield Ok::<Bytes, std::io::Error>(Bytes::from(chunk));
        }
    };

    let mut resp = Response::new(Body::from_stream(stream));
    let headers = resp.headers_mut();
    headers.insert(axum::http::header::CONTENT_TYPE, format!("multipart/x-mixed-replace; boundary={}", boundary).parse().unwrap());
    headers.insert(axum::http::header::CACHE_CONTROL, "no-cache".parse().unwrap());
    Ok(resp)
}

// Inicia un pipeline independiente para generar HLS en STORAGE_PATH/hls
pub fn start_hls_pipeline(camera_url: String, hls_dir: PathBuf) {
    // Crear directorio si no existe
    if let Err(e) = fs::create_dir_all(&hls_dir) {
        eprintln!("No se pudo crear directorio HLS: {}", e);
        return;
    }

    let segments = hls_dir.join("segment-%05d.ts");
    let playlist = hls_dir.join("stream.m3u8");

    std::thread::spawn(move || {
        if let Err(e) = gst::init() { eprintln!("GStreamer init error: {e}"); return; }

        let pipeline_str = format!(
            "rtspsrc location={camera_url} protocols=tcp latency=100 ! rtph264depay ! h264parse config-interval=1 ! mpegtsmux name=mux ! hlssink2 target-duration=2 max-files=5 playlist-length=5 location={segments} playlist-location={playlist}",
            segments = shell_escape::escape(segments.to_string_lossy()).to_string(),
            playlist = shell_escape::escape(playlist.to_string_lossy()).to_string(),
        );

        let pipeline = match gst::parse::launch(&pipeline_str) {
            Ok(e) => e.downcast::<gst::Pipeline>().unwrap(),
            Err(err) => { eprintln!("HLS pipeline error: {err}"); return; }
        };

        let bus = pipeline.bus();
        let _ = pipeline.set_state(gst::State::Playing);

        if let Some(bus) = bus {
            for msg in bus.iter_timed(gst::ClockTime::NONE) {
                use gstreamer::MessageView;
                match msg.view() {
                    MessageView::Eos(_) => { eprintln!("HLS EOS"); break; }
                    MessageView::Error(e) => { eprintln!("HLS error: {}", e.error()); break; }
                    _ => {}
                }
            }
        }

        let _ = pipeline.set_state(gst::State::Null);
    });
}
