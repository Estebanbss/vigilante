//! Módulo de métricas para Vigilante.
//!
//! Recopila y expone métricas de rendimiento.

pub mod depends;

pub use depends::collectors::MetricsCollector;

/// Manager principal para métricas.
#[derive(Clone)]
pub struct MetricsManager {
    // Aquí iría la lógica del manager
}

impl MetricsManager {
    pub fn new() -> Self {
        Self {}
    }

    pub fn gather_metrics(&self) -> Result<String, Box<dyn std::error::Error>> {
        MetricsCollector::gather()
    }
}

impl Default for MetricsManager {
    fn default() -> Self {
        Self::new()
    }
}

pub fn gather_metrics() -> Result<String, Box<dyn std::error::Error>> {
    MetricsCollector::gather()
}
