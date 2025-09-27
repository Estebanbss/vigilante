pub mod auth;
pub mod camera;
pub mod logs;
pub mod ptz;
pub mod status;
pub mod storage;
pub mod stream;

use bytes::Bytes;
use std::sync::atomic::AtomicU64;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex},
};
use tokio::sync::{broadcast, watch, Mutex};

// Dependencias de GStreamer
use gstreamer as gst;

#[derive(Clone, Debug, serde::Serialize)]
pub struct SystemStatus {
    pub pipeline_status: PipelineStatus,
    pub audio_status: AudioStatus,
    pub storage_status: StorageStatus,
    pub uptime_seconds: u64,
    pub last_updated: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct PipelineStatus {
    pub is_running: bool,
    pub start_time: Option<chrono::DateTime<chrono::Utc>>,
    pub error_count: u32,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct AudioStatus {
    pub available: bool,
    pub bytes_sent: u64,
    pub packets_sent: u64,
    pub last_activity: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct StorageStatus {
    pub total_space_bytes: u64,
    pub used_space_bytes: u64,
    pub free_space_bytes: u64,
    pub recording_count: usize,
    pub last_recording: Option<String>,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct DaySummary {
    pub day: String,
    #[serde(rename = "length")]
    pub recording_count: usize,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct RecordingEntry {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub last_modified: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    #[serde(skip_serializing)]
    pub day: String,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct RecordingSnapshot {
    pub last_scan: Option<chrono::DateTime<chrono::Utc>>,
    pub total_count: usize,
    pub last_recording: Option<String>,
    pub day_summaries: Vec<DaySummary>,
    #[serde(skip_serializing)]
    pub latest_timestamp: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug)]
pub struct AppState {
    pub camera_rtsp_url: String,
    pub camera_onvif_url: String,
    pub proxy_token: String,
    pub storage_path: PathBuf,
    pub storage_root: PathBuf,
    pub pipeline: Arc<Mutex<Option<gst::Pipeline>>>,
    pub mjpeg_tx: broadcast::Sender<Bytes>,
    pub mjpeg_low_tx: broadcast::Sender<Bytes>,
    pub audio_mp3_tx: watch::Sender<Bytes>,
    pub audio_available: Arc<Mutex<bool>>,
    pub system_status: Arc<Mutex<SystemStatus>>,
    pub audio_bytes_sent: AtomicU64,
    pub audio_packets_sent: AtomicU64,
    pub enable_hls: bool,
    // Detección manual de movimiento (cuando ONVIF no está disponible)
    pub enable_manual_motion_detection: bool,
    // Permite validar token por query (p.ej., ?token=...) en rutas de streaming
    pub allow_query_token_streams: bool,
    // Writer para logging de eventos de movimiento
    pub log_writer: Arc<StdMutex<Option<std::io::BufWriter<std::fs::File>>>>,
    // Dominio base para bypass de autenticación (host exacto o subdominios). Ej: "nubellesalon.com"
    pub bypass_base_domain: Option<String>,
    // Secreto adicional para validar bypass (header X-Bypass-Secret). Si definido, se exige para bypass.
    pub bypass_domain_secret: Option<String>,
    // Timestamp de inicio de la aplicación para calcular uptime real
    pub start_time: std::time::SystemTime,
    // Caché de grabaciones y resúmenes para evitar escaneos costosos en cada request
    pub recording_snapshot: Arc<Mutex<RecordingSnapshot>>,
}
