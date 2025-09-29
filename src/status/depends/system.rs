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
        // Check if pipeline is running
        let pipeline_running = { *state.gstreamer.pipeline_running.lock().unwrap() };

        log::info!(
            "🔍 Status check - Camera: pipeline_running={}",
            pipeline_running
        );

        // If the pipeline is running, the camera works
        if pipeline_running {
            return true;
        }

        // If not running, check for recent recordings
        let snapshot = state.system.recording_snapshot.lock().await.clone();
        let now = chrono::Utc::now();
        let recent_threshold = chrono::Duration::minutes(30);

        let has_recent_recording = if let Some(latest_ts) = snapshot.latest_timestamp {
            let time_diff = now.signed_duration_since(latest_ts);
            let is_recent = time_diff <= recent_threshold;
            log::info!(
                "🔍 Status check - Camera: latest_recording_ts={:?}, time_diff={:?}, is_recent={}",
                latest_ts,
                time_diff,
                is_recent
            );
            is_recent
        } else {
            log::info!("🔍 Status check - Camera: no latest timestamp found");
            false
        };

        has_recent_recording
    }

    async fn check_audio(&self, state: &Arc<AppState>) -> bool {
        // First check if pipeline is running - if so, audio should be available
        let pipeline_running = { *state.gstreamer.pipeline_running.lock().unwrap() };

        log::info!(
            "🔍 Status check - Audio: pipeline_running={}",
            pipeline_running
        );

        if pipeline_running {
            log::info!("🔍 Status check - Audio: pipeline is running, assuming audio works");
            return true;
        }

        // Verificar si hay datos de audio disponibles actualmente
        let audio_available = {
            let audio_lock = state.streaming.audio_available.lock().unwrap();
            *audio_lock
        };

        log::info!(
            "🔍 Status check - Audio: audio_available={}",
            audio_available
        );

        if audio_available {
            return true;
        }

        // Si no hay audio disponible actualmente, verificar si hay grabaciones recientes
        // Esto indica que el audio funcionó recientemente (ya que las grabaciones incluyen audio)
        let snapshot = state.system.recording_snapshot.lock().await.clone();
        let now = chrono::Utc::now();
        let recent_threshold = chrono::Duration::minutes(30); // Últimos 30 minutos

        let has_recent_recording = if let Some(latest_ts) = snapshot.latest_timestamp {
            let time_diff = now.signed_duration_since(latest_ts);
            let is_recent = time_diff <= recent_threshold;
            log::info!(
                "🔍 Status check - Audio: latest_recording_ts={:?}, time_diff={:?}, is_recent={}",
                latest_ts,
                time_diff,
                is_recent
            );
            is_recent
        } else {
            log::info!("🔍 Status check - Audio: no latest timestamp found");
            false
        };

        has_recent_recording
    }

    async fn check_recordings(&self, state: &Arc<AppState>) -> bool {
        // First check if pipeline is running - if so, recordings should be happening
        let pipeline_running = { *state.gstreamer.pipeline_running.lock().unwrap() };

        log::info!(
            "🔍 Status check - Recordings: pipeline_running={}",
            pipeline_running
        );

        if pipeline_running {
            log::info!(
                "🔍 Status check - Recordings: pipeline is running, assuming recordings work"
            );
            return true;
        }

        // If pipeline is not running, check snapshot for recent recordings
        let snapshot = state.system.recording_snapshot.lock().await.clone();
        log::info!(
            "🔍 Status check - Recordings: total_count={}, latest_timestamp={:?}",
            snapshot.total_count,
            snapshot.latest_timestamp
        );

        // Consider that works if there are recent recordings
        if snapshot.total_count > 0 {
            let now = chrono::Utc::now();
            let recent_threshold = chrono::Duration::hours(24); // Últimas 24 horas

            if let Some(latest_ts) = snapshot.latest_timestamp {
                let time_diff = now.signed_duration_since(latest_ts);
                let is_recent = time_diff <= recent_threshold;
                log::info!(
                    "🔍 Status check - Recordings: time_diff={:?}, is_recent={}",
                    time_diff,
                    is_recent
                );
                return is_recent;
            }
        }

        log::info!("🔍 Status check - Recordings: no recent recordings found");
        false
    }
}
