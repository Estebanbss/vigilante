use std::{env, net::SocketAddr, sync::Arc, path::PathBuf};
use axum::{
    routing::{get, post},
    Router,
};
use tower_http::cors::{CorsLayer, Any};
use dotenvy::dotenv;
use tokio::sync::Mutex;

mod auth;
mod storage;
mod stream;
mod camera;
mod ptz;
mod logs;

use storage::{get_storage_info, list_recordings, delete_recording, stream_recording, start_cleanup_task, get_log_file, stream_recording_tail, storage_stream_ws, recordings_stream_ws};
use stream::{stream_hls_handler, stream_hls_index, stream_webrtc_handler, stream_mjpeg_handler, stream_audio_handler};
use logs::stream_journal_logs;
use camera::{start_camera_pipeline};
use ptz::{pan_left, pan_right, tilt_up, tilt_down, zoom_in, zoom_out, ptz_stop};
use axum::middleware::from_fn_with_state;

// Dependencias de GStreamer
use gstreamer as gst;
use bytes::Bytes;
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct AppState {
    pub camera_rtsp_url: String,
    pub camera_onvif_url: String,
    pub proxy_token: String,
    pub storage_path: PathBuf,
    pub pipeline: Arc<Mutex<Option<gst::Pipeline>>>,
    pub mjpeg_tx: broadcast::Sender<Bytes>,
    pub mjpeg_low_tx: broadcast::Sender<Bytes>,
    pub audio_webm_tx: broadcast::Sender<Bytes>,
    pub enable_hls: bool,
    // Permite validar token por query (p.ej., ?token=...) en rutas de streaming
    pub allow_query_token_streams: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv().ok();

    gst::init()?;

    let camera_rtsp_url = env::var("CAMERA_RTSP_URL")?;
    let camera_onvif_url = env::var("CAMERA_ONVIF_URL")?;
    let proxy_token = env::var("PROXY_TOKEN")?;
    let listen_addr = env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let storage_path = env::var("STORAGE_PATH")?;
    let enable_hls = env::var("ENABLE_HLS").map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(false);
    // Soporte de compatibilidad: STREAM_MJPEG_TOKEN_IN_QUERY también habilita el modo de token en query
    let allow_query_token_streams =
        env::var("STREAM_TOKEN_IN_QUERY").map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(false)
        || env::var("STREAM_MJPEG_TOKEN_IN_QUERY").map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(false);

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let storage_path_buf = PathBuf::from(&storage_path);

    let (mjpeg_tx, _mjpeg_rx) = broadcast::channel::<Bytes>(32);
    let (mjpeg_low_tx, _mjpeg_low_rx) = broadcast::channel::<Bytes>(32);
    let (audio_webm_tx, _audio_rx) = broadcast::channel::<Bytes>(32);

    let state = Arc::new(AppState {
        camera_rtsp_url: camera_rtsp_url.clone(),
        camera_onvif_url: camera_onvif_url.clone(),
        proxy_token: proxy_token.clone(),
        storage_path: storage_path_buf.clone(),
        pipeline: Arc::new(Mutex::new(None)),
        mjpeg_tx,
        mjpeg_low_tx,
        audio_webm_tx,
        enable_hls,
        allow_query_token_streams,
    });

    // Iniciar la tarea de limpieza de almacenamiento en segundo plano
    tokio::spawn(start_cleanup_task(storage_path_buf));

    // Iniciar el pipeline de la cámara para la grabación 24/7 y detección de eventos
    tokio::spawn(start_camera_pipeline(camera_rtsp_url, state.clone()));

    // HLS ahora se genera dentro del pipeline principal en camera.rs mediante un branch del tee

    let app = Router::new()
        .route("/hls", get(stream_hls_index))
        .route("/hls/*path", get(stream_hls_handler))
        .route("/webrtc/*path", get(stream_webrtc_handler))
    .route("/api/live/mjpeg", get(stream_mjpeg_handler))
    .route("/api/live/audio", get(stream_audio_handler))
        .route("/api/logs/stream", get(stream_journal_logs))
        .route("/api/storage", get(get_storage_info))
        .route("/api/storage/list", get(list_recordings))
        .route("/api/storage/stream", get(storage_stream_ws))
        .route("/api/recordings/stream", get(recordings_stream_ws))
        .route("/api/storage/delete/*path", get(delete_recording))
        .route("/api/recordings/stream/*path", get(stream_recording))
    .route("/api/recordings/stream/tail/*path", get(stream_recording_tail))
        .route("/api/recordings/log/:date", get(get_log_file))
        // Rutas para el control PTZ
        .route("/api/ptz/pan/left", post(pan_left))
        .route("/api/ptz/pan/right", post(pan_right))
        .route("/api/ptz/tilt/up", post(tilt_up))
        .route("/api/ptz/tilt/down", post(tilt_down))
        .route("/api/ptz/zoom/in", post(zoom_in))
        .route("/api/ptz/zoom/out", post(zoom_out))
        .route("/api/ptz/stop", post(ptz_stop))
    .layer(cors)
    // Middleware global de autenticación para TODAS las rutas
    .layer(from_fn_with_state(state.clone(), auth::require_auth_middleware))
    .with_state(state);

    let addr: SocketAddr = listen_addr.parse()?;
    println!("🚀 API y Streamer escuchando en http://{}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}