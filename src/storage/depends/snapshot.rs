//! Gestión de snapshots de grabaciones.
//!
//! Crea y mantiene resúmenes de archivos disponibles.

use crate::RecordingSnapshot;
use std::time::{Duration, Instant};

pub struct SnapshotManager {
    ttl: Duration,
    last_update: Option<Instant>,
}

impl SnapshotManager {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            last_update: None,
        }
    }

    pub async fn refresh(&mut self) -> Result<RecordingSnapshot, String> {
        // Lógica para refrescar snapshot
        self.last_update = Some(Instant::now());
        Ok(RecordingSnapshot::default())
    }

    pub fn is_fresh(&self) -> bool {
        self.last_update
            .map(|t| t.elapsed() < self.ttl)
            .unwrap_or(false)
    }
}