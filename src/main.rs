use axum::body::Body;
use axum::{
    http::{header, HeaderValue, Method, Request},
    middleware::{from_fn, from_fn_with_state, Next},
    response::Response,
    routing::{get, post},
    Router,
};
use bytes::Bytes;
use dotenvy::dotenv;
use std::sync::atomic::AtomicU64;
use std::{
    env, fs,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};
use tokio::sync::{broadcast, watch, Mutex};
use tower_http::cors::CorsLayer;

use vigilante::{
    auth, camera, logs, ptz, storage, stream, AppState, AudioStatus, PipelineStatus,
    RecordingSnapshot, StorageStatus, SystemStatus,
};

use auth::flexible_auth_middleware;
use camera::start_camera_pipeline;
use logs::stream_journal_logs;
use ptz::{pan_left, pan_right, ptz_stop, tilt_down, tilt_up, zoom_in, zoom_out};
use storage::{
    get_recordings_by_date, get_storage_info, recordings_summary_ws, refresh_recording_snapshot,
    stream_recording,
};
use stream::{
    stream_audio_handler, stream_hls_handler, stream_hls_index, stream_mjpeg_handler,
    stream_webrtc_handler,
};
use vigilante::status::get_system_status;

// Dependencias de GStreamer
use gstreamer as gst;
use log::{info, warn};

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

async fn log_requests(req: Request<Body>, next: Next) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let started = Instant::now();
    info!("→ {} {}", method, uri);

    let response = next.run(req).await;
    let status = response.status();
    let elapsed = started.elapsed();
    info!(
        "← {} {} {} ({} ms)",
        method,
        uri,
        status.as_u16(),
        elapsed.as_millis()
    );

    response
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv().ok();

    simplelog::SimpleLogger::init(log::LevelFilter::Info, simplelog::Config::default()).unwrap();

    #[cfg(feature = "tokio-console")]
    {
        console_subscriber::init();
        info!("tokio-console instrumentation enabled");
    }

    gst::init()?;

    info!("GStreamer initialized successfully");

    let camera_rtsp_url =
        env::var("CAMERA_RTSP_URL").expect("CAMERA_RTSP_URL environment variable must be set");
    let camera_onvif_url =
        env::var("CAMERA_ONVIF_URL").expect("CAMERA_ONVIF_URL environment variable must be set");
    let proxy_token =
        env::var("PROXY_TOKEN").expect("PROXY_TOKEN environment variable must be set");
    if proxy_token.is_empty() {
        panic!("PROXY_TOKEN cannot be empty");
    }
    let listen_addr = env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let storage_path =
        env::var("STORAGE_PATH").expect("STORAGE_PATH environment variable must be set");
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
    let allowed_origins_str = env::var("ALLOWED_ORIGINS")
        .unwrap_or_else(|_| "http://localhost:3000,http://localhost:8080".to_string());
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
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::ACCEPT,
            header::USER_AGENT,
        ]);

    let storage_path_buf = PathBuf::from(&storage_path);
    if let Err(err) = fs::create_dir_all(&storage_path_buf) {
        warn!(
            "No se pudo asegurar la creación del directorio de almacenamiento {}: {}",
            storage_path_buf.display(),
            err
        );
    }
    let storage_root = match storage_path_buf.canonicalize() {
        Ok(path) => path,
        Err(err) => {
            warn!(
                "No se pudo canonicalizar STORAGE_PATH ({}), usando ruta original: {}",
                storage_path_buf.display(),
                err
            );
            storage_path_buf.clone()
        }
    };

    let (mjpeg_tx, _mjpeg_rx) = broadcast::channel::<Bytes>(32);
    let (mjpeg_low_tx, _mjpeg_low_rx) = broadcast::channel::<Bytes>(32);
    let (audio_mp3_tx, _audio_rx) = watch::channel::<Bytes>(Bytes::new());

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
        storage_root: storage_root.clone(),
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
        recording_snapshot: Arc::new(Mutex::new(RecordingSnapshot::default())),
    });

    // Construir snapshot inicial antes de atender solicitudes
    refresh_recording_snapshot(&state).await;

    // Refrescar caché de grabaciones de forma periódica
    {
        let state_clone = state.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                let _ = refresh_recording_snapshot(&state_clone).await;
            }
        });
    }

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
        .route(
            "/test",
            get(|| async { "OK - Vigilante API funcionando sin auth" }),
        )
        .route(
            "/api/health",
            get(|| async {
                info!("Health check received at {}", chrono::Utc::now());
                "OK"
            }),
        )
        .layer(from_fn(log_requests))
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
        .route("/api/storage", get(get_storage_info))
        .route("/api/recordings/by-date/:date", get(get_recordings_by_date))
        .route("/api/recordings/stream/*path", get(stream_recording))
        // Rutas para el control PTZ
        .route("/api/ptz/pan/left", post(pan_left))
        .route("/api/ptz/pan/right", post(pan_right))
        .route("/api/ptz/tilt/up", post(tilt_up))
        .route("/api/ptz/tilt/down", post(tilt_down))
        .route("/api/ptz/zoom/in", post(zoom_in))
        .route("/api/ptz/zoom/out", post(zoom_out))
        .route("/api/ptz/stop", post(ptz_stop))
        // Ruta de estado simple
        .route("/api/status", get(get_system_status))
        // Logging de requests
        .layer(from_fn(log_requests))
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
            info!("Shutdown handler armed; waiting for Ctrl+C or SIGTERM...");
            shutdown_signal().await;
            info!("Shutdown signal received, stopping server...");
        })
        .await?;

    info!("Server shut down gracefully");
    Ok(())
}
