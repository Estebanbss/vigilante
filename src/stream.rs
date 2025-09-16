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

        // Intentar primero decodificación por hardware (si existe en la Pi): v4l2h264dec
        let pipeline_str_hw = format!(
            "rtspsrc location={camera_url} protocols=tcp latency=50 ! rtph264depay ! h264parse ! v4l2h264dec ! videoconvert ! videoscale ! video/x-raw,width=1280,height=720 ! queue leaky=downstream max-size-buffers=1 max-size-time=0 max-size-bytes=0 ! jpegenc quality=85 ! queue leaky=downstream max-size-buffers=1 max-size-time=0 max-size-bytes=0 ! appsink name=sink sync=false max-buffers=1 drop=true"
        );
        eprintln!("[MJPEG] Trying HW decode pipeline: {pipeline_str_hw}");
        let pipeline = match gst::parse::launch(&pipeline_str_hw) {
            Ok(e) => {
                eprintln!("[MJPEG] Using HW decoder v4l2h264dec (1280x720 downscale)");
                e.downcast::<gst::Pipeline>().unwrap()
            }
            Err(err_hw) => {
                eprintln!("[MJPEG] HW decode not available ({err_hw}). Falling back to SW (avdec_h264)...");
                let pipeline_str_sw = format!(
                    "rtspsrc location={camera_url} protocols=tcp latency=50 ! rtph264depay ! h264parse ! avdec_h264 ! videoconvert ! videoscale ! video/x-raw,width=1280,height=720 ! queue leaky=downstream max-size-buffers=1 max-size-time=0 max-size-bytes=0 ! jpegenc quality=85 ! queue leaky=downstream max-size-buffers=1 max-size-time=0 max-size-bytes=0 ! appsink name=sink sync=false max-buffers=1 drop=true"
                );
                eprintln!("[MJPEG] Launching SW pipeline: {pipeline_str_sw}");
                match gst::parse::launch(&pipeline_str_sw) {
                    Ok(e) => e.downcast::<gst::Pipeline>().unwrap(),
                    Err(err_sw) => {
                        eprintln!("[MJPEG] SW pipeline error: {err_sw}");
                        return;
                    }
                }
            }
        };

        let appsink = pipeline.by_name("sink").unwrap().downcast::<gst_app::AppSink>().unwrap();
        appsink.set_caps(Some(&gst::Caps::builder("image/jpeg").build()));

        let bus = pipeline.bus();

        // Set pipeline to PLAYING and wait for state change to complete
        match pipeline.set_state(gst::State::Playing) {
            Ok(success) => {
                eprintln!("[MJPEG] Pipeline set_state(Playing): {:?}", success);
                // For live sources, NoPreroll is expected. If Async, wait a little for stabilization.
                match success {
                    gst::StateChangeSuccess::Success => {
                        eprintln!("[MJPEG] Pipeline immediately in Playing state");
                    }
                    gst::StateChangeSuccess::Async => {
                        eprintln!("[MJPEG] Async state change, waiting up to 10s...");
                        let (result, state, pending) = pipeline.state(Some(gst::ClockTime::from_seconds(10)));
                        match result {
                            Ok(gst::StateChangeSuccess::Success) => {
                                eprintln!("[MJPEG] State after async: {:?}, pending: {:?}", state, pending);
                            }
                            Ok(gst::StateChangeSuccess::NoPreroll) => {
                                eprintln!("[MJPEG] NoPreroll (live source) after async — continuing");
                            }
                            Ok(gst::StateChangeSuccess::Async) => {
                                eprintln!("[MJPEG] Still async after wait — continuing");
                            }
                            Err(e) => {
                                eprintln!("[MJPEG] Error waiting for state change: {:?}. Will continue and try to pull samples anyway.", e);
                            }
                        }
                    }
                    gst::StateChangeSuccess::NoPreroll => {
                        // Live pipelines often report NoPreroll when going to Playing — this is OK.
                        eprintln!("[MJPEG] NoPreroll (live source) — continuing");
                    }
                }
            }
            Err(e) => {
                eprintln!("[MJPEG] Pipeline set_state(Playing) failed: {:?}", e);
                return;
            }
        }

        // Wait a bit for the pipeline to stabilize
        std::thread::sleep(std::time::Duration::from_millis(500));

        // Bucle simple de lectura con timeout
        let appsink_clone = pipeline.by_name("sink").unwrap().downcast::<gst_app::AppSink>().unwrap();
        let mut frame_count: u64 = 0;
        let mut last_log = std::time::Instant::now();
        let mut first_frame_received = false;
        
        loop {
            // Revisa mensajes del bus rápidamente para registrar errores/EOS
            if let Some(ref bus) = bus {
                while let Some(msg) = bus.timed_pop(gst::ClockTime::from_mseconds(10)) {
                    use gstreamer::MessageView;
                    match msg.view() {
                        MessageView::Error(e) => {
                            eprintln!("[MJPEG] Bus Error from {:?}: {} ({:?})", msg.src().map(|s| s.path_string()), e.error(), e.debug());
                            let _ = pipeline.set_state(gst::State::Null);
                            return;
                        }
                        MessageView::Eos(_) => {
                            eprintln!("[MJPEG] Bus EOS received");
                            let _ = pipeline.set_state(gst::State::Null);
                            return;
                        }
                        MessageView::StateChanged(s) => {
                            if let Some(src) = msg.src() {
                                if src.is::<gst::Pipeline>() {
                                    eprintln!("[MJPEG] Pipeline state changed: {:?} -> {:?}", s.old(), s.current());
                                }
                            }
                        }
                        MessageView::StreamStart(_) => {
                            eprintln!("[MJPEG] Stream started");
                        }
                        MessageView::AsyncDone(_) => {
                            eprintln!("[MJPEG] Async operation completed");
                        }
                        _ => {}
                    }
                }
            }

            // Try to pull a sample with a timeout
            match appsink_clone.try_pull_sample(Some(gst::ClockTime::from_mseconds(1000))) {
                Some(sample) => {
                    if !first_frame_received {
                        eprintln!("[MJPEG] First frame received successfully!");
                        first_frame_received = true;
                    }
                    
                    if let Some(buffer) = sample.buffer() {
                        if let Ok(map) = buffer.map_readable() {
                            let data = Bytes::copy_from_slice(map.as_ref());
                            frame_count += 1;
                            if tx.blocking_send(data).is_err() {
                                eprintln!("[MJPEG] Client disconnected (send failed), stopping stream loop");
                                break;
                            }
                            if last_log.elapsed() > std::time::Duration::from_secs(10) {
                                eprintln!("[MJPEG] Frames sent in last 10s: {} (pipeline active)", frame_count);
                                frame_count = 0;
                                last_log = std::time::Instant::now();
                            }
                        }
                    }
                }
                None => {
                    if !first_frame_received {
                        eprintln!("[MJPEG] No frame received yet, checking pipeline state...");
                        let (result, state, pending) = pipeline.state(Some(gst::ClockTime::from_mseconds(200)));
                        match result {
                            Ok(gst::StateChangeSuccess::Success) | Ok(gst::StateChangeSuccess::Async) | Ok(gst::StateChangeSuccess::NoPreroll) => {
                                eprintln!("[MJPEG] Current state: {:?}, pending: {:?}", state, pending);
                                if state != gst::State::Playing && pending != gst::State::Playing {
                                    eprintln!("[MJPEG] Pipeline not in Playing, attempting soft restart...");
                                    let _ = pipeline.set_state(gst::State::Ready);
                                    std::thread::sleep(std::time::Duration::from_millis(150));
                                    let _ = pipeline.set_state(gst::State::Playing);
                                }
                            }
                            Err(e) => {
                                eprintln!("[MJPEG] Error getting pipeline state: {:?}", e);
                            }
                        }
                    }
                    // Continue the loop to keep trying
                    continue;
                }
            }
        }

        let _ = pipeline.set_state(gst::State::Null);
        eprintln!("[MJPEG] Pipeline set to Null (stopped)");
    });

    // Creamos un body streaming multipart/x-mixed-replace
    let boundary = "frame";
    let mut first = true;
    let stream = async_stream::stream! {
        while let Some(jpeg) = rx.recv().await {
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

        // En algunas builds de GStreamer en Raspberry Pi, hlssink2 no acepta directamente
        // caps de video/mpegts desde mpegtsmux. En su lugar, alimentamos H264 elementary
        // stream (byte-stream, AU) directamente a hlssink2, que internamente hace el mux TS.
        let pipeline_str = format!(
            concat!(
                "rtspsrc location={camera_url} protocols=tcp latency=100 ",
                "! rtph264depay ",
                "! h264parse config-interval=1 ",
                "! video/x-h264,stream-format=byte-stream,alignment=au ",
                "! hlssink2 target-duration=2 max-files=5 playlist-length=5 ",
                "location={segments} playlist-location={playlist}"
            ),
            camera_url = camera_url,
            segments = segments.to_string_lossy(),
            playlist = playlist.to_string_lossy(),
        );

        eprintln!("[HLS] Launching pipeline: {pipeline_str}");

        let pipeline = match gst::parse::launch(&pipeline_str) {
            Ok(e) => e.downcast::<gst::Pipeline>().unwrap(),
            Err(err) => { eprintln!("HLS pipeline error: {err}"); return; }
        };

        let bus = pipeline.bus();
        let _ = pipeline.set_state(gst::State::Playing);
        eprintln!("[HLS] Pipeline set to Playing");

        if let Some(bus) = bus {
            for msg in bus.iter_timed(gst::ClockTime::NONE) {
                use gstreamer::MessageView;
                match msg.view() {
                    MessageView::Eos(_) => { eprintln!("[HLS] EOS"); break; }
                    MessageView::Error(e) => { eprintln!("[HLS] error from {:?}: {} ({:?})", msg.src().map(|s| s.path_string()), e.error(), e.debug()); break; }
                    MessageView::StateChanged(s) => {
                        if let Some(src) = msg.src() {
                            if src.is::<gst::Pipeline>() {
                                eprintln!("[HLS] Pipeline state: {:?} -> {:?}", s.old(), s.current());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        let _ = pipeline.set_state(gst::State::Null);
        eprintln!("[HLS] Pipeline set to Null (stopped)");
    });
}
