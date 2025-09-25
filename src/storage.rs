use crate::{auth::RequireAuth, AppState};
use axum::http::header;
use axum::response::sse::Event;
use axum::{
    body::Body,
    extract::{Path, Query, State, ws::{WebSocket, WebSocketUpgrade}},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response, Sse},
    Json,
};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use fs2;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::{convert::Infallible, fs, path::PathBuf, sync::Arc, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, BufReader};
use tokio::{fs::File, time::sleep};

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

    match fs2::statvfs(&state.storage_path) {
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

#[derive(Serialize)]
pub struct Recording {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub last_modified: DateTime<Utc>,
    pub duration: Option<f64>,
}

// Estructura para los parámetros de la consulta de paginación y filtrado
#[derive(Deserialize)]
pub struct ListRecordingsParams {
    pub page: Option<u64>,
    pub limit: Option<u64>,
    pub date: Option<String>,
}

// Función auxiliar para obtener la duración de un archivo MP4 usando ffprobe
fn get_mp4_duration(file_path: &PathBuf) -> Option<f64> {
    use std::process::Command;

    // Intentar usar ffprobe para obtener la duración
    let output = match Command::new("ffprobe")
        .args(&[
            "-v", "quiet",
            "-print_format", "json",
            "-show_format",
            file_path.to_str()?,
        ])
        .output()
    {
        Ok(out) => out,
        Err(_) => {
            // ffprobe no está disponible, devolver None silenciosamente
            return None;
        }
    };

    if !output.status.success() {
        return None;
    }

    let json_str = String::from_utf8(output.stdout).ok()?;
    let json: serde_json::Value = serde_json::from_str(&json_str).ok()?;

    if let Some(format_obj) = json.get("format") {
        if let Some(duration_str) = format_obj.get("duration") {
            if let Some(duration_str) = duration_str.as_str() {
                return duration_str.parse::<f64>().ok();
            }
        }
    }

    None
}

// Función auxiliar recursiva para obtener grabaciones de todos los subdirectorios
fn get_recordings_recursively(path: &PathBuf) -> Vec<Recording> {
    let mut recordings = Vec::new();

    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let entry_path = entry.path();
            if entry_path.is_dir() {
                // Si es un directorio, llamamos a la función de nuevo
                recordings.append(&mut get_recordings_recursively(&entry_path));
            } else if entry_path.is_file() {
                if let Some(file_name_str) = entry_path.file_name().and_then(|s| s.to_str()) {
                    // Filtramos los archivos de log
                    if file_name_str.ends_with("-log.txt") {
                        continue;
                    }

                    if let Ok(metadata) = fs::metadata(&entry_path) {
                        if let Ok(modified_time) = metadata.modified() {
                            // Calcular duración solo para archivos MP4
                            let duration = if file_name_str.ends_with(".mp4") {
                                get_mp4_duration(&entry_path)
                            } else {
                                None
                            };

                            let recording = Recording {
                                name: file_name_str.to_string(),
                                path: entry_path.to_str().unwrap_or_default().to_string(),
                                size: metadata.len(),
                                last_modified: modified_time.into(),
                                duration,
                            };
                            recordings.push(recording);
                        }
                    }
                }
            }
        }
    }

    recordings
}

pub async fn list_recordings(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListRecordingsParams>,
) -> impl IntoResponse {
    // Obtenemos todas las grabaciones de forma recursiva
    let mut all_recordings = get_recordings_recursively(&state.storage_path);

    // Filtramos por fecha si se provee el parámetro
    if let Some(filter_date) = &params.date {
        all_recordings.retain(|rec| rec.name.starts_with(filter_date));
    }

    // Lógica de paginación
    let page = params.page.unwrap_or(1) as usize;
    let limit = params.limit.unwrap_or(20) as usize;
    let start = (page - 1) * limit;

    let paginated_recordings: Vec<Recording> =
        all_recordings.into_iter().skip(start).take(limit).collect();

    (StatusCode::OK, Json(paginated_recordings)).into_response()
}

pub async fn delete_recording(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(file_path): Path<String>,
) -> impl IntoResponse {
    let candidate = PathBuf::from(&state.storage_path).join(&file_path);
    // Canonicalize to avoid .. traversal and ensure it's within storage root
    let Ok(full_path) = candidate.canonicalize() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse {
                status: "error".into(),
                message: "Invalid path".into(),
            }),
        )
            .into_response();
    };
    let Ok(root) = state.storage_path.canonicalize() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse {
                status: "error".into(),
                message: "Storage misconfigured".into(),
            }),
        )
            .into_response();
    };
    if !full_path.starts_with(&root) {
        return (
            StatusCode::FORBIDDEN,
            Json(ApiResponse {
                status: "error".into(),
                message: "Path not allowed".into(),
            }),
        )
            .into_response();
    }

    if full_path.exists() && full_path.is_file() {
        match fs::remove_file(&full_path) {
            Ok(_) => (
                StatusCode::OK,
                Json(ApiResponse {
                    status: "success".to_string(),
                    message: "File deleted successfully".to_string(),
                }),
            )
                .into_response(),
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

    let candidate = PathBuf::from(&state.storage_path).join(&file_path);
    // Canonicalize to avoid .. traversal and ensure it's within storage root
    let full_path = candidate
        .canonicalize()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let root = state
        .storage_path
        .canonicalize()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !full_path.starts_with(&root) {
        return Err(StatusCode::FORBIDDEN);
    }

    // Tamaño del archivo para rangos
    let meta = match tokio::fs::metadata(&full_path).await {
        Ok(m) => m,
        Err(_) => return Err(StatusCode::NOT_FOUND),
    };
    let file_size = meta.len();

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
    let mut file = match File::open(&full_path).await {
        Ok(f) => f,
        Err(_) => return Err(StatusCode::NOT_FOUND),
    };
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
    h.insert(axum::http::header::CACHE_CONTROL, "no-cache, no-store, must-revalidate".parse().unwrap());
    // CORS headers for streaming
    h.insert("Access-Control-Allow-Origin", "*".parse().unwrap());
    h.insert("Access-Control-Allow-Methods", "GET, POST, OPTIONS".parse().unwrap());
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
    // Nuevo esquema: logs dentro de carpeta del día DD-MM-YY
    let file_name = format!("{}-log.txt", date);
    // Convertir YYYY-MM-DD -> DD-MM-YY
    let day_dir = match chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d") {
        Ok(d) => state.storage_path.join(d.format("%d-%m-%y").to_string()),
        Err(_) => state.storage_path.clone(),
    };
    let full_path_new = day_dir.join(&file_name);
    // Compat: fallback a raíz si no existe
    let full_path = if full_path_new.exists() {
        full_path_new
    } else {
        state.storage_path.join(&file_name)
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

    let candidate = PathBuf::from(&state.storage_path).join(&file_path);
    let full_path = candidate
        .canonicalize()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let root = state
        .storage_path
        .canonicalize()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !full_path.starts_with(&root) {
        return Err(StatusCode::FORBIDDEN);
    }

    // Abrir archivo y crear stream que sigue leyendo mientras crece
    let mut file = File::open(&full_path)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;

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

// Endpoint para lista completa de grabaciones con metadatos
pub async fn recordings_list(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    // Obtener todas las grabaciones sin paginación
    let recordings = get_recordings_recursively(&state.storage_path);
    (StatusCode::OK, Json(recordings))
}

// Nueva función SSE para streaming de logs de grabaciones en tiempo real
pub async fn stream_recording_logs_sse(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
    Path(date): Path<String>,
) -> Result<Response, StatusCode> {
    // Construir la ruta del archivo de log
    let file_name = format!("{}-log.txt", date);
    let day_dir = match chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d") {
        Ok(d) => state.storage_path.join(d.format("%d-%m-%y").to_string()),
        Err(_) => state.storage_path.clone(),
    };
    let full_path_new = day_dir.join(&file_name);
    let log_path = if full_path_new.exists() {
        full_path_new
    } else {
        state.storage_path.join(&file_name)
    };

    // Verificar que el archivo existe
    if !log_path.exists() {
        return Err(StatusCode::NOT_FOUND);
    }

    // Abrir el archivo para lectura
    let file = File::open(&log_path)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut reader = BufReader::new(file);

    // Crear stream SSE
    let stream = async_stream::stream! {
        let mut buf = String::new();
        let mut last_pos: u64 = 0;

        // Enviar evento de conexión
        yield Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b":connected\n\n"));

        loop {
            // Verificar si el archivo ha crecido
            let metadata = match tokio::fs::metadata(&log_path).await {
                Ok(m) => m,
                Err(_) => break, // Archivo eliminado
            };

            let current_size = metadata.len();

            // Si el archivo ha crecido, leer las nuevas líneas
            if current_size > last_pos {
                // Mover el cursor a la última posición leída
                if let Ok(_) = reader.seek(std::io::SeekFrom::Start(last_pos)).await {
                    buf.clear();
                    while let Ok(read) = reader.read_line(&mut buf).await {
                        if read == 0 {
                            break; // EOF
                        }

                        // Enviar línea como evento SSE
                        let line = format!("data: {}\n\n", buf.trim_end_matches(['\n', '\r']));
                        yield Ok(axum::body::Bytes::from(line));

                        buf.clear();
                    }
                }

                last_pos = current_size;
            }

            // Esperar un poco antes de verificar nuevamente
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }
    };

    // Crear respuesta SSE
    let mut resp = Response::new(Body::from_stream(stream));
    let headers = resp.headers_mut();
    headers.insert(header::CONTENT_TYPE, "text/event-stream".parse().unwrap());
    headers.insert(header::CACHE_CONTROL, "no-cache".parse().unwrap());
    headers.insert(header::CONNECTION, "keep-alive".parse().unwrap());
    headers.insert("X-Accel-Buffering", "no".parse().unwrap());
    // CORS headers for streaming
    headers.insert("Access-Control-Allow-Origin", "*".parse().unwrap());
    headers.insert("Access-Control-Allow-Methods", "GET, POST, OPTIONS".parse().unwrap());
    headers.insert("Access-Control-Allow-Headers", "*".parse().unwrap());

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
    let recordings = get_recordings_recursively(&state.storage_path);

    // Crear mapa de fechas con conteo
    use std::collections::HashMap;
    let mut date_counts: HashMap<String, usize> = HashMap::new();

    for recording in &recordings {
        if let Some(date_str) = recording.modified.as_ref()
            .and_then(|dt| dt.split('T').next()) {
            *date_counts.entry(date_str.to_string()).or_insert(0) += 1;
        }
    }

    // Convertir a vector ordenado por fecha descendente
    let mut summary: Vec<(String, usize)> = date_counts.into_iter().collect();
    summary.sort_by(|a, b| b.0.cmp(&a.0));

    let summary_data = serde_json::json!({
        "type": "summary",
        "data": summary,
        "timestamp": chrono::Utc::now().to_rfc3339()
    });

    socket.send(axum::extract::ws::Message::Text(summary_data.to_string())).await?;
    Ok(())
}

// Nueva función WebSocket para streaming de lista completa de grabaciones
pub async fn recordings_list_ws(
    ws: WebSocketUpgrade,
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_recordings_list_ws(socket, state))
}

async fn handle_recordings_list_ws(mut socket: WebSocket, state: Arc<AppState>) {
    println!("🎯 WebSocket: Recordings list stream started");

    // Enviar lista inicial
    if let Err(e) = send_recordings_list(&mut socket, &state).await {
        println!("Error sending initial list: {}", e);
        return;
    }

    // Mantener conexión abierta y enviar actualizaciones periódicas
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                if let Err(e) = send_recordings_list(&mut socket, &state).await {
                    println!("Error sending list update: {}", e);
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

    println!("🎯 WebSocket: Recordings list stream ended");
}

async fn send_recordings_list(
    socket: &mut WebSocket,
    state: &Arc<AppState>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut recordings = get_recordings_recursively(&state.storage_path);

    // Ordenar por fecha modificada descendente
    recordings.sort_by(|a, b| {
        let a_date = a.modified.as_ref()
            .and_then(|dt| DateTime::parse_from_rfc3339(dt).ok())
            .unwrap_or_else(|| DateTime::parse_from_rfc3339("1900-01-01T00:00:00Z").unwrap());
        let b_date = b.modified.as_ref()
            .and_then(|dt| DateTime::parse_from_rfc3339(dt).ok())
            .unwrap_or_else(|| DateTime::parse_from_rfc3339("1900-01-01T00:00:00Z").unwrap());
        b_date.cmp(&a_date)
    });

    let list_data = serde_json::json!({
        "type": "list",
        "data": recordings,
        "timestamp": chrono::Utc::now().to_rfc3339()
    });

    socket.send(axum::extract::ws::Message::Text(list_data.to_string())).await?;
    Ok(())
}
