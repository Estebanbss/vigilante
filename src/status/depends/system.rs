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

        // Si el pipeline está activo, la cámara funciona
        if pipeline_active {
            return true;
        }

        // Si no hay pipeline activo, verificar si hay grabaciones recientes
        // Esto indica que la cámara funcionó recientemente
        let snapshot = state.system.recording_snapshot.lock().await.clone();
        let now = chrono::Utc::now();
        let recent_threshold = chrono::Duration::minutes(30); // Últimos 30 minutos

        if let Some(latest_ts) = snapshot.latest_timestamp {
            let time_diff = now.signed_duration_since(latest_ts);
            time_diff <= recent_threshold
        } else {
            false
        }
    }

    async fn check_audio(&self, state: &Arc<AppState>) -> bool {
        // Verificar si hay datos de audio disponibles actualmente
        let audio_available = {
            let audio_lock = state.streaming.audio_available.lock().unwrap();
            *audio_lock
        };

        if audio_available {
            return true;
        }

        // Si no hay audio disponible actualmente, verificar si hay grabaciones recientes
        // Esto indica que el audio funcionó recientemente (ya que las grabaciones incluyen audio)
        let snapshot = state.system.recording_snapshot.lock().await.clone();
        let now = chrono::Utc::now();
        let recent_threshold = chrono::Duration::minutes(30); // Últimos 30 minutos

        if let Some(latest_ts) = snapshot.latest_timestamp {
            let time_diff = now.signed_duration_since(latest_ts);
            time_diff <= recent_threshold
        } else {
            false
        }
    }

    async fn check_recordings(&self, state: &Arc<AppState>) -> bool {
        // Verificar si hay grabaciones recientes
        let snapshot = state.system.recording_snapshot.lock().await.clone();

        // Considerar que funciona si hay al menos una grabación
        if snapshot.total_count > 0 {
            let now = chrono::Utc::now();
            let recent_threshold = chrono::Duration::hours(24); // Últimas 24 horas

            if let Some(latest_ts) = snapshot.latest_timestamp {
                let time_diff = now.signed_duration_since(latest_ts);
                return time_diff <= recent_threshold;
            }
        }

        false
    }
}