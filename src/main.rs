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

use storage::{get_storage_info, list_recordings, delete_recording, stream_recording, start_cleanup_task, get_log_file};
use stream::{stream_hls_handler, stream_hls_index, stream_webrtc_handler, stream_mjpeg_handler};
use camera::{start_camera_pipeline};
use ptz::{pan_left, pan_right, tilt_up, tilt_down, zoom_in, zoom_out, ptz_stop};

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

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let storage_path_buf = PathBuf::from(&storage_path);

    let (mjpeg_tx, _mjpeg_rx) = broadcast::channel::<Bytes>(32);

    let state = Arc::new(AppState {
        camera_rtsp_url: camera_rtsp_url.clone(),
        camera_onvif_url: camera_onvif_url.clone(),
        proxy_token: proxy_token.clone(),
        storage_path: storage_path_buf.clone(),
        pipeline: Arc::new(Mutex::new(None)),
        mjpeg_tx,
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
        .route("/api/storage", get(get_storage_info))
        .route("/api/storage/list", get(list_recordings))
        .route("/api/storage/delete/*path", get(delete_recording))
        .route("/api/recordings/stream/*path", get(stream_recording))
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
        .with_state(state);

    let addr: SocketAddr = listen_addr.parse()?;
    println!("🚀 API y Streamer escuchando en http://{}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}