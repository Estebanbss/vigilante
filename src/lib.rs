pub mod auth;
pub mod camera;
pub mod error;
pub mod logs;
pub mod metrics;
pub mod ptz;
pub mod state;
pub mod status;
pub mod storage;
pub mod stream;

use auth::AuthManager;

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex},
};
use tokio::sync::broadcast;

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

impl Default for NotificationManager {
    fn default() -> Self {
        Self::new()
    }
}

// Dependencias de GStreamer

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
    pub total_size_bytes: u64,
    pub latest_timestamp: Option<chrono::DateTime<chrono::Utc>>,
    pub latest_path: Option<String>,
    pub last_scanned_mtime: Option<std::time::SystemTime>,
}

#[derive(Debug)]
pub struct AppState {
    pub camera: state::CameraConfig,
    pub storage: state::StorageConfig,
    pub streaming: Arc<state::StreamingState>,
    pub logging: Arc<state::LoggingState>,
    pub system: Arc<state::SystemState>,
    pub auth: state::AuthState,
    pub gstreamer: Arc<state::GStreamerState>,
    pub camera_pipeline: Arc<StdMutex<Option<Arc<crate::camera::depends::ffmpeg::CameraPipeline>>>>,
    pub stream: Arc<stream::StreamManager>,
}

impl AppState {
    pub fn auth(&self) -> &AuthManager {
        &self.auth.manager
    }

    // Métodos de conveniencia para acceso directo a campos comunes
    pub fn camera_rtsp_url(&self) -> &str {
        &self.camera.rtsp_url
    }

    pub fn camera_onvif_url(&self) -> &str {
        &self.camera.onvif_url
    }

    pub fn proxy_token(&self) -> &str {
        &self.auth.proxy_token
    }

    pub fn storage_path(&self) -> &PathBuf {
        &self.storage.path
    }

    pub fn storage_root(&self) -> &PathBuf {
        &self.storage.root
    }

    pub fn db_conn(&self) -> &Arc<StdMutex<rusqlite::Connection>> {
        &self.storage.db_conn
    }

    pub fn log_tx(&self) -> &broadcast::Sender<String> {
        &self.logging.log_tx
    }
}
