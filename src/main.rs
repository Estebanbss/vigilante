use axum::{
    routing::{get, post},
    Router,
};
use bytes::Bytes;
use dotenvy::dotenv;
use std::{env, net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;
use tower_http::cors::{Any, CorsLayer};

use vigilante::{auth, camera, events, logs, ptz, storage, stream, AppState};

use auth::flexible_auth_middleware;
use axum::middleware::from_fn_with_state;
use camera::start_camera_pipeline;
use events::motion_event_callback;
use logs::stream_journal_logs;
use ptz::{pan_left, pan_right, ptz_stop, tilt_down, tilt_up, zoom_in, zoom_out};
use storage::{
    delete_recording, get_log_file, get_storage_info, list_recordings, recordings_stream_sse,
    start_cleanup_task, storage_stream_sse, stream_recording, stream_recording_logs_sse,
    stream_recording_tail,
};
use stream::{
    stream_audio_handler, stream_hls_handler, stream_hls_index, stream_mjpeg_handler,
    stream_webrtc_handler,
};

// Dependencias de GStreamer
use gstreamer as gst;
use tokio::sync::broadcast;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv().ok();

    gst::init()?;

    let camera_rtsp_url = env::var("CAMERA_RTSP_URL")?;
    let camera_onvif_url = env::var("CAMERA_ONVIF_URL")?;
    let proxy_token = env::var("PROXY_TOKEN")?;
    let listen_addr = env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let storage_path = env::var("STORAGE_PATH")?;
    let enable_hls = env::var("ENABLE_HLS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    // Soporte de compatibilidad: STREAM_MJPEG_TOKEN_IN_QUERY también habilita el modo de token en query
    let allow_query_token_streams = env::var("STREAM_TOKEN_IN_QUERY")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
        || env::var("STREAM_MJPEG_TOKEN_IN_QUERY")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let storage_path_buf = PathBuf::from(&storage_path);

    let (mjpeg_tx, _mjpeg_rx) = broadcast::channel::<Bytes>(32);
    let (mjpeg_low_tx, _mjpeg_low_rx) = broadcast::channel::<Bytes>(32);
    let (audio_mp3_tx, _audio_rx) = broadcast::channel::<Bytes>(2);

    let state = Arc::new(AppState {
        camera_rtsp_url: camera_rtsp_url.clone(),
        camera_onvif_url: camera_onvif_url.clone(),
        proxy_token: proxy_token.clone(),
        storage_path: storage_path_buf.clone(),
        pipeline: Arc::new(Mutex::new(None)),
        mjpeg_tx,
        mjpeg_low_tx,
        audio_mp3_tx,
        enable_hls,
        allow_query_token_streams,
        log_writer: Arc::new(Mutex::new(None)),
    });

    // Iniciar la tarea de limpieza de almacenamiento en segundo plano
    tokio::spawn(start_cleanup_task(storage_path_buf));

    // Iniciar el pipeline de la cámara para la grabación 24/7 y detección de eventos
    tokio::spawn(start_camera_pipeline(camera_rtsp_url, state.clone()));

    // Iniciar servicio de eventos ONVIF para detección nativa de movimiento
    if let Err(e) = events::start_onvif_events_service(state.clone()).await {
        println!("⚠️  Error iniciando eventos ONVIF: {}", e);
        println!("📝 Continuando con detección manual de movimiento");
    }

    // HLS ahora se genera dentro del pipeline principal en camera.rs mediante un branch del tee

    // Router único con autenticación flexible (header o query token)
    let app = Router::new()
        .route("/hls", get(stream_hls_index))
        .route("/hls/*path", get(stream_hls_handler))
        .route("/webrtc/*path", get(stream_webrtc_handler))
        .route("/api/live/mjpeg", get(stream_mjpeg_handler))
        .route("/api/live/audio", get(stream_audio_handler))
        .route("/api/logs/stream", get(stream_journal_logs))
        .route("/api/storage/stream", get(storage_stream_sse))
        .route("/api/recordings/stream", get(recordings_stream_sse))
        .route(
            "/api/recordings/stream/tail/*path",
            get(stream_recording_tail),
        )
        .route("/api/storage", get(get_storage_info))
        .route("/api/storage/list", get(list_recordings))
        .route("/api/storage/delete/*path", get(delete_recording))
        .route("/api/recordings/stream/*path", get(stream_recording))
        .route("/api/recordings/log/:date", get(get_log_file))
        .route(
            "/api/recordings/log/:date/stream",
            get(stream_recording_logs_sse),
        )
        // Rutas para el control PTZ
        .route("/api/ptz/pan/left", post(pan_left))
        .route("/api/ptz/pan/right", post(pan_right))
        .route("/api/ptz/tilt/up", post(tilt_up))
        .route("/api/ptz/tilt/down", post(tilt_down))
        .route("/api/ptz/zoom/in", post(zoom_in))
        .route("/api/ptz/zoom/out", post(zoom_out))
        .route("/api/ptz/stop", post(ptz_stop))
        // Ruta para eventos de movimiento ONVIF (sin autenticación)
        .route("/api/events/motion", post(motion_event_callback))
        .layer(cors)
        // Middleware flexible: valida token por header O por query
        .layer(from_fn_with_state(state.clone(), flexible_auth_middleware))
        .with_state(state);

    let addr: SocketAddr = listen_addr.parse()?;
    println!("🚀 API y Streamer escuchando en http://{}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
