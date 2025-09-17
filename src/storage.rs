use crate::auth::check_auth;
use crate::AppState;
use axum::{
    extract::{Path, State, Query},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
    body::Body,
};
use serde::{Serialize, Deserialize};
use std::{fs, path::PathBuf, sync::Arc, time::Duration};
use fs2;
use chrono::{DateTime, Utc};
use tokio::{fs::File, time::sleep};
use tokio_util::io::ReaderStream;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use bytes::Bytes;

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
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(status) = check_auth(&headers, &state.proxy_token).await {
        return (status, Json(ApiResponse {
            status: "error".to_string(),
            message: "Unauthorized".to_string(),
        })).into_response();
    }

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
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse {
                status: "error".to_string(),
                message: "Failed to get storage info".to_string(),
            })).into_response()
        }
    }
}

#[derive(Serialize)]
pub struct Recording {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub last_modified: DateTime<Utc>,
}

// Estructura para los parámetros de la consulta de paginación y filtrado
#[derive(Deserialize)]
pub struct ListRecordingsParams {
    pub page: Option<u64>,
    pub limit: Option<u64>,
    pub date: Option<String>,
}

// Auth helper via query param (?token=) to ease VLC/players
#[derive(Deserialize, Default)]
pub struct TokenQuery { pub token: Option<String> }

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
                            let recording = Recording {
                                name: file_name_str.to_string(),
                                path: entry_path.to_str().unwrap_or_default().to_string(),
                                size: metadata.len(),
                                last_modified: modified_time.into(),
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
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<ListRecordingsParams>,
) -> impl IntoResponse {
    if let Err(status) = check_auth(&headers, &state.proxy_token).await {
        return (status, Json(ApiResponse {
            status: "error".to_string(),
            message: "Unauthorized".to_string(),
        })).into_response();
    }

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

    let paginated_recordings: Vec<Recording> = all_recordings
        .into_iter()
        .skip(start)
        .take(limit)
        .collect();
    
    (StatusCode::OK, Json(paginated_recordings)).into_response()
}

pub async fn delete_recording(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(file_path): Path<String>,
) -> impl IntoResponse {
    if let Err(status) = check_auth(&headers, &state.proxy_token).await {
        return (status, Json(ApiResponse {
            status: "error".to_string(),
            message: "Unauthorized".to_string(),
        })).into_response();
    }
    
    let candidate = PathBuf::from(&state.storage_path).join(&file_path);
    // Canonicalize to avoid .. traversal and ensure it's within storage root
    let Ok(full_path) = candidate.canonicalize() else {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse { status: "error".into(), message: "Invalid path".into()})).into_response();
    };
    let Ok(root) = state.storage_path.canonicalize() else {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse { status: "error".into(), message: "Storage misconfigured".into()})).into_response();
    };
    if !full_path.starts_with(&root) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse { status: "error".into(), message: "Path not allowed".into()})).into_response();
    }

    if full_path.exists() && full_path.is_file() {
        match fs::remove_file(&full_path) {
            Ok(_) => (StatusCode::OK, Json(ApiResponse {
                status: "success".to_string(),
                message: "File deleted successfully".to_string(),
            })).into_response(),
            Err(e) => {
                eprintln!("❌ Error al eliminar el archivo: {}", e);
                (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse {
                    status: "error".to_string(),
                    message: "Failed to delete file".to_string(),
                })).into_response()
            }
        }
    } else {
        (StatusCode::NOT_FOUND, Json(ApiResponse {
            status: "error".to_string(),
            message: "File not found".to_string(),
        })).into_response()
        
    }
}

pub async fn stream_recording(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    Path(file_path): Path<String>,
) -> Result<Response, StatusCode> {
    // Auth: acepta header Authorization o query ?token=
    if q.token.as_deref() != Some(&state.proxy_token) {
        if let Err(_status) = check_auth(&headers, &state.proxy_token).await { return Err(StatusCode::UNAUTHORIZED); }
    }
    
    let candidate = PathBuf::from(&state.storage_path).join(&file_path);
    // Canonicalize to avoid .. traversal and ensure it's within storage root
    let full_path = candidate.canonicalize().map_err(|_| StatusCode::BAD_REQUEST)?;
    let root = state.storage_path.canonicalize().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !full_path.starts_with(&root) {
        return Err(StatusCode::FORBIDDEN);
    }

    // Abrir el archivo de forma asíncrona
    let file = match File::open(&full_path).await {
        Ok(file) => file,
        Err(_) => return Err(StatusCode::NOT_FOUND),
    };

    // Crear un stream para el cuerpo de la respuesta
    let stream = ReaderStream::new(file);

    // Preparar los headers para la respuesta
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(axum::http::header::CONTENT_TYPE, "video/mp4".parse().unwrap());
    headers.insert(axum::http::header::CONTENT_DISPOSITION, "inline".parse().unwrap());

    // Devolver la respuesta con el stream
    Ok(Response::builder()
        .status(StatusCode::OK)
        .body(Body::from_stream(stream))
        .unwrap()
    )
}

pub async fn get_log_file(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(date): Path<String>,
) -> impl IntoResponse {
    if let Err(status) = check_auth(&headers, &state.proxy_token).await {
        return (status, Json(ApiResponse {
            status: "error".to_string(),
            message: "Unauthorized".to_string(),
        })).into_response();
    }

    // Nuevo esquema: logs dentro de carpeta del día DD-MM-YY
    let file_name = format!("{}-log.txt", date);
    // Convertir YYYY-MM-DD -> DD-MM-YY
    let day_dir = match chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d") {
        Ok(d) => state.storage_path.join(d.format("%d-%m-%y").to_string()),
        Err(_) => state.storage_path.clone(),
    };
    let full_path_new = day_dir.join(&file_name);
    // Compat: fallback a raíz si no existe
    let full_path = if full_path_new.exists() { full_path_new } else { state.storage_path.join(&file_name) };

    match tokio::fs::read_to_string(&full_path).await {
        Ok(content) => (StatusCode::OK, content).into_response(),
        Err(e) => {
            eprintln!("❌ Error al leer el archivo de log: {}", e);
            if e.kind() == std::io::ErrorKind::NotFound {
                (StatusCode::NOT_FOUND, "".to_string()).into_response()
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, "Error interno del servidor".to_string()).into_response()
            }
        }
    }
}

// Stream que "sigue" creciendo el archivo para verlo en tiempo real
pub async fn stream_recording_tail(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    Path(file_path): Path<String>,
) -> Result<Response, StatusCode> {
    // Auth: acepta header Authorization o query ?token=
    if q.token.as_deref() != Some(&state.proxy_token) {
        if let Err(_status) = check_auth(&headers, &state.proxy_token).await { return Err(StatusCode::UNAUTHORIZED); }
    }

    let candidate = PathBuf::from(&state.storage_path).join(&file_path);
    let full_path = candidate.canonicalize().map_err(|_| StatusCode::BAD_REQUEST)?;
    let root = state.storage_path.canonicalize().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !full_path.starts_with(&root) { return Err(StatusCode::FORBIDDEN); }

    // Abrir archivo y crear stream que sigue leyendo mientras crece
    let mut file = File::open(&full_path).await.map_err(|_| StatusCode::NOT_FOUND)?;

    // Posición inicial: desde el principio (para incluir moov/ftyp) y que el reproductor pueda decodificar
    let mut pos: u64 = 0;

    let stream = async_stream::stream! {
        let mut buf = vec![0u8; 64 * 1024]; // 64KB
        loop {
            // Leer si hay nuevos datos
            let len = match tokio::fs::metadata(&full_path).await { Ok(m)=> m.len(), Err(_)=> 0 };
            if len > pos {
                // Hay datos nuevos, leer en bloques
                if let Err(e) = file.seek(std::io::SeekFrom::Start(pos)).await { eprintln!("seek err: {}", e); break; }
                while pos < len {
                    let to_read = std::cmp::min(buf.len() as u64, len - pos) as usize;
                    match file.read_exact(&mut buf[..to_read]).await {
                        Ok(_) => {
                            pos += to_read as u64;
                            yield Ok::<Bytes, std::io::Error>(Bytes::copy_from_slice(&buf[..to_read]));
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                            // Se alcanzó fin temporal, salir a esperar
                            break;
                        }
                        Err(e) => {
                            eprintln!("read err: {}", e);
                            break;
                        }
                    }
                }
            } else {
                // No hay nuevos datos; dormir un poco
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    };

    let mut resp = Response::new(Body::from_stream(stream));
    let headers = resp.headers_mut();
    headers.insert(axum::http::header::CONTENT_TYPE, "video/mp4".parse().unwrap());
    headers.insert(axum::http::header::CACHE_CONTROL, "no-cache".parse().unwrap());
    Ok(resp)
}


// Background task to automatically manage storage space
pub async fn start_cleanup_task(storage_path: PathBuf) {
    loop {
        // Espera un período de tiempo antes de revisar de nuevo
        sleep(Duration::from_secs(30 * 60)).await; // Revisa cada 30 minutos

        // Obtiene las estadísticas de almacenamiento
        if let Ok(stats) = fs2::statvfs(&storage_path) {
            let total_space = stats.total_space();
            let used_space = total_space - stats.free_space();
            let used_percentage = (used_space as f64 / total_space as f64) * 100.0;

            println!("✅ Almacenamiento usado: {:.2}%", used_percentage);

            // Si el uso de almacenamiento está por encima del umbral alto (90%)
            if used_percentage > 90.0 {
                println!("⚠️ Uso de almacenamiento crítico, iniciando limpieza...");
                
                let mut all_recordings = get_recordings_recursively(&storage_path);
                // Ordena las grabaciones por fecha de modificación (las más antiguas primero)
                all_recordings.sort_by_key(|rec| rec.last_modified);
                
                // Sigue eliminando los archivos más antiguos hasta que el uso esté por debajo del umbral bajo (85%)
                while let Ok(current_stats) = fs2::statvfs(&storage_path) {
                    let current_used = current_stats.total_space() - current_stats.free_space();
                    let current_percentage = (current_used as f64 / current_stats.total_space() as f64) * 100.0;
                    
                    if current_percentage < 85.0 {
                        println!("✅ Limpieza completa. Almacenamiento actual: {:.2}%", current_percentage);
                        break;
                    }
                    
                    if let Some(recording) = all_recordings.pop() {
                        let log_path = PathBuf::from(&recording.path.replace(".mp4", "-log.txt"));
                        
                        // Eliminar archivo de video
                        if let Err(e) = fs::remove_file(&recording.path) {
                            eprintln!("❌ Error al eliminar el video {}: {}", recording.path, e);
                        } else {
                            println!("🗑️ Video eliminado: {}", recording.name);
                        }

                        // Eliminar archivo de log
                        if log_path.exists() {
                            if let Err(e) = fs::remove_file(&log_path) {
                                eprintln!("❌ Error al eliminar el log {}: {}", log_path.display(), e);
                            } else {
                                println!("🗑️ Log eliminado: {}", log_path.display());
                            }
                        }
                    } else {
                        // No hay más grabaciones para eliminar
                        println!("⚠️ No hay más grabaciones para eliminar, el disco sigue lleno.");
                        break;
                    }
                }
            }
        }
    }
}
