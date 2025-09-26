use axum::{
    http::{HeaderValue, Method, header},
    routing::{get, post},
    Router,
};
use bytes::Bytes;
use dotenvy::dotenv;
use std::{env, net::SocketAddr, path::PathBuf, sync::{Arc, Mutex as StdMutex}};
use std::sync::atomic::AtomicU64;
use tokio::sync::{broadcast, watch, Mutex};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use vigilante::{auth, camera, logs, ptz, storage, stream, AppState, SystemStatus, PipelineStatus, AudioStatus, StorageStatus};

use auth::flexible_auth_middleware;
use axum::middleware::from_fn_with_state;
use camera::start_camera_pipeline;
use logs::stream_journal_logs;
use ptz::{pan_left, pan_right, ptz_stop, tilt_down, tilt_up, zoom_in, zoom_out};
use storage::{
    delete_recording, get_log_file, get_storage_info, list_recordings,
    recordings_list_ws, recordings_summary_ws, stream_recording,
    stream_recording_logs_sse, stream_recording_tail, get_recordings_by_date,
};
use stream::{
    stream_audio_handler, stream_hls_handler, stream_hls_index, stream_mjpeg_handler,
    stream_webrtc_handler,
};
use vigilante::status::{
    get_system_status,
    get_pipeline_status,
    get_audio_status,
    get_storage_status,
    generate_realtime_status,
    status_websocket,
};

// Dependencias de GStreamer
use gstreamer as gst;
use log::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv().ok();

    simplelog::SimpleLogger::init(log::LevelFilter::Info, simplelog::Config::default()).unwrap();

    gst::init()?;

    info!("GStreamer initialized successfully");

    let camera_rtsp_url = env::var("CAMERA_RTSP_URL")
        .expect("CAMERA_RTSP_URL environment variable must be set");
    let camera_onvif_url = env::var("CAMERA_ONVIF_URL")
        .expect("CAMERA_ONVIF_URL environment variable must be set");
    let proxy_token = env::var("PROXY_TOKEN")
        .expect("PROXY_TOKEN environment variable must be set");
    if proxy_token.is_empty() {
        panic!("PROXY_TOKEN cannot be empty");
    }
    let listen_addr = env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let storage_path = env::var("STORAGE_PATH")
        .expect("STORAGE_PATH environment variable must be set");
    let enable_hls = env::var("ENABLE_HLS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    // Detección manual de movimiento (activada por defecto)
    let enable_manual_motion_detection = env::var("ENABLE_MANUAL_MOTION_DETECTION")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(true);
    // Soporte de compatibilidad: STREAM_MJPEG_TOKEN_IN_QUERY también habilita el modo de token en query
    let allow_query_token_streams = env::var("STREAM_TOKEN_IN_QUERY")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
        || env::var("STREAM_MJPEG_TOKEN_IN_QUERY")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

    // Configuración de CORS: orígenes permitidos desde variable de entorno
    let allowed_origins_str = env::var("ALLOWED_ORIGINS").unwrap_or_else(|_| "http://localhost:3000,http://localhost:8080".to_string());
    let mut allowed_origins: Vec<HeaderValue> = allowed_origins_str
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    if allowed_origins.is_empty() {
        eprintln!("Warning: No valid origins in ALLOWED_ORIGINS, defaulting to localhost");
        allowed_origins.push(HeaderValue::from_static("http://localhost:3000"));
        allowed_origins.push(HeaderValue::from_static("http://localhost:8080"));
    }

    let cors = CorsLayer::new()
        .allow_origin(allowed_origins)
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE, header::ACCEPT, header::USER_AGENT]);

    let storage_path_buf = PathBuf::from(&storage_path);

    let (mjpeg_tx, _mjpeg_rx) = broadcast::channel::<Bytes>(32);
    let (mjpeg_low_tx, _mjpeg_low_rx) = broadcast::channel::<Bytes>(32);
    let (audio_mp3_tx, _audio_rx) = watch::channel::<Bytes>(Bytes::new());
    let (status_tx, _status_rx) = broadcast::channel::<serde_json::Value>(16);

    // Inicializar estado del sistema
    let initial_status = SystemStatus {
        pipeline_status: PipelineStatus {
            is_running: false,
            start_time: None,
            error_count: 0,
            last_error: None,
        },
        audio_status: AudioStatus {
            available: false,
            bytes_sent: 0,
            packets_sent: 0,
            last_activity: None,
        },
        storage_status: StorageStatus {
            total_space_bytes: 0,
            used_space_bytes: 0,
            free_space_bytes: 0,
            recording_count: 0,
            last_recording: None,
        },
        uptime_seconds: 0,
        last_updated: chrono::Utc::now(),
    };

    let state = Arc::new(AppState {
        camera_rtsp_url: camera_rtsp_url.clone(),
        camera_onvif_url: camera_onvif_url.clone(),
        proxy_token: proxy_token.clone(),
        storage_path: storage_path_buf.clone(),
        pipeline: Arc::new(Mutex::new(None)),
        mjpeg_tx,
        mjpeg_low_tx,
        audio_mp3_tx,
        audio_available: Arc::new(Mutex::new(false)),
        system_status: Arc::new(Mutex::new(initial_status)),
        audio_bytes_sent: AtomicU64::new(0),
        audio_packets_sent: AtomicU64::new(0),
        enable_hls,
        enable_manual_motion_detection,
        allow_query_token_streams,
        log_writer: Arc::new(StdMutex::new(None)),
        bypass_base_domain: env::var("BYPASS_DOMAIN").ok().map(|d| d.to_lowercase()),
        bypass_domain_secret: env::var("BYPASS_DOMAIN_SECRET").ok(),
        start_time: std::time::SystemTime::now(),
        status_tx,
    });

    // Iniciar background task para status en tiempo real
    {
        let state_clone = state.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3));
            loop {
                interval.tick().await;
                info!("Background status task running at {}", chrono::Utc::now());
                let status_update = generate_realtime_status(&state_clone).await;
                let _ = state_clone.status_tx.send(status_update);
            }
        });
    }

    info!("Real-time status background task started");

    // Iniciar tarea de monitoreo de salud del backend
    {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
            loop {
                interval.tick().await;
                info!("Backend health check: alive at {}", chrono::Utc::now());
            }
        });
    }

    info!("Backend health monitoring started");

    // Iniciar el pipeline de la cámara para la grabación 24/7 y detección de eventos
    tokio::spawn(start_camera_pipeline(camera_rtsp_url, state.clone()));

    info!("Camera pipeline started");

    // HLS ahora se genera dentro del pipeline principal en camera.rs mediante un branch del tee

    // Router PÚBLICO sin autenticación
    let public_routes = Router::new()
        .route("/test", get(|| async { "OK - Vigilante API funcionando sin auth" }))
        .route("/api/health", get(|| async {
            info!("Health check received at {}", chrono::Utc::now());
            "OK"
        }))
        .layer(TraceLayer::new_for_http())
        .layer(cors.clone())
        .with_state(state.clone());

    // Router PROTEGIDO con autenticación flexible (header o query token)
    let app = Router::new()
        .route("/hls", get(stream_hls_index))
        .route("/hls/*path", get(stream_hls_handler))
        .route("/webrtc/*path", get(stream_webrtc_handler))
        .route("/api/live/mjpeg", get(stream_mjpeg_handler))
        .route("/api/live/audio", get(stream_audio_handler))
        .route("/api/logs/stream", get(stream_journal_logs))
        // .route("/api/storage/stream", get(storage_stream_sse)) // Función eliminada en actualización
        .route("/api/recordings/summary", get(recordings_summary_ws))
        .route("/api/recordings/list", get(recordings_list_ws))
        .route(
            "/api/recordings/stream/tail/*path",
            get(stream_recording_tail),
        )
        .route("/api/storage", get(get_storage_info))
        .route("/api/storage/list", get(list_recordings))
        .route("/api/recordings/by-date/:date", get(get_recordings_by_date))
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
        // Nuevas rutas para el estado del sistema
        .route("/api/status", get(get_system_status))
        .route("/api/status/pipeline", get(get_pipeline_status))
        .route("/api/status/audio", get(get_audio_status))
        .route("/api/status/storage", get(get_storage_status))
        .route("/api/status/ws", get(status_websocket))
        // Logging de requests
        .layer(TraceLayer::new_for_http())
        // CORS middleware debe ir ANTES de autenticación para manejar preflight OPTIONS
        .layer(cors)
        // Middleware flexible: valida token por header O por query
        .layer(from_fn_with_state(state.clone(), flexible_auth_middleware))
        .with_state(state);

    // Combinar routers: público + protegido
    let app = public_routes.merge(app);

    let addr: SocketAddr = listen_addr.parse()?;
    info!("🚀 API y Streamer escuchando en http://{}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            info!("Shutdown signal received, stopping server...");
            tokio::signal::ctrl_c().await.expect("failed to listen for shutdown signal");
            info!("Shutdown signal received, stopping server...");
        })
        .await?;

    info!("Server shut down gracefully");
    Ok(())
}
