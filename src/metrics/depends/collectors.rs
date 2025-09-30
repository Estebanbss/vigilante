//! Recopiladores de métricas.
//!
//! Define y registra métricas de Prometheus.

use lazy_static::lazy_static;
use prometheus::{register_counter, register_gauge, Counter, Encoder, Gauge, TextEncoder};

lazy_static! {
    pub static ref HTTP_REQUESTS_TOTAL: Counter = register_counter!(
        "vigilante_http_requests_total",
        "Total number of HTTP requests"
    )
    .unwrap();
    pub static ref HTTP_REQUESTS_DURATION: Counter = register_counter!(
        "vigilante_http_requests_duration_seconds_total",
        "Total duration of HTTP requests in seconds"
    )
    .unwrap();
    pub static ref HTTP_REQUESTS_ERRORS: Counter = register_counter!(
        "vigilante_http_requests_errors_total",
        "Total number of HTTP request errors"
    )
    .unwrap();
    pub static ref UPTIME_SECONDS: Gauge = register_gauge!(
        "vigilante_uptime_seconds",
        "Uptime of the service in seconds"
    )
    .unwrap();
    pub static ref MOTION_EVENTS_TOTAL: Counter = register_counter!(
        "vigilante_motion_events_total",
        "Total number of motion detection events"
    )
    .unwrap();
    pub static ref LIVE_LATENCY_EWMA_MS: Gauge = register_gauge!(
        "vigilante_live_latency_ewma_ms",
        "Latencia suavizada (EWMA) del streaming en vivo en milisegundos"
    )
    .unwrap();
    pub static ref LIVE_LATENCY_LAST_MS: Gauge = register_gauge!(
        "vigilante_live_latency_last_ms",
        "Latencia medida del último fragmento emitido en milisegundos"
    )
    .unwrap();
}

pub struct MetricsCollector;

impl MetricsCollector {
    pub fn init() {
        // Métricas ya registradas con lazy_static
    }

    pub fn increment_requests() {
        HTTP_REQUESTS_TOTAL.inc();
    }

    pub fn increment_errors() {
        HTTP_REQUESTS_ERRORS.inc();
    }

    pub fn record_duration(duration: f64) {
        HTTP_REQUESTS_DURATION.inc_by(duration);
    }

    pub fn set_uptime(seconds: f64) {
        UPTIME_SECONDS.set(seconds);
    }

    pub fn increment_motion_events() {
        MOTION_EVENTS_TOTAL.inc();
    }

    pub fn record_live_latency(latency_ms: f64, ewma_ms: Option<f64>) {
        LIVE_LATENCY_LAST_MS.set(latency_ms);
        if let Some(ewma) = ewma_ms {
            LIVE_LATENCY_EWMA_MS.set(ewma);
        }
    }

    pub fn gather() -> Result<String, Box<dyn std::error::Error>> {
        let encoder = TextEncoder::new();
        let metric_families = prometheus::gather();
        let mut buffer = Vec::new();
        encoder.encode(&metric_families, &mut buffer)?;
        Ok(String::from_utf8(buffer)?)
    }
}
