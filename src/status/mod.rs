//! Módulo de estado del sistema para Vigilante.
//!
//! Proporciona información sobre el estado de componentes.

pub mod depends;

use crate::AppState;
use axum::{extract::State, response::Json};
pub use depends::system::SystemStatus;
use std::sync::Arc;

/// Manager principal para estado.
#[derive(Clone)]
pub struct StatusManager {
    // Aquí iría la lógica del manager
}

impl StatusManager {
    pub fn new() -> Self {
        Self {}
    }

    pub async fn get_status(&self, state: &Arc<AppState>) -> SystemStatus {
        let mut status = SystemStatus::default();
        status.check(state).await;

        // Intentar autoreparación automática si algún componente está fallando
        let any_failing = !status.camera_works || !status.audio_works || !status.recordings_work;
        if any_failing {
            log::warn!("🔧 Detectados componentes fallando, iniciando autoreparación...");
            let repaired = status.auto_repair(state).await;
            if repaired {
                log::info!("✅ Autoreparación completada, verificando status actualizado...");
                // Verificar el status nuevamente después de la reparación
                status.check(state).await;
            } else {
                log::error!("❌ Autoreparación fallida, status permanece degradado");
            }
        }

        status
    }
}

impl Default for StatusManager {
    fn default() -> Self {
        Self::new()
    }
}

pub async fn get_system_status(
    State(state): State<Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    let manager = StatusManager::new();
    let status = manager.get_status(&state).await;
    Json(status)
}
