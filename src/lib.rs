pub mod auth;
pub mod camera;
pub mod logs;
pub mod ptz;
pub mod status;
pub mod storage;
pub mod stream;

use bytes::Bytes;
use std::{path::PathBuf, sync::Arc};
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

#[derive(Clone)]
pub struct AppState {
    pub camera_rtsp_url: String,
    pub camera_onvif_url: String,
    pub proxy_token: String,
    pub storage_path: PathBuf,
    pub pipeline: Arc<Mutex<Option<gst::Pipeline>>>,
    pub mjpeg_tx: broadcast::Sender<Bytes>,
    pub mjpeg_low_tx: broadcast::Sender<Bytes>,
    pub audio_mp3_tx: watch::Sender<Bytes>,
    pub audio_available: Arc<Mutex<bool>>,
    pub system_status: Arc<Mutex<SystemStatus>>,
    pub enable_hls: bool,
    // Detección manual de movimiento (cuando ONVIF no está disponible)
    pub enable_manual_motion_detection: bool,
    // Permite validar token por query (p.ej., ?token=...) en rutas de streaming
    pub allow_query_token_streams: bool,
    // Writer para logging de eventos de movimiento
    pub log_writer: Arc<Mutex<Option<std::io::BufWriter<std::fs::File>>>>,
    // Dominio base para bypass de autenticación (host exacto o subdominios). Ej: "nubellesalon.com"
    pub bypass_base_domain: Option<String>,
    // Secreto adicional para validar bypass (header X-Bypass-Secret). Si definido, se exige para bypass.
    pub bypass_domain_secret: Option<String>,
    // Timestamp de inicio de la aplicación para calcular uptime real
    pub start_time: std::time::SystemTime,
    // Canal para broadcast de status en tiempo real via WebSocket
    pub status_tx: broadcast::Sender<serde_json::Value>,
}
