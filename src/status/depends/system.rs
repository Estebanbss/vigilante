//! Estado del sistema.
//!
//! Verifica componentes como cámara, audio, grabaciones.

use crate::AppState;
use std::sync::Arc;

#[derive(Clone, Debug, serde::Serialize, Default)]
pub struct SystemStatus {
    pub camera_works: bool,
    pub audio_works: bool,
    pub recordings_work: bool,
}

impl SystemStatus {
    pub async fn check(&mut self, state: &Arc<AppState>) {
        // Verificar estado de la cámara
        self.camera_works = self.check_camera(state).await;

        // Verificar estado del audio
        self.audio_works = self.check_audio(state).await;

        // Verificar estado de las grabaciones
        self.recordings_work = self.check_recordings(state).await;
    }

    async fn check_camera(&self, state: &Arc<AppState>) -> bool {
        // Verificar si el pipeline de GStreamer está activo
        let pipeline_active = {
            let pipeline_lock = state.gstreamer.pipeline.lock().await;
            pipeline_lock.is_some()
        };
        pipeline_active
    }

    async fn check_audio(&self, state: &Arc<AppState>) -> bool {
        // Verificar si el audio está disponible y tiene datos recientes
        let available = *state.streaming.audio_available.lock().unwrap();

        // Verificar si hay datos de audio recientes (últimos 30 segundos)
        let has_recent_data = if let Some(last_ts) = *state.streaming.last_audio_timestamp.lock().unwrap() {
            last_ts.elapsed() < std::time::Duration::from_secs(30)
        } else {
            false
        };

        available && has_recent_data
    }

    async fn check_recordings(&self, state: &Arc<AppState>) -> bool {
        // Verificar si hay grabaciones recientes
        let snapshot = state.system.recording_snapshot.lock().await.clone();

        // Considerar que funciona si hay al menos una grabación
        snapshot.total_count > 0
    }
}