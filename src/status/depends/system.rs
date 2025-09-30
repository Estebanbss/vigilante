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

    pub async fn auto_repair(&mut self, state: &Arc<AppState>) -> bool {
        let mut repaired_any = false;

        log::info!("🔧 Iniciando autoreparación del sistema...");

        // Intentar reparar cámara si está fallando
        if !self.camera_works {
            log::warn!("🔧 Intentando reparar componente de cámara...");
            if self.repair_camera(state).await {
                self.camera_works = true;
                repaired_any = true;
                log::info!("✅ Componente de cámara reparado exitosamente");
            } else {
                log::error!("❌ No se pudo reparar el componente de cámara");
            }
        }

        // Intentar reparar audio si está fallando
        if !self.audio_works {
            log::warn!("🔧 Intentando reparar componente de audio...");
            if self.repair_audio(state).await {
                self.audio_works = true;
                repaired_any = true;
                log::info!("✅ Componente de audio reparado exitosamente");
            } else {
                log::error!("❌ No se pudo reparar el componente de audio");
            }
        }

        // Intentar reparar grabaciones si está fallando
        if !self.recordings_work {
            log::warn!("🔧 Intentando reparar componente de grabaciones...");
            if self.repair_recordings(state).await {
                self.recordings_work = true;
                repaired_any = true;
                log::info!("✅ Componente de grabaciones reparado exitosamente");
            } else {
                log::error!("❌ No se pudo reparar el componente de grabaciones");
            }
        }

        if repaired_any {
            log::info!("🎉 Autoreparación completada exitosamente");
        } else {
            log::warn!("⚠️ No se pudo reparar ningún componente");
        }

        repaired_any
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

    async fn repair_camera(&self, state: &Arc<AppState>) -> bool {
        log::info!("🔧 Intentando reiniciar el pipeline de la cámara...");

        // Usar la función de reinicio completa del módulo de camera
        match crate::camera::restart_camera_pipeline(Arc::clone(state)).await {
            Ok(_) => {
                log::info!("✅ Pipeline de cámara reiniciado exitosamente");

                // Verificar que el pipeline esté corriendo después del reinicio
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                let pipeline_running = *state.gstreamer.pipeline_running.lock().unwrap();

                if pipeline_running {
                    log::info!("✅ Pipeline de cámara confirmado como activo");
                    return true;
                } else {
                    log::warn!("⚠️ Pipeline reiniciado pero no se confirmó como activo");
                    return false;
                }
            }
            Err(e) => {
                log::error!("❌ Error al reiniciar pipeline de cámara: {:?}", e);
                return false;
            }
        }
    }

    async fn repair_audio(&self, state: &Arc<AppState>) -> bool {
        log::info!("🔧 Intentando reparar componente de audio reiniciando pipeline...");

        // El audio depende del pipeline de GStreamer, reiniciar el pipeline completo
        match crate::camera::restart_camera_pipeline(Arc::clone(state)).await {
            Ok(_) => {
                log::info!("✅ Pipeline reiniciado para reparar audio");

                // Verificar que el audio esté disponible después del reinicio
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

                let audio_available = {
                    let audio_lock = state.streaming.audio_available.lock().unwrap();
                    *audio_lock
                };

                if audio_available {
                    log::info!("✅ Audio confirmado como disponible después del reinicio");
                    return true;
                } else {
                    log::warn!("⚠️ Pipeline reiniciado pero audio no disponible");
                    return false;
                }
            }
            Err(e) => {
                log::error!("❌ Error al reiniciar pipeline para audio: {:?}", e);
                return false;
            }
        }
    }

    async fn repair_recordings(&self, state: &Arc<AppState>) -> bool {
        log::info!("🔧 Intentando reparar componente de grabaciones reiniciando pipeline...");

        // Las grabaciones dependen del pipeline de GStreamer, reiniciar el pipeline completo
        match crate::camera::restart_camera_pipeline(Arc::clone(state)).await {
            Ok(_) => {
                log::info!("✅ Pipeline reiniciado para reparar grabaciones");

                // Verificar que las grabaciones estén funcionando después del reinicio
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

                // Verificar si hay una grabación reciente (últimos 5 minutos)
                let snapshot = state.system.recording_snapshot.lock().await.clone();
                let now = chrono::Utc::now();
                let recent_threshold = chrono::Duration::minutes(5);

                let has_recent_recording = if let Some(latest_ts) = snapshot.latest_timestamp {
                    let time_diff = now.signed_duration_since(latest_ts);
                    let is_recent = time_diff <= recent_threshold;
                    log::info!(
                        "🔍 Verificación post-reinicio - Grabaciones: latest_ts={:?}, time_diff={:?}, is_recent={}",
                        latest_ts,
                        time_diff,
                        is_recent
                    );
                    is_recent
                } else {
                    log::info!("🔍 Verificación post-reinicio - Grabaciones: no hay timestamp disponible aún");
                    // Si no hay timestamp, esperar un poco más y verificar de nuevo
                    tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                    let snapshot = state.system.recording_snapshot.lock().await.clone();
                    snapshot.latest_timestamp.is_some()
                };

                if has_recent_recording {
                    log::info!("✅ Grabaciones confirmadas como activas después del reinicio");
                    return true;
                } else {
                    log::warn!("⚠️ Pipeline reiniciado pero grabaciones no confirmadas");
                    return false;
                }
            }
            Err(e) => {
                log::error!("❌ Error al reiniciar pipeline para grabaciones: {:?}", e);
                return false;
            }
        }
    }
}
