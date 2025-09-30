//! Estados y configuraciones separados para AppState.
//!
//! Separa las responsabilidades de AppState en structs más pequeños y enfocados.

use crate::{AuthManager, NotificationManager, RecordingSnapshot};
use bytes::Bytes;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use tokio::sync::{broadcast, Mutex as TokioMutex};

/// Configuración de cámara
#[derive(Clone, Debug)]
pub struct CameraConfig {
    pub rtsp_url: String,
    pub onvif_url: String,
    pub enable_manual_motion_detection: bool,
}

/// Configuración de almacenamiento
#[derive(Clone, Debug)]
pub struct StorageConfig {
    pub path: PathBuf,
    pub root: PathBuf,
    pub db_conn: Arc<StdMutex<rusqlite::Connection>>,
}

/// Estado de streaming
#[derive(Debug)]
pub struct StreamingState {
    pub mjpeg_tx: broadcast::Sender<bytes::Bytes>,
    pub mjpeg_low_tx: broadcast::Sender<bytes::Bytes>,
    pub audio_mp3_tx: broadcast::Sender<bytes::Bytes>,
    pub audio_available: Arc<StdMutex<bool>>,
    pub last_audio_timestamp: Arc<StdMutex<Option<std::time::Instant>>>,
    pub mp4_init_segments: Arc<StdMutex<Vec<Bytes>>>,
    pub mp4_init_complete: Arc<StdMutex<bool>>,
    pub mp4_init_scan_tail: Arc<StdMutex<Vec<u8>>>,
}

/// Estado de logging
#[derive(Debug)]
pub struct LoggingState {
    pub log_tx: broadcast::Sender<String>,
    pub log_writer: Arc<StdMutex<Option<std::io::BufWriter<std::fs::File>>>>,
}

/// Estado del sistema
#[derive(Debug)]
pub struct SystemState {
    pub start_time: std::time::SystemTime,
    pub recording_snapshot: Arc<TokioMutex<RecordingSnapshot>>,
    pub notifications: Arc<NotificationManager>,
}

/// Estado de autenticación
#[derive(Clone, Debug)]
pub struct AuthState {
    pub manager: AuthManager,
    pub proxy_token: String,
    pub allow_query_token_streams: bool,
    pub bypass_base_domain: Option<String>,
    pub bypass_domain_secret: Option<String>,
}

/// Estado de GStreamer
#[derive(Debug)]
pub struct GStreamerState {
    pub pipeline_running: Arc<StdMutex<bool>>,
}
