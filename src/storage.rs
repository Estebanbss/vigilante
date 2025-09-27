use crate::{auth::RequireAuth, AppState, DaySummary, RecordingSnapshot};
use axum::{
    body::Body,
    extract::{
        ws::{WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use bytes::Bytes;
use chrono::{NaiveDate, Utc};
use fs2;
use log::info;
use serde::Serialize;
use std::{
    fs,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

const MAX_SUMMARY_DAYS: usize = 120;

fn canonicalize_within_root(
    state: &Arc<AppState>,
    candidate: PathBuf,
) -> Result<PathBuf, StatusCode> {
    let root = &state.storage_root;
    let full = candidate
        .canonicalize()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    if !full.starts_with(root) {
        Err(StatusCode::FORBIDDEN)
    } else {
        Ok(full)
    }
}

fn resolve_storage_path(
    state: &Arc<AppState>,
    relative: impl AsRef<FsPath>,
) -> Result<PathBuf, StatusCode> {
    let rel_path = relative.as_ref();
    if rel_path.is_absolute() {
        return Err(StatusCode::FORBIDDEN);
    }
    let candidate = state.storage_root.join(rel_path);
    canonicalize_within_root(state, candidate)
}

fn iso_to_day_folder(date: &str) -> Option<String> {
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .ok()
        .map(|d| d.format("%d-%m-%y").to_string())
}

fn resolve_log_file_path(state: &Arc<AppState>, date: &str) -> Result<PathBuf, StatusCode> {
    let file_name = format!("{}-log.txt", date);

    if let Some(day_folder) = iso_to_day_folder(date) {
        let candidate = state.storage_root.join(&day_folder).join(&file_name);
        if candidate.exists() {
            return canonicalize_within_root(state, candidate);
        }
    }

    let fallback = state.storage_root.join(&file_name);
    if fallback.exists() {
        return canonicalize_within_root(state, fallback);
    }

    Err(StatusCode::NOT_FOUND)
}

async fn open_recording_file(
    state: &Arc<AppState>,
    relative: &str,
) -> Result<(PathBuf, File), StatusCode> {
    let full_path = resolve_storage_path(state, FsPath::new(relative))?;
    let file = File::open(&full_path)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    Ok((full_path, file))
}

fn compare_day_labels_desc(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |s: &str| NaiveDate::parse_from_str(s, "%d-%m-%y").ok();
    match (parse(a), parse(b)) {
        (Some(da), Some(db)) => db.cmp(&da),
        _ => b.cmp(a),
    }
}

fn list_recent_day_directories(storage_path: &FsPath) -> Vec<(String, PathBuf)> {
    let mut days: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(entries) = fs::read_dir(storage_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            days.push((name, path));
        }
    }

    days.sort_by(|(a, _), (b, _)| compare_day_labels_desc(a, b));
    if days.len() > MAX_SUMMARY_DAYS {
        days.truncate(MAX_SUMMARY_DAYS);
    }
    days
}

struct DayScanResult {
    count: usize,
    latest: Option<(chrono::DateTime<chrono::Utc>, String)>,
}

fn summarize_day_with_ls(day_path: &FsPath) -> DayScanResult {
    let mut count = 0usize;
    let mut latest: Option<(std::time::SystemTime, PathBuf)> = None;

    if let Ok(entries) = fs::read_dir(day_path) {
        for entry in entries.flatten() {
            let path = entry.path();

            if !path.is_file() {
                continue;
            }

            if path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.ends_with("-log.txt"))
                .unwrap_or(false)
            {
                continue;
            }

            count += 1;

            if let Ok(meta) = entry.metadata() {
                if let Ok(modified) = meta.modified() {
                    match latest {
                        Some((current, _)) if modified <= current => {}
                        _ => latest = Some((modified, path.clone())),
                    }
                }
            }
        }
    }

    let latest = latest.map(|(ts, path)| {
        (
            chrono::DateTime::<chrono::Utc>::from(ts),
            path.to_string_lossy().to_string(),
        )
    });

    DayScanResult { count, latest }
}

fn scan_recordings_sync(storage_path: &FsPath) -> RecordingSnapshot {
    let mut snapshot = RecordingSnapshot::default();
    let mut summaries: Vec<DaySummary> = Vec::new();
    let mut total_count: usize = 0;
    let mut latest_recording: Option<(chrono::DateTime<chrono::Utc>, String)> = None;
    let started = std::time::Instant::now();

    let day_dirs = list_recent_day_directories(storage_path);
    let scanned_days = day_dirs.len();

    for (day_name, day_path) in day_dirs {
        let day_result = summarize_day_with_ls(&day_path);

        total_count += day_result.count;

        if let Some((ts, path)) = day_result.latest.as_ref() {
            if latest_recording
                .as_ref()
                .map(|(current_ts, _)| ts > current_ts)
                .unwrap_or(true)
            {
                latest_recording = Some((ts.clone(), path.clone()));
            }
        }

        summaries.push(DaySummary {
            day: day_name,
            recording_count: day_result.count,
        });
    }

    snapshot.total_count = total_count;
    snapshot.last_recording = latest_recording.as_ref().map(|(_, path)| path.clone());
    snapshot.latest_timestamp = latest_recording.map(|(ts, _)| ts);
    snapshot.day_summaries = summaries;
    snapshot.last_scan = Some(Utc::now());

    log::debug!(
        "scan_recordings_sync: {} files across {} recent days in {:.2?}",
        snapshot.total_count,
        scanned_days,
        started.elapsed()
    );

    snapshot
}

pub async fn rebuild_recording_snapshot(storage_path: PathBuf) -> RecordingSnapshot {
    tokio::task::spawn_blocking(move || scan_recordings_sync(&storage_path))
        .await
        .unwrap_or_default()
}

pub async fn refresh_recording_snapshot(state: &Arc<AppState>) -> RecordingSnapshot {
    let start = std::time::Instant::now();
    let snapshot = rebuild_recording_snapshot(state.storage_root.clone()).await;
    let elapsed = start.elapsed();

    {
        let mut cache = state.recording_snapshot.lock().await;
        *cache = snapshot.clone();
    }

    {
        let mut system = state.system_status.lock().await;
        system.storage_status.recording_count = snapshot.total_count;
        system.storage_status.last_recording = snapshot.last_recording.clone();
    }

    info!(
        "📊 Snapshot de grabaciones actualizado en {:.2?} ({} archivos)",
        elapsed, snapshot.total_count
    );

    snapshot
}

// Estructura para la respuesta estándar de la API
#[derive(Serialize)]
pub struct ApiResponse {
    pub status: String,
    pub message: String,
}

#[derive(Serialize)]
pub struct StorageInfo {
    pub total_space_bytes: u64,
    pub used_space_bytes: u64,
    pub storage_path: String,
    pub storage_name: String,
}

pub async fn get_storage_info(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    println!("🎯 Handler: .00i0n0fo");

    match fs2::statvfs(&state.storage_root) {
        Ok(stats) => {
            let total_space = stats.total_space();
            let free_space = stats.free_space();
            let used_space = total_space - free_space;

            let info = StorageInfo {
                total_space_bytes: total_space,
                used_space_bytes: used_space,
                storage_path: state.storage_path.to_str().unwrap_or_default().to_string(),
                // Puedes personalizar el nombre basado en la ruta o un archivo de configuración
                storage_name: "Disco de Grabaciones".to_string(),
            };
            (StatusCode::OK, Json(info)).into_response()
        }
        Err(e) => {
            eprintln!("❌ Error al obtener info de almacenamiento: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse {
                    status: "error".to_string(),
                    message: "Failed to get storage info".to_string(),
                }),
            )
                .into_response()
        }
    }
}

// Función auxiliar para obtener la duración de un archivo MP4 usando ffprobe
// Deshabilitada por rendimiento - usar None por ahora
// fn get_mp4_duration(file_path: &PathBuf) -> Option<f64> {
//     use std::process::Command;

//     // Intentar usar ffprobe para obtener la duración
//     let output = match Command::new("ffprobe")
//         .args(&[
//             "-v", "quiet",
//             "-print_format", "json",
//             "-show_format",
//             file_path.to_str()?,
//         ])
//         .output()
//     {
//         Ok(out) => out,
//         Err(_) => {
//             // ffprobe no está disponible, devolver None silenciosamente
//             return None;
//         }
//     };

//     if !output.status.success() {
//         return None;
//     }

//     let json_str = String::from_utf8(output.stdout).ok()?;
//     let json: serde_json::Value = serde_json::from_str(&json_str).ok()?;

//     if let Some(format_obj) = json.get("format") {
//         if let Some(duration_str) = format_obj.get("duration") {
//             if let Some(duration_str) = duration_str.as_str() {
//                 return duration_str.parse::<f64>().ok();
//             }
//         }
//     }

//     None
// }

pub async fn get_recordings_by_date(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(date): Path<String>,
) -> impl IntoResponse {
    // Validar formato de fecha (YYYY-MM-DD)
    if date.len() != 10 || !date.chars().all(|c| c.is_numeric() || c == '-') {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse {
                status: "error".to_string(),
                message: "Formato de fecha inválido. Use YYYY-MM-DD".to_string(),
            }),
        )
            .into_response();
    }

    let snapshot = { state.recording_snapshot.lock().await.clone() };
    let friendly_day = iso_to_day_folder(&date).unwrap_or_else(|| date.clone());

    let count = snapshot
        .day_summaries
        .iter()
        .find(|summary| summary.day == friendly_day)
        .map(|summary| summary.recording_count)
        .unwrap_or(0);

    let response = serde_json::json!({
        "date": date,
        "day": friendly_day,
        "length": count,
        "total": snapshot.total_count,
        "last_scan": snapshot.last_scan.map(|ts| ts.to_rfc3339()),
    });

    (StatusCode::OK, Json(response)).into_response()
}

pub async fn delete_recording(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(file_path): Path<String>,
) -> impl IntoResponse {
    let full_path = match resolve_storage_path(&state, FsPath::new(&file_path)) {
        Ok(path) => path,
        Err(StatusCode::BAD_REQUEST) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse {
                    status: "error".into(),
                    message: "Invalid path".into(),
                }),
            )
                .into_response()
        }
        Err(StatusCode::FORBIDDEN) => {
            return (
                StatusCode::FORBIDDEN,
                Json(ApiResponse {
                    status: "error".into(),
                    message: "Path not allowed".into(),
                }),
            )
                .into_response()
        }
        Err(status) => {
            return (
                status,
                Json(ApiResponse {
                    status: "error".into(),
                    message: "Storage misconfigured".into(),
                }),
            )
                .into_response()
        }
    };

    if full_path.exists() && full_path.is_file() {
        match fs::remove_file(&full_path) {
            Ok(_) => {
                let state_clone = state.clone();
                tokio::spawn(async move {
                    let _ = refresh_recording_snapshot(&state_clone).await;
                });
                (
                    StatusCode::OK,
                    Json(ApiResponse {
                        status: "success".to_string(),
                        message: "File deleted successfully".to_string(),
                    }),
                )
                    .into_response()
            }
            Err(e) => {
                eprintln!("❌ Error al eliminar el archivo: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiResponse {
                        status: "error".to_string(),
                        message: "Failed to delete file".to_string(),
                    }),
                )
                    .into_response()
            }
        }
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(ApiResponse {
                status: "error".to_string(),
                message: "File not found".to_string(),
            }),
        )
            .into_response()
    }
}

pub async fn stream_recording(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(file_path): Path<String>,
) -> Result<Response, StatusCode> {
    // Autenticación ya validada por middleware global

    let (_full_path, file) = open_recording_file(&state, &file_path).await?;

    // Tamaño del archivo para rangos
    let file_size = file
        .metadata()
        .await
        .map(|meta| meta.len())
        .map_err(|_| StatusCode::NOT_FOUND)?;

    // Soporte para Range: bytes=START-END
    let range_hdr = headers
        .get(axum::http::header::RANGE)
        .and_then(|v| v.to_str().ok());
    let (status, start, end) = if let Some(r) = range_hdr {
        // Parseo simple del primer rango
        if !r.starts_with("bytes=") {
            return Err(StatusCode::RANGE_NOT_SATISFIABLE);
        }
        let spec = &r[6..];
        let mut parts = spec.splitn(2, '-');
        let a = parts.next().unwrap_or("");
        let b = parts.next().unwrap_or("");
        let (start, end) = if a.is_empty() {
            // Sufijo: bytes=-N (últimos N bytes)
            let n: u64 = b.parse().unwrap_or(0);
            if n == 0 {
                (0, file_size.saturating_sub(1))
            } else {
                let s = file_size.saturating_sub(n);
                (s, file_size.saturating_sub(1))
            }
        } else {
            let s: u64 = a.parse().unwrap_or(0);
            let e: u64 = if b.is_empty() {
                file_size.saturating_sub(1)
            } else {
                b.parse().unwrap_or(0)
            };
            (s, e.min(file_size.saturating_sub(1)))
        };
        if start > end || start >= file_size {
            return Err(StatusCode::RANGE_NOT_SATISFIABLE);
        }
        (StatusCode::PARTIAL_CONTENT, start, end)
    } else {
        (StatusCode::OK, 0u64, file_size.saturating_sub(1))
    };

    // Abrir y posicionar
    let mut file = file;
    if start > 0 {
        if let Err(_) = file.seek(std::io::SeekFrom::Start(start)).await {
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }

    // Construir stream limitado al rango solicitado
    // Mayor buffer y uso de read() en lugar de read_exact() para evitar bloqueos en FS remotos
    let total_len = end.saturating_sub(start).saturating_add(1);
    let stream = async_stream::stream! {
        let mut remaining = total_len;
        // 512KB buffer para mejorar throughput en lectura secuencial
        let mut buf = vec![0u8; 512 * 1024];
        while remaining > 0 {
            let to_read = std::cmp::min(buf.len() as u64, remaining) as usize;
            match file.read(&mut buf[..to_read]).await {
                Ok(0) => {
                    // EOF alcanzado antes de completar el rango
                    break;
                }
                Ok(n) => {
                    remaining = remaining.saturating_sub(n as u64);
                    yield Ok::<Bytes, std::io::Error>(Bytes::copy_from_slice(&buf[..n]));
                }
                Err(e) => {
                    eprintln!("stream read err: {}", e);
                    break;
                }
            }
        }
    };

    let mut resp = Response::new(Body::from_stream(stream));
    let h = resp.headers_mut();
    h.insert(
        axum::http::header::CONTENT_TYPE,
        "video/mp4".parse().unwrap(),
    );
    h.insert(
        axum::http::header::CONTENT_DISPOSITION,
        "inline".parse().unwrap(),
    );
    h.insert(axum::http::header::ACCEPT_RANGES, "bytes".parse().unwrap());
    h.insert(
        axum::http::header::CONTENT_LENGTH,
        total_len.to_string().parse().unwrap(),
    );
    h.insert(axum::http::header::PRAGMA, "no-cache".parse().unwrap());
    h.insert(axum::http::header::EXPIRES, "0".parse().unwrap());
    // Evitar buffering por proxies/reverse proxies, y permitir envío inmediato
    h.insert("X-Accel-Buffering", "no".parse().unwrap());
    h.insert(
        axum::http::header::CACHE_CONTROL,
        "no-cache, no-store, must-revalidate".parse().unwrap(),
    );
    // CORS headers for streaming
    h.insert("Access-Control-Allow-Origin", "*".parse().unwrap());
    h.insert(
        "Access-Control-Allow-Methods",
        "GET, POST, OPTIONS".parse().unwrap(),
    );
    h.insert("Access-Control-Allow-Headers", "*".parse().unwrap());
    if status == StatusCode::PARTIAL_CONTENT {
        let cr = format!("bytes {}-{}/{}", start, end, file_size);
        h.insert(axum::http::header::CONTENT_RANGE, cr.parse().unwrap());
    }
    *resp.status_mut() = status;
    Ok(resp)
}

pub async fn get_log_file(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(date): Path<String>,
) -> impl IntoResponse {
    let full_path = match resolve_log_file_path(&state, &date) {
        Ok(path) => path,
        Err(StatusCode::NOT_FOUND) => {
            return (StatusCode::NOT_FOUND, "".to_string()).into_response()
        }
        Err(status) => {
            eprintln!(
                "❌ Error al resolver ruta de log ({}): status {:?}",
                date, status
            );
            return (status, "Error interno del servidor".to_string()).into_response();
        }
    };

    match tokio::fs::read_to_string(&full_path).await {
        Ok(content) => (StatusCode::OK, content).into_response(),
        Err(e) => {
            eprintln!("❌ Error al leer el archivo de log: {}", e);
            if e.kind() == std::io::ErrorKind::NotFound {
                (StatusCode::NOT_FOUND, "".to_string()).into_response()
            } else {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Error interno del servidor".to_string(),
                )
                    .into_response()
            }
        }
    }
}

// Stream que "sigue" creciendo el archivo para verlo en tiempo real
pub async fn stream_recording_tail(
    State(state): State<Arc<AppState>>,
    Path(file_path): Path<String>,
) -> Result<Response, StatusCode> {
    // Autenticación ya validada por middleware flexible

    let (full_path, mut file) = open_recording_file(&state, &file_path).await?;

    // Posición inicial: desde el principio (para incluir ftyp+moov) y que el reproductor pueda decodificar
    let mut pos: u64 = 0;

    let stream = async_stream::stream! {
        let mut buf = vec![0u8; 64 * 1024]; // 64KB
        let mut sent_header = false;
    let mut _header_end: Option<u64> = None; // offset del fin de 'moov'

        loop {
            // Leer si hay nuevos datos
            let len = match tokio::fs::metadata(&full_path).await { Ok(m)=> m.len(), Err(_)=> 0 };

            if !sent_header {
                // Esperar a que el archivo tenga suficiente tamaño para contener metadatos
                if len >= 64 * 1024 { // Reducido de 1MB a 64KB - suficiente para ftyp + moov básico
                    // Con faststart=true, el moov debería estar al inicio
                    // Leer los primeros 128KB para asegurar tener ftyp+moov
                    let header_size = std::cmp::min(len, 128 * 1024) as usize; // Reducido de 2MB a 128KB
                    let mut header_buf = vec![0u8; header_size];

                    if let Err(e) = file.seek(std::io::SeekFrom::Start(0)).await {
                        eprintln!("header seek err: {}", e);
                        break;
                    }
                    match file.read_exact(&mut header_buf).await {
                        Ok(_) => {
                            // Verificar que al menos tenga ftyp y algo de moov
                            if header_buf.len() >= 8 && &header_buf[4..8] == b"ftyp" {
                                pos = header_size as u64;
                                sent_header = true;
                                println!("📤 Enviado header MP4: {} bytes", header_size);
                                yield Ok::<Bytes, std::io::Error>(Bytes::from(header_buf));
                                continue;
                            } else {
                                tokio::time::sleep(Duration::from_millis(100)).await; // Reducido de 500ms a 100ms
                                continue;
                            }
                        }
                        Err(e) => {
                            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                                tokio::time::sleep(Duration::from_millis(500)).await;
                                continue;
                            }
                            eprintln!("header read err: {}", e);
                            break;
                        }
                    }
                } else {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
            } else if len > pos {
                // Para datos después del header, enviar fragmentos más pequeños para menor latencia
                let available = len - pos;
                if available >= 4 * 1024 { // Reducido de 32KB a 4KB para menor latencia
                    let to_read = std::cmp::min(buf.len() as u64, available) as usize;
                    if let Err(e) = file.seek(std::io::SeekFrom::Start(pos)).await {
                        eprintln!("seek err: {}", e);
                        break;
                    }
                    match file.read_exact(&mut buf[..to_read]).await {
                        Ok(_) => {
                            pos += to_read as u64;
                            println!("📤 Enviado fragmento: {} bytes (pos: {})", to_read, pos);
                            yield Ok::<Bytes, std::io::Error>(Bytes::copy_from_slice(&buf[..to_read]));
                            continue;
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                            // Se alcanzó fin temporal, esperar menos tiempo
                            tokio::time::sleep(Duration::from_millis(100)).await; // Reducido de 500ms a 100ms
                        }
                        Err(e) => {
                            eprintln!("read err: {}", e);
                            break;
                        }
                    }
                }
            }

            // No hay nuevos datos o aún no listo: dormir un poco
            tokio::time::sleep(Duration::from_millis(100)).await; // Reducido de 500ms a 100ms para más responsividad
        }
    };

    let mut resp = Response::new(Body::from_stream(stream));
    let headers = resp.headers_mut();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        "video/mp4".parse().unwrap(),
    );
    headers.insert(
        axum::http::header::CONTENT_DISPOSITION,
        "inline; filename=live.mp4".parse().unwrap(),
    );
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        "no-cache, no-store, must-revalidate".parse().unwrap(),
    );
    headers.insert(axum::http::header::PRAGMA, "no-cache".parse().unwrap());
    headers.insert(axum::http::header::EXPIRES, "0".parse().unwrap());
    // Importante: headers para streaming (sin Accept-Ranges para evitar skips del moov)
    headers.insert("X-Content-Type-Options", "nosniff".parse().unwrap());
    headers.insert("X-Accel-Buffering", "no".parse().unwrap()); // nginx: desactiva buffering
    headers.insert("Cache-Control", "no-transform".parse().unwrap()); // evita proxies que reescriben
    headers.insert("Access-Control-Allow-Origin", "*".parse().unwrap());
    headers.insert("Connection", "keep-alive".parse().unwrap());
    Ok(resp)
}

// Nueva función WebSocket para streaming de resumen de grabaciones por fechas
pub async fn recordings_summary_ws(
    ws: WebSocketUpgrade,
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_recordings_summary_ws(socket, state))
}

async fn handle_recordings_summary_ws(mut socket: WebSocket, state: Arc<AppState>) {
    println!("🎯 WebSocket: Recordings summary stream started");

    // Enviar resumen inicial
    if let Err(e) = send_recordings_summary(&mut socket, &state).await {
        println!("Error sending initial summary: {}", e);
        return;
    }

    // Mantener conexión abierta y enviar actualizaciones periódicas
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                if let Err(e) = send_recordings_summary(&mut socket, &state).await {
                    println!("Error sending summary update: {}", e);
                    break;
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(axum::extract::ws::Message::Close(_))) => {
                        println!("WebSocket closed by client");
                        break;
                    }
                    Some(Err(e)) => {
                        println!("WebSocket error: {}", e);
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    println!("🎯 WebSocket: Recordings summary stream ended");
}

async fn send_recordings_summary(
    socket: &mut WebSocket,
    state: &Arc<AppState>,
) -> Result<(), Box<dyn std::error::Error>> {
    let snapshot = { state.recording_snapshot.lock().await.clone() };

    let summary_data = serde_json::json!({
        "type": "summary",
        "data": snapshot.day_summaries,
    "timestamp": Utc::now().to_rfc3339(),
        "last_scan": snapshot.last_scan.map(|ts| ts.to_rfc3339()),
        "total": snapshot.total_count,
    });

    socket
        .send(axum::extract::ws::Message::Text(summary_data.to_string()))
        .await?;
    Ok(())
}
