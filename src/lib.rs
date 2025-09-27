pub mod auth;
pub mod camera;
pub mod logs;
pub mod metrics;
pub mod ptz;
pub mod status;
pub mod storage;
pub mod stream;

use bytes::Bytes;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex},
};
use tokio::sync::{broadcast, watch, Mutex};

// Evento para notificaciones de recordings (más eficiente que clonar todo el snapshot)
#[derive(Clone, Debug, serde::Serialize)]
pub struct RecordingsEvent {
    pub event_type: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub total_files: usize,
    pub total_size_bytes: u64,
    pub days_count: usize,
}

// Notification manager for robust channel handling
#[derive(Debug)]
pub struct NotificationManager {
    event_tx: broadcast::Sender<RecordingsEvent>,
    client_count: Arc<StdMutex<usize>>,
}

impl NotificationManager {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(100); // Buffer de 100 eventos
        Self {
            event_tx: tx,
            client_count: Arc::new(StdMutex::new(0)),
        }
    }

    pub fn send_snapshot(&self, snapshot: RecordingSnapshot) -> bool {
        let event = RecordingsEvent {
            event_type: "storage_snapshot".to_string(),
            timestamp: chrono::Utc::now(),
            total_files: snapshot.total_count,
            total_size_bytes: snapshot.total_size_bytes,
            days_count: snapshot.day_meta.len(),
        };

        match self.event_tx.send(event) {
            Ok(_) => true,
            Err(_) => {
                eprintln!("⚠️ Failed to send snapshot notification - no subscribers");
                false
            }
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<RecordingsEvent> {
        *self.client_count.lock().unwrap() += 1;
        self.event_tx.subscribe()
    }

    pub fn unsubscribe(&self) {
        let mut count = self.client_count.lock().unwrap();
        if *count > 0 {
            *count -= 1;
        }
    }

    pub fn client_count(&self) -> usize {
        *self.client_count.lock().unwrap()
    }

    pub fn is_active(&self) -> bool {
        true // Broadcast channels are always active
    }
}

// Dependencias de GStreamer
use gstreamer as gst;

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
    pub total_size_bytes: u64,
    pub last_recording: Option<String>,
    pub day_summaries: Vec<DaySummary>,
    #[serde(skip_serializing)]
    pub latest_timestamp: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing)]
    pub day_meta: HashMap<String, DayMeta>,
}

#[derive(Clone, Debug, Default)]
pub struct DayMeta {
    pub recording_count: usize,
    pub latest_timestamp: Option<chrono::DateTime<chrono::Utc>>,
    pub latest_path: Option<String>,
    pub last_scanned_mtime: Option<std::time::SystemTime>,
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
    pub audio_available: Arc<StdMutex<bool>>,
    // Timestamp del último audio recibido para detectar caídas
    pub last_audio_timestamp: Arc<StdMutex<Option<std::time::Instant>>>,
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
    pub notifications: Arc<NotificationManager>,
    // SQLite connection for persistent metadata cache
    pub db_conn: Arc<StdMutex<rusqlite::Connection>>,
}
