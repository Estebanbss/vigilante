use axum::body::{to_bytes, Body, Bytes};
use axum::{
    extract::Request,
    http::{header, HeaderValue, Method, StatusCode},
    middleware::{from_fn, from_fn_with_state, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Router,
};
use dotenvy::dotenv;
use std::{
    env, fs,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};
use tokio::sync::{broadcast, Mutex};
use tower_http::cors::CorsLayer;

// Custom logger that sends JSON logs to broadcast channel and stdout
struct BroadcastLogger {
    tx: broadcast::Sender<String>,
}

impl log::Log for BroadcastLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            let timestamp = chrono::Utc::now().format("%H:%M:%S").to_string();
            let message = record.args().to_string();

            // Try to parse as structured log, otherwise use generic format
            let log_line = if message.starts_with("→ ") || message.starts_with("← ") {
                // Structured request/response log
                self.parse_request_response_log(&timestamp, &message)
            } else {
                // Generic log
                format!("[{}] {}", timestamp, message)
            };

            // Print to stdout
            println!("{}", log_line);
            // Send to channel
            let _ = self.tx.send(log_line);
        }
    }

    fn flush(&self) {}
}

impl BroadcastLogger {
    fn parse_request_response_log(&self, timestamp: &str, message: &str) -> String {
        if let Some(stripped) = message.strip_prefix("→ ") {
            // Request: "→ GET /api/live/mjpeg?token=abc"
            let parts: Vec<&str> = stripped.split_whitespace().collect();
            if parts.len() >= 2 {
                let method = parts[0];
                let full_uri = parts[1..].join(" ");
                // Parse URI and query
                if let Some((path, _query)) = full_uri.split_once('?') {
                    return format!("[{}] → {} {}", timestamp, method, path);
                } else {
                    return format!("[{}] → {} {}", timestamp, method, full_uri);
                }
            }
        } else if let Some(stripped) = message.strip_prefix("← ") {
            // Response: "← GET /api/live/mjpeg 200 (0 ms) [size: 1234] [body: null]"
            // Split by '[' to separate the main part from metadata
            let bracket_parts: Vec<&str> = stripped.split(" [").collect();
            if bracket_parts.len() >= 2 {
                let main_part = bracket_parts[0];
                let metadata_str = format!("[{}", bracket_parts[1..].join(" ["));

                let main_parts: Vec<&str> = main_part.split_whitespace().collect();
                if main_parts.len() >= 4 {
                    let method = main_parts[0];
                    let uri = main_parts[1];
                    let status_code = main_parts[2].parse::<u16>().unwrap_or(0);
                    let duration_part = main_parts[3];
                    let duration_ms = duration_part
                        .trim_start_matches('(')
                        .trim_end_matches(')')
                        .parse::<u64>()
                        .unwrap_or(0);

                    let status_text = match status_code {
                        200 => "éxito",
                        404 => "no encontrado",
                        401 => "no autorizado",
                        500 => "error interno",
                        _ => "desconocido",
                    };

                    // Parse body from metadata
                    let mut response_body = String::new();
                    if let Some(body_start) = metadata_str.find("[body: ") {
                        if let Some(body_end) = metadata_str[body_start..].find("]") {
                            let body_str = &metadata_str[body_start + 7..body_start + body_end];
                            if !body_str.is_empty() && body_str != "null" {
                                response_body = body_str.to_string();
                            }
                        }
                    }

                    let body_display = if response_body.is_empty() {
                        String::new()
                    } else {
                        format!(" body: {}", response_body)
                    };

                    return format!(
                        "[{}] ← {} {} - {} ({}ms){}",
                        timestamp, method, uri, status_text, duration_ms, body_display
                    );
                }
            }
        }
        // Fallback
        format!("[{}] {}", timestamp, message)
    }
}

use vigilante::{
    auth, auth::AuthManager, camera, logs, metrics, ptz, state, storage, stream, AppState,
    NotificationManager, RecordingSnapshot,
};

use auth::flexible_auth_middleware;
use camera::start_camera_pipeline;
use logs::{get_log_entries_handler, stream_logs_sse};
use ptz::{pan_left, pan_right, ptz_stop, tilt_down, tilt_up, zoom_in, zoom_out};
use storage::{
    delete_recording, recordings_by_day, recordings_summary_ws, refresh_recording_snapshot,
    storage_info, storage_overview, storage_stream_sse, stream_live_recording, system_storage_info,
};
 use stream::{stream_audio_handler, stream_combined_av, webrtc_offer, webrtc_answer, webrtc_close};
use vigilante::status::get_system_status;

// Dependencias de GStreamer
use gstreamer as gst;
use log::{error, info, warn};

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

    // Capturar el body de la respuesta para logging si es pequeño
    let (parts, body) = response.into_parts();
    let content_type = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let is_streaming = content_type.contains("multipart/x-mixed-replace")
        || content_type.contains("text/event-stream")
        || content_type.starts_with("audio/")
        || content_type.starts_with("video/");

    if is_streaming {
        info!(
            "← {} {} {} ({} ms) [size: stream] [body: <stream>]",
            method,
            uri,
            status.as_u16(),
            elapsed.as_millis()
        );

        metrics::depends::collectors::MetricsCollector::increment_requests();
        metrics::depends::collectors::MetricsCollector::record_duration(elapsed.as_secs_f64());
        if status.is_client_error() || status.is_server_error() {
            metrics::depends::collectors::MetricsCollector::increment_errors();
        }

        return Response::from_parts(parts, body);
    }

    let body_bytes = match to_bytes(body, 2048).await {
        Ok(bytes) => bytes,
        Err(_) => Bytes::new(),
    };
    let response_size = body_bytes.len() as u64;

    // Incluir body en logs si es pequeño y parece JSON
    let response_body = if body_bytes.len() < 2048 && body_bytes.starts_with(b"{") {
        Some(String::from_utf8_lossy(&body_bytes).to_string())
    } else {
        None
    };

    info!(
        "← {} {} {} ({} ms) [size: {}] [body: {}]",
        method,
        uri,
        status.as_u16(),
        elapsed.as_millis(),
        response_size,
        response_body.as_deref().unwrap_or("null")
    );

    // Incrementar métricas
    metrics::depends::collectors::MetricsCollector::increment_requests();
    metrics::depends::collectors::MetricsCollector::record_duration(elapsed.as_secs_f64());
    if status.is_client_error() || status.is_server_error() {
        metrics::depends::collectors::MetricsCollector::increment_errors();
    }

    // Reconstruir la response
    Response::from_parts(parts, Body::from(body_bytes))
}

async fn metrics_handler() -> impl IntoResponse {
    match metrics::gather_metrics() {
        Ok(metrics) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
            metrics,
        ),
        Err(err) => {
            eprintln!("❌ Error gathering metrics: {}", err);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/plain")],
                "Error gathering metrics".to_string(),
            )
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv().ok();

    // Initialize log channel first
    let (log_tx, _log_rx) = broadcast::channel::<String>(100);

    // Initialize custom logger that broadcasts logs
    let logger = BroadcastLogger { tx: log_tx.clone() };
    log::set_boxed_logger(Box::new(logger)).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    #[cfg(feature = "tokio-console")]
    {
        console_subscriber::init();
        info!("tokio-console instrumentation enabled");
    }

    gst::init()?;

    info!("GStreamer initialized successfully");

    // Initialize metrics
    metrics::depends::collectors::MetricsCollector::init();

    // Cargar variables de entorno críticas con manejo de errores adecuado
    let camera_rtsp_url = env::var("CAMERA_RTSP_URL")
        .map_err(|_| "CAMERA_RTSP_URL environment variable must be set")?;
    let camera_onvif_url = env::var("CAMERA_ONVIF_URL")
        .map_err(|_| "CAMERA_ONVIF_URL environment variable must be set")?;
    let proxy_token =
        env::var("PROXY_TOKEN").map_err(|_| "PROXY_TOKEN environment variable must be set")?;
    if proxy_token.is_empty() {
        return Err("PROXY_TOKEN cannot be empty".into());
    }
    let listen_addr = env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let storage_path =
        env::var("STORAGE_PATH").map_err(|_| "STORAGE_PATH environment variable must be set")?;
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
        .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
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

    // Initialize SQLite database for persistent metadata cache
    let db_path = storage_root.join("recordings.db");
    let db_conn = match storage::init_recordings_db(&db_path) {
        Ok(conn) => Arc::new(StdMutex::new(conn)),
        Err(e) => {
            warn!("Failed to initialize recordings database: {}", e);
            // Fallback to in-memory only, but we can continue
            Arc::new(StdMutex::new(
                rusqlite::Connection::open_in_memory().unwrap(),
            ))
        }
    };

    let (audio_mp3_tx, _audio_rx) = broadcast::channel::<Bytes>(100);
    let (log_tx, _log_rx) = broadcast::channel::<String>(100);
    let notifications = Arc::new(NotificationManager::new());

    // Initialize auth manager
    let auth_manager = AuthManager::new(auth::AuthConfig {
        bearer_token: Some(proxy_token.clone()),
    })?;

    // Create separate state structs
    let camera_config = state::CameraConfig {
        rtsp_url: camera_rtsp_url.clone(),
        onvif_url: camera_onvif_url.clone(),
        enable_manual_motion_detection,
    };

    let storage_config = state::StorageConfig {
        path: storage_path_buf.clone(),
        root: storage_root.clone(),
        db_conn: db_conn.clone(),
    };

    let streaming_state = Arc::new(state::StreamingState {
        audio_mp3_tx,
        audio_available: Arc::new(StdMutex::new(false)),
        last_audio_timestamp: Arc::new(StdMutex::new(None)),
        mp4_init_segments: Arc::new(StdMutex::new(Vec::new())),
        mp4_init_complete: Arc::new(StdMutex::new(false)),
    mp4_init_scan_tail: Arc::new(StdMutex::new(Vec::new())),
    mp4_init_warned_before_moov: Arc::new(StdMutex::new(false)),
    live_latency_snapshot: Arc::new(StdMutex::new(state::LatencySnapshot::default())),
    });

    let logging_state = Arc::new(state::LoggingState {
        log_tx,
        log_writer: Arc::new(StdMutex::new(None)),
    });

    let system_state = Arc::new(state::SystemState {
        start_time: std::time::SystemTime::now(),
        recording_snapshot: Arc::new(Mutex::new(RecordingSnapshot::default())),
        notifications,
    });

    let auth_state = state::AuthState {
        manager: auth_manager,
        proxy_token: proxy_token.clone(),
        allow_query_token_streams,
        bypass_base_domain: env::var("BYPASS_DOMAIN").ok().map(|d| d.to_lowercase()),
        bypass_domain_secret: env::var("BYPASS_DOMAIN_SECRET").ok(),
    };

    let gstreamer_state = Arc::new(state::GStreamerState {
        pipeline_running: Arc::new(StdMutex::new(false)),
    });

    let state = Arc::new(AppState {
        camera: camera_config,
        storage: storage_config,
        streaming: streaming_state.clone(),
        logging: logging_state,
        system: system_state,
        auth: auth_state,
        gstreamer: gstreamer_state,
        camera_pipeline: Arc::new(StdMutex::new(None)),
        stream: Arc::new(stream::StreamManager::new(streaming_state)?),
    });

    // Construir snapshot inicial antes de atender solicitudes
    if let Err(e) = refresh_recording_snapshot(&state).await {
        warn!("Failed to build initial recording snapshot: {}", e);
        // No es crítico, continuar con snapshot vacío
    }

    // Inicializar streaming WebRTC
    if let Err(e) = state.stream.start_streaming(&camera_rtsp_url).await {
        error!("Failed to initialize WebRTC streaming: {}", e);
        // Continuar, WebRTC fallará pero el resto funcionará
    }

    // Refrescar caché de grabaciones de forma periódica
    {
        let state_clone = state.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                if let Err(e) = refresh_recording_snapshot(&state_clone).await {
                    warn!("Failed to refresh recording snapshot: {}", e);
                    // Continuar el loop, no es crítico
                }
            }
        });
    }

    // Actualizar métricas de uptime periódicamente
    {
        let start_time = state.system.start_time;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                if let Ok(elapsed) = start_time.elapsed() {
                    metrics::depends::collectors::MetricsCollector::set_uptime(
                        elapsed.as_secs_f64(),
                    );
                }
            }
        });
    }

    // Iniciar el pipeline de la cámara para la grabación 24/7 y detección de eventos
    tokio::spawn(start_camera_pipeline(camera_rtsp_url, state.clone()));

    info!("Camera pipeline started");

    // Router PROTEGIDO con autenticación flexible (header o query token)
    let app = Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/api/webrtc/offer", post(webrtc_offer))
        .route("/api/webrtc/answer/:client_id", post(webrtc_answer))
        .route("/api/webrtc/close/:client_id", post(webrtc_close))
        .route("/api/stream/audio", get(stream_audio_handler))
        .route("/api/stream/av", get(stream_combined_av))
        .route("/api/logs/stream", get(stream_logs_sse))
        .route("/api/logs/entries/:date", get(get_log_entries_handler))
        .route("/api/recordings/summary", get(recordings_summary_ws))
        .route("/api/recordings/day/:date", get(recordings_by_day))
        .route("/api/storage", get(storage_overview))
        .route("/api/storage/info", get(storage_info))
        .route("/api/system/storage", get(system_storage_info))
        .route("/api/storage/stream", get(storage_stream_sse))
        .route("/api/recordings/stream/*path", get(stream_live_recording))
        .route("/api/recordings/delete/*path", delete(delete_recording))
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
        // Middleware flexible: valida token por header O por query
        .layer(from_fn_with_state(state.clone(), flexible_auth_middleware))
        // CORS middleware permite preflight antes de llegar al handler
        .layer(cors)
        // Logging de requests (debe envolver toda la pila para registrar cualquier respuesta)
        .layer(from_fn(log_requests))
        .with_state(state);

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
