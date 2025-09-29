//! Módulo de estado del sistema para Vigilante.
//!
//! Proporciona información sobre el estado de componentes.

pub mod depends;

pub use depends::system::SystemStatus;
use crate::AppState;
use axum::{extract::State, response::Json};
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