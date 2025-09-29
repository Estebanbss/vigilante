//! Detector de movimiento para análisis de video.
//!
//! Procesa frames para detectar cambios y activar alertas.

use crate::AppState;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use chrono;
use tokio::io::AsyncWriteExt;
use crate::metrics::depends::collectors::MOTION_EVENTS_TOTAL;
use crate::error::VigilanteError;

pub struct MotionDetector {
    pub log_path: std::path::PathBuf,
    pub context: Arc<AppState>,
    pub previous_frame: Arc<Mutex<Option<Vec<u8>>>>,
    pub log_writer: Arc<Mutex<Option<tokio::fs::File>>>,
}

impl MotionDetector {
    pub fn new(context: Arc<AppState>) -> Self {
        let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let log_path = context.storage_path().join(format!("{}.txt", date));
        Self {
            log_path,
            context,
            previous_frame: Arc::new(Mutex::new(None)),
            log_writer: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn start_detection(&self, _interval: Duration) -> Result<(), VigilanteError> {
        // Lógica para iniciar detección de movimiento
        Ok(())
    }

    pub async fn detect_motion(&self, frame: &[u8]) -> bool {
        let mut prev = self.previous_frame.lock().await;
        let motion = if let Some(ref prev_frame) = *prev {
            let diff: f64 = frame.iter().zip(prev_frame.iter()).map(|(a, b)| (a.abs_diff(*b) as f64).powi(2)).sum();
            let mse = diff / frame.len() as f64;
            mse.sqrt() > 10.0 // threshold
        } else {
            false
        };
        *prev = Some(frame.to_vec());

        if motion {
            // Escribir al log
            use std::fs::OpenOptions;
            let mut writer = self.log_writer.lock().await;
            if writer.is_none() {
                let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
                let dir = self.context.storage_path().join(&date);
                std::fs::create_dir_all(&dir).unwrap();
                let log_path = dir.join(format!("log_{}.txt", date));
                let file = OpenOptions::new().create(true).append(true).open(log_path).unwrap();
                *writer = Some(file.into());
            }
            if let Some(ref mut w) = *writer {
                let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                let log_entry = format!("{} - Movimiento detectado 🚶\n", now);
                w.write_all(log_entry.as_bytes()).await.unwrap();
            }

            // Incrementar métrica de motion events
            MOTION_EVENTS_TOTAL.inc();
        }

        motion
    }
}