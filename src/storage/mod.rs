//! Módulo de almacenamiento para Vigilante.
//!
//! Gestiona streaming en vivo de grabaciones antiguas,
//! delegando operaciones pesadas a `depends/`.

pub mod depends;

pub use depends::db::StorageDb;
pub use depends::filesystem::FileManager;
pub use depends::paths::PathResolver;
pub use depends::snapshot::SnapshotManager;

use crate::error::VigilanteError;
use crate::AppState;
use crate::RecordingEntry;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncSeekExt;
use tokio::process::Command;
use std::process::Stdio;
use serde::Serialize;
use log::warn;
use std::sync::Arc;
use std::sync::Mutex;
use std::collections::HashMap;
use std::io::Read as _;
use async_stream;
use bytes;
// GStreamer discovery for media metadata (duration, codecs)
use gstreamer_pbutils::Discoverer; // main discoverer
use gstreamer_pbutils::DiscovererInfo; // info struct
use gstreamer_pbutils::DiscovererResult; // result enum

#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

#[derive(Serialize)]
pub struct DayRecordings {
    pub name: String,
    pub records: usize,
}

/// Estado de archivos en grabación activa
#[derive(Clone)]
pub struct RecordingState {
    pub active_recordings: Arc<Mutex<std::collections::HashSet<String>>>,
}

impl RecordingState {
    pub fn new() -> Self {
        Self {
            active_recordings: Arc::new(Mutex::new(std::collections::HashSet::new())),
        }
    }

    pub fn is_recording(&self, path: &str) -> bool {
        self.active_recordings.lock().unwrap().contains(path)
    }

    pub fn start_recording(&self, path: &str) {
        self.active_recordings
            .lock()
            .unwrap()
            .insert(path.to_string());
    }

    pub fn stop_recording(&self, path: &str) {
        self.active_recordings.lock().unwrap().remove(path);
    }
}

impl Default for RecordingState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Serialize, Clone, Default)]
pub struct StorageInfoInner {
    pub storage_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_space_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_space_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free_space_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_space_bytes: Option<u64>,
}

#[derive(Debug, Serialize, Clone, Default)]
pub struct RecordingSnapshotInfo {
    pub total_files: usize,
    pub total_size_bytes: u64,
    pub days_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_scan_utc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_recording: Option<String>,
    pub day_summaries: Vec<crate::DaySummary>,
}

#[derive(Debug, Serialize, Clone, Default)]
pub struct StorageInfoResponse {
    pub storage_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_space_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_space_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free_space_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_space_bytes: Option<u64>,
    pub total_files: usize,
    pub days_count: usize,
    pub total_recorded_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_scan_utc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_recording: Option<String>,
    pub storage_info: StorageInfoInner,
    pub recording_snapshot: RecordingSnapshotInfo,
}

fn gather_disk_usage(storage_path: &std::path::Path) -> StorageInfoInner {
    let path_string = storage_path.display().to_string();

    #[cfg(unix)]
    {
        let c_path = match CString::new(storage_path.as_os_str().as_bytes()) {
            Ok(cstr) => cstr,
            Err(err) => {
                warn!(
                    "storage::gather_disk_usage: failed to convert path {} to CString: {}",
                    path_string, err
                );
                return StorageInfoInner {
                    storage_path: path_string,
                    ..Default::default()
                };
            }
        };

        unsafe {
            let mut stat: libc::statvfs = std::mem::zeroed();
            if libc::statvfs(c_path.as_ptr(), &mut stat) == 0 {
                let block_size = stat.f_frsize as u64;
                let total = stat.f_blocks as u64 * block_size;
                let free = stat.f_bfree as u64 * block_size;
                let available = stat.f_bavail as u64 * block_size;
                let used = total.saturating_sub(free);

                return StorageInfoInner {
                    storage_path: path_string,
                    total_space_bytes: Some(total),
                    used_space_bytes: Some(used),
                    free_space_bytes: Some(free),
                    available_space_bytes: Some(available),
                };
            } else {
                let err = std::io::Error::last_os_error();
                warn!(
                    "storage::gather_disk_usage: statvfs failed for {}: {}",
                    path_string, err
                );
            }
        }
    }

    StorageInfoInner {
        storage_path: path_string,
        ..Default::default()
    }
}

fn gather_recording_snapshot(snapshot: &crate::RecordingSnapshot) -> RecordingSnapshotInfo {
    let last_scan = snapshot.last_scan.map(|dt| dt.to_rfc3339());
    let last_recording = snapshot.last_recording.clone();

    RecordingSnapshotInfo {
        total_files: snapshot.total_count,
        total_size_bytes: snapshot.total_size_bytes,
        days_count: snapshot.day_summaries.len(),
        last_scan_utc: last_scan,
        last_recording,
        day_summaries: snapshot.day_summaries.clone(),
    }
}

fn build_storage_response(
    storage_path: &std::path::Path,
    snapshot: &crate::RecordingSnapshot,
) -> StorageInfoResponse {
    let storage_info = gather_disk_usage(storage_path);
    let recording_snapshot = gather_recording_snapshot(snapshot);

    StorageInfoResponse {
        storage_path: storage_info.storage_path.clone(),
        total_space_bytes: storage_info.total_space_bytes,
        used_space_bytes: storage_info.used_space_bytes,
        free_space_bytes: storage_info.free_space_bytes,
        available_space_bytes: storage_info.available_space_bytes,
        total_files: recording_snapshot.total_files,
        days_count: recording_snapshot.days_count,
        total_recorded_bytes: recording_snapshot.total_size_bytes,
        last_scan_utc: recording_snapshot.last_scan_utc.clone(),
        last_recording: recording_snapshot.last_recording.clone(),
        storage_info,
        recording_snapshot,
    }
}

async fn storage_response_from_state(state: &Arc<AppState>) -> StorageInfoResponse {
    let storage_path = state.storage_path().clone();
    let snapshot = state.system.recording_snapshot.lock().await.clone();
    build_storage_response(&storage_path, &snapshot)
}

pub async fn storage_overview(
    State(state): State<Arc<AppState>>,
) -> Result<Json<StorageInfoResponse>, VigilanteError> {
    Ok(Json(storage_response_from_state(&state).await))
}

pub async fn storage_info(
    State(state): State<Arc<AppState>>,
) -> Result<Json<StorageInfoResponse>, VigilanteError> {
    Ok(Json(storage_response_from_state(&state).await))
}

pub async fn system_storage_info(
    State(state): State<Arc<AppState>>,
) -> Result<Json<StorageInfoResponse>, VigilanteError> {
    Ok(Json(storage_response_from_state(&state).await))
}

/// Manager principal para almacenamiento.
#[derive(Clone)]
pub struct StorageManager {
    recording_state: RecordingState,
}

impl StorageManager {
    pub fn new() -> Self {
        Self {
            recording_state: RecordingState::new(),
        }
    }

    pub async fn refresh_snapshot(&self) -> Result<(), VigilanteError> {
        // Lógica para refrescar snapshot
        Ok(())
    }

    pub fn recording_state(&self) -> &RecordingState {
        &self.recording_state
    }
}

impl Default for StorageManager {
    fn default() -> Self {
        Self::new()
    }
}

// Cache para recordar el instante en que "vimos por primera vez" un archivo en curso.
lazy_static::lazy_static! {
    static ref LIVE_START_SEEN: Mutex<HashMap<String, std::time::SystemTime>> = Mutex::new(HashMap::new());
}

/// Estima el instante de inicio de una grabación en curso.
/// - Preferimos metadata.created() si está disponible.
/// - En Linux puede no estar; entonces usamos un cache de "primer visto" por ruta.
fn estimate_started_at(path: &std::path::Path, meta: &std::fs::Metadata) -> std::time::SystemTime {
    if let Ok(created) = meta.created() {
        return created;
    }
    let key = path.to_string_lossy().to_string();
    let now = std::time::SystemTime::now();
    let mut map = LIVE_START_SEEN.lock().unwrap();
    *map.entry(key).or_insert(now)
}

/// Calcula duración en segundos de una grabación en curso usando inicio estimado.
fn live_duration_seconds(path: &std::path::Path, meta: &std::fs::Metadata) -> Option<f64> {
    let started = estimate_started_at(path, meta);
    if let Ok(elapsed) = std::time::SystemTime::now().duration_since(started) {
        let secs = elapsed.as_secs_f64();
        if secs.is_finite() && secs > 0.0 { return Some(secs); }
    }
    None
}

/// Intenta extraer la duración del archivo de video (segundos, f64).
/// Usa GStreamer Discoverer de forma bloqueante dentro de spawn_blocking para no bloquear el runtime.
async fn probe_duration_seconds(path: &std::path::Path) -> Option<f64> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        // Inicializar GStreamer si no está listo
        let _ = gstreamer::init();

    let timeout = gstreamer::ClockTime::from_mseconds(5_000);
    let discoverer = Discoverer::new(timeout).ok()?; // timeout 5s
        let uri = format!("file://{}", path.display());
        let info: DiscovererInfo = discoverer.discover_uri(&uri).ok()?;
        match info.result() {
            DiscovererResult::Ok | DiscovererResult::Timeout | DiscovererResult::MissingPlugins => {
                if let Some(dur) = info.duration() {
                    // Convertir GstClockTime (nanosegundos) a segundos f64
                    let secs = (dur.nseconds() as f64) / 1_000_000_000.0;
                    if secs.is_finite() && secs > 0.0 {
                        return Some(secs);
                    }
                }
                None
            }
            _ => None,
        }
    })
    .await
    .ok()
    .flatten()
}

/// Lector rápido de duración para MP4 (mvhd) leyendo los primeros ~2MB.
/// Evita invocar GStreamer cuando no es necesario y suele ser más veloz.
fn probe_mp4_duration_quick(path: &std::path::Path) -> Option<f64> {
    // Solo aplica a .mp4
    if path.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("mp4")) != Some(true) {
        return None;
    }
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 2 * 1024 * 1024]; // 2MB
    let n = file.read(&mut buf).ok()?;
    let data = &buf[..n];

    let mut pos = 0usize;
    while pos + 8 <= data.len() {
        let size = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        let kind = &data[pos + 4..pos + 8];
        if size < 8 { break; }
        if kind == b"moov" {
            // Buscar mvhd dentro de moov
            let moov_end = pos.saturating_add(size).min(data.len());
            let mut cpos = pos + 8;
            while cpos + 8 <= moov_end {
                let csize = u32::from_be_bytes([
                    data[cpos], data[cpos + 1], data[cpos + 2], data[cpos + 3],
                ]) as usize;
                if csize < 8 { break; }
                let ctype = &data[cpos + 4..cpos + 8];
                if ctype == b"mvhd" {
                    // Parse mvhd
                    let start = cpos + 8;
                    if start + 4 <= moov_end {
                        let version = data[start];
                        // flags = 3 bytes después de version
                        if version == 0 {
                            // v0: creation_time(4) mod_time(4) timescale(4) duration(4)
                            let need = start + 1 + 3 + 4 + 4 + 4 + 4; // version+flags+4 fields
                            if need <= moov_end {
                                let timescale_off = start + 1 + 3 + 4 + 4;
                                let duration_off = timescale_off + 4;
                                let timescale = u32::from_be_bytes([
                                    data[timescale_off], data[timescale_off + 1], data[timescale_off + 2], data[timescale_off + 3]
                                ]);
                                let duration = u32::from_be_bytes([
                                    data[duration_off], data[duration_off + 1], data[duration_off + 2], data[duration_off + 3]
                                ]);
                                if timescale > 0 {
                                    return Some(duration as f64 / timescale as f64);
                                }
                            }
                        } else if version == 1 {
                            // v1: creation_time(8) mod_time(8) timescale(4) duration(8)
                            let need = start + 1 + 3 + 8 + 8 + 4 + 8;
                            if need <= moov_end {
                                let timescale_off = start + 1 + 3 + 8 + 8;
                                let duration_off = timescale_off + 4;
                                let timescale = u32::from_be_bytes([
                                    data[timescale_off], data[timescale_off + 1], data[timescale_off + 2], data[timescale_off + 3]
                                ]);
                                let duration = u64::from_be_bytes([
                                    data[duration_off], data[duration_off + 1], data[duration_off + 2], data[duration_off + 3],
                                    data[duration_off + 4], data[duration_off + 5], data[duration_off + 6], data[duration_off + 7]
                                ]);
                                if timescale > 0 {
                                    return Some(duration as f64 / timescale as f64);
                                }
                            }
                        }
                    }
                    return None;
                }
                cpos = cpos.saturating_add(csize);
            }
            return None;
        }
        pos = pos.saturating_add(size);
    }
    None
}

#[axum::debug_handler]
pub async fn delete_recording(
    axum::extract::Path(path): axum::extract::Path<String>,
    axum::extract::State(state): axum::extract::State<std::sync::Arc<crate::AppState>>,
) -> impl axum::response::IntoResponse {
    use axum::response::Response;

    // Normalizar y validar la ruta relativa (evitar traversal)
    let mut cleaned = path.replace('\\', "/");
    while cleaned.starts_with('/') {
        cleaned.remove(0);
    }
    let parts: Vec<&str> = cleaned
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if parts.iter().any(|p| *p == "." || *p == "..") {
        return (
            StatusCode::BAD_REQUEST,
            "Invalid path: directory traversal not allowed",
        )
            .into_response();
    }
    if parts.len() < 2 {
        return (
            StatusCode::BAD_REQUEST,
            "Invalid path: expected 'YYYY-MM-DD/filename.mp4'",
        )
            .into_response();
    }

    let full_path = state.storage_root().join(parts.join("/"));

    if !full_path.exists() {
        return (StatusCode::NOT_FOUND, "Recording not found").into_response();
    }
    if !full_path.is_file() {
        return (StatusCode::BAD_REQUEST, "Target is not a file").into_response();
    }

    // Evitar borrar grabación en curso
    if is_file_being_recorded(&full_path).await {
        return (
            StatusCode::LOCKED,
            "Recording is in progress; cannot delete right now",
        )
            .into_response();
    }

    // Intentar borrar
    match tokio::fs::remove_file(&full_path).await {
        Ok(_) => {
            // Intentar refrescar snapshot (no crítico si falla)
            if let Err(e) = crate::storage::refresh_recording_snapshot(&state).await {
                log::warn!("Failed to refresh recording snapshot after delete: {}", e);
            }

            let body = serde_json::json!({
                "deleted": true,
                "path": path,
            });
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .header(
                    header::CACHE_CONTROL,
                    "no-store, no-cache, must-revalidate, proxy-revalidate, max-age=0, no-transform",
                )
                .body(axum::body::Body::from(body.to_string()))
                .unwrap()
        }
        Err(e) => {
            log::error!("Failed to delete recording {}: {}", full_path.display(), e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to delete recording: {}", e),
            )
                .into_response()
        }
    }
}

#[axum::debug_handler]
pub async fn recordings_summary_ws(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> Result<impl axum::response::IntoResponse, VigilanteError> {
    let storage_path = state.storage_path();

    // Leer las carpetas del directorio raíz (cada carpeta es un día)
    let mut entries = tokio::fs::read_dir(storage_path)
        .await
        .map_err(|e| VigilanteError::Io(e))?;

    let mut day_summaries = Vec::new();

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| VigilanteError::Io(e))?
    {
        let path = entry.path();

        // Solo procesar directorios (días)
        if path.is_dir() {
            if let Some(day_name) = path.file_name().and_then(|n| n.to_str()) {
                // Contar archivos de grabación en esta carpeta
                let recording_count = count_recordings_in_day(&path).await?;
                day_summaries.push(DayRecordings {
                    name: day_name.to_string(),
                    records: recording_count,
                });
            }
        }
    }

    // Serializar para logging y para respuesta sin caché
    let payload = serde_json::to_string(&day_summaries).unwrap_or("[]".to_string());
    log::info!("/api/recordings/summary body: {}", payload);

    let mut resp = axum::response::Response::new(axum::body::Body::from(payload));
    resp.headers_mut().insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    resp.headers_mut().insert(header::CACHE_CONTROL, "no-store, no-cache, must-revalidate, proxy-revalidate, max-age=0, no-transform".parse().unwrap());
    resp.headers_mut().insert(header::PRAGMA, "no-cache".parse().unwrap());
    resp.headers_mut().insert(header::EXPIRES, "0".parse().unwrap());
    resp.headers_mut().insert("surrogate-control", "no-store".parse().unwrap());
    resp.headers_mut().insert("x-accel-buffering", "no".parse().unwrap());
    Ok(resp)
}

async fn count_recordings_in_day(day_path: &std::path::Path) -> Result<usize, VigilanteError> {
    let mut entries = tokio::fs::read_dir(day_path)
        .await
        .map_err(|e| VigilanteError::Io(e))?;

    let mut count = 0;

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| VigilanteError::Io(e))?
    {
        let path = entry.path();

        // Contar archivos con extensiones de video
        if path.is_file() {
            if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
                match extension {
                    "mkv" | "mp4" | "avi" | "mov" | "flv" | "wmv" => {
                        count += 1;
                    }
                    _ => {}
                }
            }
        }
    }

    Ok(count)
}

async fn list_recordings_in_day(
    day_path: &std::path::Path,
    date: &str,
) -> Result<Vec<RecordingEntry>, VigilanteError> {
    let mut entries = tokio::fs::read_dir(day_path)
        .await
        .map_err(|e| VigilanteError::Io(e))?;

    let mut recordings = Vec::new();

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| VigilanteError::Io(e))?
    {
        let path = entry.path();

        // Procesar archivos con extensiones de video
        if path.is_file() {
            if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
                match extension {
                    "mkv" | "mp4" | "avi" | "mov" | "flv" | "wmv" => {
                        if let Ok(metadata) = tokio::fs::metadata(&path).await {
                            let file_name = path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("unknown")
                                .to_string();

                            let size = metadata.len();
                            let modified = metadata
                                .modified()
                                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                            let modified_dt = chrono::DateTime::<chrono::Utc>::from(modified);

                            // Duración: priorizar según estado
                            let mut duration: Option<f64>;
                            let live_now = is_file_being_recorded(&path).await;
                            if live_now {
                                // En curso: usar estimación viva para que siempre suba
                                duration = live_duration_seconds(&path, &metadata);
                            } else {
                                // Terminado: rápido por MP4 (mvhd); si falla, GStreamer
                                duration = probe_mp4_duration_quick(&path);
                                if duration.is_none() {
                                    duration = probe_duration_seconds(&path).await;
                                }
                            }

                            let duration_source = if live_now {
                                Some("live_estimate".to_string())
                            } else if path
                                .extension()
                                .and_then(|e| e.to_str())
                                .map(|e| e.eq_ignore_ascii_case("mp4"))
                                == Some(true)
                                && probe_mp4_duration_quick(&path).is_some() {
                                Some("mp4_fast".to_string())
                            } else if duration.is_some() {
                                Some("gstreamer".to_string())
                            } else {
                                None
                            };

                            recordings.push(RecordingEntry {
                                name: file_name.clone(),
                                path: format!(
                                    "{}/{}",
                                    day_path
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("unknown"),
                                    file_name
                                ),
                                size,
                                last_modified: modified_dt,
                                duration,
                                is_live: Some(live_now),
                                duration_source,
                                day: date.to_string(),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // Ordenar por fecha de modificación (más reciente primero)
    recordings.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));

    Ok(recordings)
}

/// Obtener lista detallada de grabaciones de un día específico
pub async fn recordings_by_day(
    axum::extract::Path(date): axum::extract::Path<String>,
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> Result<axum::response::Response, VigilanteError> {
    let storage_path = state.storage_path();
    let day_path = storage_path.join(&date);

    // Verificar que el directorio existe
    if !day_path.exists() || !day_path.is_dir() {
        return Err(VigilanteError::Other(format!(
            "No recordings found for date: {}",
            date
        )));
    }

    // Obtener lista de grabaciones del día
    let recordings = list_recordings_in_day(&day_path, &date).await?;

    // Responder con JSON sin cacheo para reflejar cambios en disco en tiempo real
    let payload = serde_json::to_string(&recordings).unwrap_or("[]".to_string());
    let mut resp = axum::response::Response::new(axum::body::Body::from(payload));
    resp.headers_mut().insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    resp.headers_mut().insert(header::CACHE_CONTROL, "no-store, no-cache, must-revalidate, proxy-revalidate, max-age=0, no-transform".parse().unwrap());
    resp.headers_mut().insert(header::PRAGMA, "no-cache".parse().unwrap());
    resp.headers_mut().insert(header::EXPIRES, "0".parse().unwrap());
    resp.headers_mut().insert("surrogate-control", "no-store".parse().unwrap());
    resp.headers_mut().insert("x-accel-buffering", "no".parse().unwrap());
    Ok(resp.into())
}

async fn calculate_directory_size(day_path: &std::path::Path) -> Result<u64, VigilanteError> {
    let mut entries = tokio::fs::read_dir(day_path)
        .await
        .map_err(|e| VigilanteError::Io(e))?;

    let mut total_size = 0u64;

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| VigilanteError::Io(e))?
    {
        let path = entry.path();

        // Sumar tamaño de archivos con extensiones de video
        if path.is_file() {
            if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
                match extension {
                    "mkv" | "mp4" | "avi" | "mov" | "flv" | "wmv" => {
                        if let Ok(metadata) = tokio::fs::metadata(&path).await {
                            total_size += metadata.len();
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    Ok(total_size)
}

pub async fn refresh_recording_snapshot(
    state: &Arc<AppState>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let storage_path = state.storage_path();

    // Leer las carpetas del directorio raíz (cada carpeta es un día)
    let mut entries = tokio::fs::read_dir(storage_path)
        .await
        .map_err(|e| VigilanteError::Io(e))?;

    let mut total_count = 0;
    let mut total_size_bytes = 0u64;
    let mut day_summaries = Vec::new();
    let mut latest_timestamp = None;

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| VigilanteError::Io(e))?
    {
        let path = entry.path();

        // Solo procesar directorios (días)
        if path.is_dir() {
            if let Some(day_name) = path.file_name().and_then(|n| n.to_str()) {
                // Contar archivos de grabación en esta carpeta
                let recording_count = count_recordings_in_day(&path).await?;
                let day_size = calculate_directory_size(&path).await?;

                total_count += recording_count;
                total_size_bytes += day_size;

                day_summaries.push(crate::DaySummary {
                    day: day_name.to_string(),
                    recording_count,
                });

                // Actualizar timestamp más reciente si encontramos archivos
                if recording_count > 0 {
                    if let Ok(metadata) = tokio::fs::metadata(&path).await {
                        if let Ok(modified) = metadata.modified() {
                            let dt = chrono::DateTime::<chrono::Utc>::from(modified);
                            if latest_timestamp.is_none() || dt > latest_timestamp.unwrap() {
                                latest_timestamp = Some(dt);
                            }
                        }
                    }
                }
            }
        }
    }

    // Actualizar el snapshot
    let mut snapshot = state.system.recording_snapshot.lock().await;
    snapshot.last_scan = Some(chrono::Utc::now());
    snapshot.total_count = total_count;
    snapshot.total_size_bytes = total_size_bytes;
    snapshot.day_summaries = day_summaries;
    snapshot.latest_timestamp = latest_timestamp;

    Ok(())
}
pub async fn storage_stream_sse() -> impl axum::response::IntoResponse {
    "OK"
}

/// SSE de metadatos de la grabación en curso
pub async fn current_recording_meta_sse(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<crate::AppState>>,
) -> impl axum::response::IntoResponse {
    use axum::body::Body;

    let mut rx = state.streaming.recording_meta_tx.subscribe();

    let stream = async_stream::stream! {
        // Enviar un ping inicial para abrir el stream
        yield Ok::<_, std::io::Error>(bytes::Bytes::from("event: open\n\n"));
        loop {
            match rx.recv().await {
                Ok(line) => {
                    let mut buf = String::new();
                    buf.push_str("data: ");
                    buf.push_str(&line);
                    buf.push_str("\n\n");
                    yield Ok(bytes::Bytes::from(buf));
                }
                Err(_) => break,
            }
        }
    };

    let body = Body::from_stream(stream);

    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-store, no-cache, must-revalidate, proxy-revalidate, max-age=0, no-transform")
        .header(header::PRAGMA, "no-cache")
        .header(header::EXPIRES, "0")
        .header("surrogate-control", "no-store")
        .header("x-accel-buffering", "no")
        .body(body)
        .unwrap()
}

/// SSE reactivo de grabaciones por día
pub async fn recordings_by_day_sse(
    axum::extract::Path(date): axum::extract::Path<String>,
    axum::extract::State(state): axum::extract::State<std::sync::Arc<crate::AppState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl axum::response::IntoResponse {
    use axum::body::Body;

    let storage_path = state.storage_path().clone();

    // Modo compatibilidad: si ?compat=1, enviar lista completa en cada cambio (sin eventos nombrados)
    let compat_mode = params.get("compat").map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(false);

    // Unificar tipo de stream en ambos branches
    use std::pin::Pin;
    type SseStream = Pin<Box<dyn futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send>>;

    let stream: SseStream = if compat_mode {
        Box::pin(async_stream::stream! {
            let mut last_payload = String::new();
            loop {
                let day_path = storage_path.join(&date);
                let list = if day_path.is_dir() {
                    crate::storage::list_recordings_in_day(&day_path, &date).await.unwrap_or_default()
                } else { Vec::new() };
                let json = serde_json::to_string(&list).unwrap_or("[]".to_string());
                if json != last_payload {
                    last_payload = json.clone();
                    let mut buf = String::new();
                    buf.push_str("data: ");
                    buf.push_str(&json);
                    buf.push_str("\n\n");
                    yield Ok::<_, std::io::Error>(bytes::Bytes::from(buf));
                }
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        })
    } else {
        Box::pin(async_stream::stream! {
            use std::collections::HashMap;

            // Estado previo indexado por path para calcular diffs
            let mut last_index: HashMap<String, crate::RecordingEntry> = HashMap::new();
            let tick = std::time::Duration::from_secs(3);

            // Helper para decidir si un registro cambió significativamente
            fn changed(a: &crate::RecordingEntry, b: &crate::RecordingEntry) -> bool {
                if a.size != b.size { return true; }
                if a.last_modified != b.last_modified { return true; }
                if a.is_live != b.is_live { return true; }
                if a.duration_source != b.duration_source { return true; }
                match (a.duration, b.duration) {
                    (Some(x), Some(y)) => (x - y).abs() >= 2.0, // evitar spam por pequeños incrementos
                    (None, None) => false,
                    _ => true,
                }
            }

            // Bucle principal de sondeo
            loop {
                let day_path = storage_path.join(&date);
                let list = if day_path.is_dir() {
                    crate::storage::list_recordings_in_day(&day_path, &date).await.unwrap_or_default()
                } else { Vec::new() };

                // Construir índice actual
                let mut cur_index: HashMap<String, crate::RecordingEntry> = HashMap::new();
                for rec in list.into_iter() {
                    cur_index.insert(rec.path.clone(), rec);
                }

                if last_index.is_empty() {
                    // Primer envío: snapshot completo
                    let snapshot_records: Vec<&crate::RecordingEntry> = cur_index.values().collect();
                    let payload = serde_json::json!({
                        "event": "snapshot",
                        "day": date,
                        "records": snapshot_records,
                    }).to_string();
                    let mut buf = String::new();
                    buf.push_str("event: snapshot\n");
                    buf.push_str("data: ");
                    buf.push_str(&payload);
                    buf.push_str("\n\n");
                    yield Ok::<_, std::io::Error>(bytes::Bytes::from(buf));

                    last_index = cur_index;
                    tokio::time::sleep(tick).await;
                    continue;
                }

                // Calcular diffs: added, removed, updated
                let mut added: Vec<&crate::RecordingEntry> = Vec::new();
                let mut removed: Vec<&crate::RecordingEntry> = Vec::new();
                let mut updated: Vec<&crate::RecordingEntry> = Vec::new();

                // Added y updated
                for (k, vnew) in cur_index.iter() {
                    if let Some(vold) = last_index.get(k) {
                        if changed(vold, vnew) { updated.push(vnew); }
                    } else {
                        added.push(vnew);
                    }
                }
                // Removed
                for (k, vold) in last_index.iter() {
                    if !cur_index.contains_key(k) {
                        removed.push(vold);
                    }
                }

                if !(added.is_empty() && removed.is_empty() && updated.is_empty()) {
                    let payload = serde_json::json!({
                        "event": "changes",
                        "day": date,
                        "added": added,
                        "removed": removed,
                        "updated": updated,
                    }).to_string();

                    let mut buf = String::new();
                    buf.push_str("event: changes\n");
                    buf.push_str("data: ");
                    buf.push_str(&payload);
                    buf.push_str("\n\n");
                    yield Ok::<_, std::io::Error>(bytes::Bytes::from(buf));
                }

                last_index = cur_index;
                tokio::time::sleep(tick).await;
            }
        })
    };

    let body = Body::from_stream(stream);

    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-store, no-cache, must-revalidate, proxy-revalidate, max-age=0, no-transform")
        .header(header::PRAGMA, "no-cache")
        .header(header::EXPIRES, "0")
        .header("surrogate-control", "no-store")
        .header("x-accel-buffering", "no")
        .body(body)
        .unwrap()
}

/// Streaming en vivo de grabaciones antiguas con soporte de formato
pub async fn stream_live_recording(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<crate::AppState>>,
    axum::extract::Path(path): axum::extract::Path<String>,
    axum::extract::Query(_params): axum::extract::Query<std::collections::HashMap<String, String>>,
    headers: axum::http::HeaderMap,
) -> impl axum::response::IntoResponse {
    // Determinar formato basado en la extensión del archivo
    let format = if path.ends_with(".mkv") {
        "video/x-matroska"
    } else if path.ends_with(".mp4") {
        "video/mp4"
    } else if path.ends_with(".avi") {
        "video/x-msvideo"
    } else {
        "application/octet-stream"
    };

    // Streaming continuo como si fuera en vivo
    stream_continuous_recording(state.storage.root.clone(), path, format, headers).await
}

/// Streaming continuo de grabaciones como si fueran en vivo
async fn stream_continuous_recording(
    storage_root: std::path::PathBuf,
    path: String,
    content_type: &str,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let req_start = std::time::Instant::now();
    log::info!("[recordings] stream request start path={} content_type={}", path, content_type);
    let full_path = storage_root.join(&path);

    if !full_path.exists() {
        return (StatusCode::NOT_FOUND, "Recording not found").into_response();
    }

    let file_size = match tokio::fs::metadata(&full_path).await {
        Ok(m) => m.len(),
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "Metadata error").into_response(),
    };

    // Verificar si es una petición range para streaming parcial
    if let Some(range) = headers.get("range") {
        log::info!("[recordings] range request detected: {}", range.to_str().unwrap_or("<invalid>"));
        let resp = handle_range_request(&full_path, range, file_size, content_type).await;
        log::info!("[recordings] range handler returning for path={} after {}ms", path, req_start.elapsed().as_millis());
        return resp;
    }

    // Buffer inicial inteligente para videos - enviar primeros segundos automáticamente
    // para iniciar la reproducción más rápido (similar a YouTube)
    let is_video = content_type.starts_with("video/");
    if is_video && file_size > 0 {
        // Estimar buffer inicial inteligente:
        // - Para archivos pequeños (< 100MB): enviar 20% del archivo
        // - Para archivos medianos (100MB-1GB): enviar 10% o 100MB lo que sea menor
        // - Para archivos grandes (>1GB): enviar 5% o 200MB lo que sea menor
        let initial_buffer_bytes = if file_size < 100 * 1024 * 1024 {
            file_size / 5 // 20% para archivos pequeños
        } else if file_size < 1024 * 1024 * 1024 {
            std::cmp::min(file_size / 10, 100 * 1024 * 1024) // 10% o 100MB
        } else {
            std::cmp::min(file_size / 20, 200 * 1024 * 1024) // 5% o 200MB
        };

        let initial_range = format!("bytes=0-{}", initial_buffer_bytes - 1);
        log::info!("[recordings] sending smart initial buffer for video: {} bytes ({:.1}%)",
                   initial_buffer_bytes, (initial_buffer_bytes as f64 / file_size as f64) * 100.0);

        // Crear range header artificial para el buffer inicial
        let mut range_headers = headers.clone();
        range_headers.insert("range", initial_range.parse().unwrap());

        let resp = handle_range_request(&full_path, range_headers.get("range").unwrap(), file_size, content_type).await;
        log::info!("[recordings] initial buffer sent for path={} after {}ms", path, req_start.elapsed().as_millis());
        return resp;
    }

    // Permitir streaming de grabaciones activas - el código ahora maneja archivos en grabación
    // If MP4 and the quick MP4 probe didn't find a 'moov' atom up front,
    // try to remux on-the-fly with ffmpeg into a fragmented MP4 so browsers
    // can start playback immediately. If ffmpeg is not available or fails,
    // fallback to streaming the file as-is.
    let try_remux = full_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("mp4"))
        .unwrap_or(false)
        && probe_mp4_duration_quick(&full_path).is_none();

        if try_remux {
            log::info!("[recordings] remux candidate (mp4 with moov at end) for path={}", path);
            // Attempt to spawn ffmpeg to remux to fragmented MP4 streamed to stdout
            let fp = full_path.to_string_lossy().to_string();
            let ffmpeg_cmd = Command::new("ffmpeg")
                .arg("-hide_banner")
                .arg("-loglevel")
                .arg("error")
                .arg("-i")
                .arg(&fp)
                .arg("-c")
                .arg("copy")
                .arg("-movflags")
                .arg("frag_keyframe+empty_moov+faststart+default_base_moof")
                .arg("-f")
                .arg("mp4")
                .arg("-avoid_negative_ts")
                .arg("make_zero")
                .arg("-fflags")
                .arg("+discardcorrupt+genpts")
                .arg("-")
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn();

            match ffmpeg_cmd {
                Ok(mut child) => {
                    log::info!("[recordings] ffmpeg spawned pid={} for path={}", child.id().unwrap_or(0), path);
                    if let Some(stdout) = child.stdout.take() {
                    use axum::body::Body;
                    use tokio_util::io::ReaderStream;

                    let stream = ReaderStream::new(stdout);
                    let body = Body::from_stream(stream);

                    // Note: we don't set Content-Length because this will be chunked
                    let mut builder = axum::response::Response::builder();
                    builder = builder.status(StatusCode::OK);
                    builder = builder.header(header::CONTENT_TYPE, "video/mp4");
                    // Fallback CORS header in case the global CORS layer is not applied
                    builder = builder.header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*");

                    // Headers optimizados para streaming MP4 remuxeado
                    builder = builder.header(
                        header::CACHE_CONTROL,
                        "public, max-age=3600, stale-while-revalidate=86400",
                    );
                    builder = builder.header(header::PRAGMA, "no-cache");
                    builder = builder.header(header::EXPIRES, "0");
                    builder = builder.header("x-accel-buffering", "no");
                    builder = builder.header(header::TRANSFER_ENCODING, "chunked");
                    builder = builder.header("X-Recording-Remuxed", "true");
                    builder = builder.header("Link", "</>; rel=\"preload\"; as=\"video\"");

                    // Return the response streaming ffmpeg stdout. The child process
                    // will be automatically terminated when stdout closes; if the
                    // client disconnects early the child may remain -- OS will
                    // reap when process exits; acceptable for now.
                    return builder.body(body).unwrap();
                    } else {
                        log::warn!("[recordings] ffmpeg started but stdout not piped for path={}", path);
                        let _ = child.kill().await;
                    }
                }
                Err(e) => {
                    log::warn!("[recordings] ffmpeg spawn failed for path={} err={}", path, e);
                }
            }
            // If we reach here, remux attempt failed; fallthrough to normal streaming
        }

        match File::open(&full_path).await {
            Ok(file) => {
                use axum::body::Body;
                use tokio_util::io::ReaderStream;

                let stream = ReaderStream::new(file);
                let body = Body::from_stream(stream);
                let mut builder = axum::response::Response::builder();
                builder = builder.status(StatusCode::OK);
                builder = builder.header(header::CONTENT_TYPE, content_type);
                // Fallback CORS header in case the global CORS layer is not applied
                builder = builder.header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*");
                builder = builder.header(header::CONTENT_LENGTH, file_size.to_string());

                // Headers optimizados para streaming rápido como YouTube
                if content_type.starts_with("video/") {
                    // Para videos: permitir cache agresivo pero con revalidación
                    builder = builder.header(
                        header::CACHE_CONTROL,
                        "public, max-age=3600, stale-while-revalidate=86400",
                    );
                    // Indicar al navegador que haga preload del contenido
                    builder = builder.header("X-Accel-Buffering", "no");
                    builder = builder.header("Link", "</>; rel=\"preload\"; as=\"video\"");
                } else {
                    // Para otros tipos: no cache para asegurar frescura
                    builder = builder.header(
                        header::CACHE_CONTROL,
                        "no-store, no-cache, must-revalidate, proxy-revalidate, max-age=0, no-transform",
                    );
                    builder = builder.header("Surrogate-Control", "no-store");
                    builder = builder.header("x-accel-buffering", "no");
                }

                builder = builder.header(header::PRAGMA, "no-cache");
                builder = builder.header(header::EXPIRES, "0");
                builder = builder.header(header::ACCEPT_RANGES, "bytes");

                let resp = builder.body(body).unwrap();
                log::info!("[recordings] serving file path={} size={} took={}ms", path, file_size, req_start.elapsed().as_millis());
                return resp;
            }
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "File open error").into_response(),
        }
}

/// Verificar si un archivo está siendo grabado actualmente
/// (considera que archivos modificados en los últimos 10 minutos están en grabación)
async fn is_file_being_recorded(file_path: &std::path::Path) -> bool {
    match tokio::fs::metadata(file_path).await {
        Ok(metadata) => {
            if let Ok(modified) = metadata.modified() {
                let now = std::time::SystemTime::now();
                if let Ok(duration) = now.duration_since(modified) {
                    // Cambiado a 10 minutos (600 segundos) para evitar streaming de archivos activos
                    duration.as_secs() < 600
                } else {
                    false
                }
            } else {
                false
            }
        }
        Err(_) => false,
    }
}

/// Manejar peticiones range para streaming parcial (seek/scrubbing)
async fn handle_range_request(
    full_path: &std::path::Path,
    range: &axum::http::HeaderValue,
    file_size: u64,
    content_type: &str,
) -> axum::response::Response {
    let range_str = range.to_str().unwrap_or("");
    if let Some(start_end) = range_str.strip_prefix("bytes=") {
        let parts: Vec<&str> = start_end.split('-').collect();
        if parts.len() == 2 {
            let start: u64 = parts[0].parse().unwrap_or(0);
            // Validar que el inicio esté dentro del tamaño actual
            if start >= file_size {
                return (StatusCode::RANGE_NOT_SATISFIABLE, "Invalid range").into_response();
            }
            let mut end: u64 = parts[1].parse().unwrap_or(file_size.saturating_sub(1));
            // Asegurar que el final no exceda el tamaño actual
            end = std::cmp::min(end, file_size.saturating_sub(1));
            let content_length = end.saturating_sub(start) + 1;

            // Limit range size to prevent memory exhaustion (max 10MB per range request)
            const MAX_RANGE_SIZE: u64 = 50 * 1024 * 1024; // 50MB - aumentado para mejor buffering
            // Serve a minimum chunk size to avoid many tiny range requests from the client
            const MIN_RANGE_RESPONSE: u64 = 256 * 1024; // 256KB

            // If client requested a very small range, expand it to MIN_RANGE_RESPONSE
            let expanded_content_length = if content_length < MIN_RANGE_RESPONSE {
                let max_possible = file_size.saturating_sub(start);
                let expanded = std::cmp::min(MIN_RANGE_RESPONSE, max_possible);
                log::debug!(
                    "[recordings] expanding small range request start={} requested={} -> expanded={}",
                    start,
                    content_length,
                    expanded
                );
                expanded
            } else {
                content_length
            };

            let actual_content_length = std::cmp::min(expanded_content_length, MAX_RANGE_SIZE);

            let mut file = match File::open(full_path).await {
                Ok(f) => f,
                Err(_) => {
                    return (StatusCode::INTERNAL_SERVER_ERROR, "File open error").into_response()
                }
            };

            file.seek(std::io::SeekFrom::Start(start)).await.unwrap();

            // Crear un stream para el range en lugar de leer todo en memoria
            let stream = async_stream::stream! {
                let mut remaining = actual_content_length;
                let mut buf = vec![0u8; 256 * 1024]; // 256KB chunks para mejor responsividad

                while remaining > 0 {
                    let chunk_len = std::cmp::min(remaining, buf.len() as u64) as usize;
                    match file.read_exact(&mut buf[..chunk_len]).await {
                        Ok(_) => {
                            yield Ok::<_, std::io::Error>(bytes::Bytes::copy_from_slice(&buf[..chunk_len]));
                            remaining -= chunk_len as u64;
                        }
                        Err(e) => {
                            log::warn!("range_stream: read error: {}", e);
                            break;
                        }
                    }
                }
            };

            let body = axum::body::Body::from_stream(stream);
            let mut response = axum::response::Response::new(body);
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, content_type.parse().unwrap());
            // Fallback CORS header in case the global CORS layer is not applied
            response
                .headers_mut()
                .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*".parse().unwrap());
            response.headers_mut().insert(
                header::CONTENT_LENGTH,
                actual_content_length.to_string().parse().unwrap(),
            );
            response
                .headers_mut()
                .insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                format!("bytes {}-{}/{}", start, start + actual_content_length - 1, file_size)
                    .parse()
                    .unwrap(),
            );

            // Headers optimizados para range requests
            if content_type.starts_with("video/") {
                response.headers_mut().insert(
                    header::CACHE_CONTROL,
                    "public, max-age=3600, stale-while-revalidate=86400".parse().unwrap(),
                );
                response.headers_mut().insert("x-accel-buffering", "no".parse().unwrap());
                response.headers_mut().insert("Link", "</>; rel=\"preload\"; as=\"video\"".parse().unwrap());
            } else {
                response.headers_mut().insert(
                    header::CACHE_CONTROL,
                    "no-store, no-cache, must-revalidate, proxy-revalidate, max-age=0, no-transform".parse().unwrap(),
                );
                response.headers_mut().insert("surrogate-control", "no-store".parse().unwrap());
                response.headers_mut().insert("x-accel-buffering", "no".parse().unwrap());
            }

            response
                .headers_mut()
                .insert(header::PRAGMA, "no-cache".parse().unwrap());
            response
                .headers_mut()
                .insert(header::EXPIRES, "0".parse().unwrap());
            *response.status_mut() = StatusCode::PARTIAL_CONTENT;
            return response;
        }
    }

    // Si no hay range válido, devolver error
    (StatusCode::RANGE_NOT_SATISFIABLE, "Invalid range").into_response()
}

pub fn init_recordings_db(
    _db_path: &std::path::PathBuf,
) -> Result<rusqlite::Connection, Box<dyn std::error::Error + Send + Sync>> {
    Ok(rusqlite::Connection::open_in_memory()?)
}
