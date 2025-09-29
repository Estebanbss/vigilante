//! Lectura de logs de motion detection.
//!
//! Lee logs diarios de detección de movimiento.

use std::path::Path;
use tokio::fs;

pub struct LogReader {
    storage_path: String,
}

impl LogReader {
    pub fn new(storage_path: String) -> Self {
        Self { storage_path }
    }

    pub async fn get_log_entries(&self, date: &str, limit: Option<usize>) -> Result<Vec<String>, String> {
        let log_path = Path::new(&self.storage_path).join(date).join(format!("{}.txt", date));
        if !log_path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&log_path).await.map_err(|e| format!("Failed to read log file: {}", e))?;
        let mut entries: Vec<String> = content.lines().map(|s| s.to_string()).collect();
        if let Some(lim) = limit {
            entries.truncate(lim);
        }
        Ok(entries)
    }
}