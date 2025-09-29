//! Recopiladores de métricas.
//!
//! Define y registra métricas de Prometheus.

use prometheus::{register_counter, register_gauge, Counter, Gauge, Encoder, TextEncoder};
use lazy_static::lazy_static;

lazy_static! {
    pub static ref HTTP_REQUESTS_TOTAL: Counter = register_counter!(
        "vigilante_http_requests_total",
        "Total number of HTTP requests"
    ).unwrap();

    pub static ref HTTP_REQUESTS_DURATION: Counter = register_counter!(
        "vigilante_http_requests_duration_seconds_total",
        "Total duration of HTTP requests in seconds"
    ).unwrap();

    pub static ref HTTP_REQUESTS_ERRORS: Counter = register_counter!(
        "vigilante_http_requests_errors_total",
        "Total number of HTTP request errors"
    ).unwrap();

    pub static ref UPTIME_SECONDS: Gauge = register_gauge!(
        "vigilante_uptime_seconds",
        "Uptime of the service in seconds"
    ).unwrap();

    pub static ref MOTION_EVENTS_TOTAL: Counter = register_counter!(
        "vigilante_motion_events_total",
        "Total number of motion detection events"
    ).unwrap();
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

    pub fn gather() -> Result<String, Box<dyn std::error::Error>> {
        let encoder = TextEncoder::new();
        let metric_families = prometheus::gather();
        let mut buffer = Vec::new();
        encoder.encode(&metric_families, &mut buffer)?;
        Ok(String::from_utf8(buffer)?)
    }
}