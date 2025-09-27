use prometheus::{Encoder, Histogram, IntCounter, IntGauge, TextEncoder, register_histogram, register_int_counter, register_int_gauge};
use lazy_static::lazy_static;

lazy_static! {
    // Histograma para la duración del refresco del snapshot
    pub static ref DURACION_REFRESCO_SNAPSHOT: Histogram = register_histogram!(
        "vigilante_duracion_refresco_snapshot_segundos",
        "Duración de las operaciones de refresco del snapshot de grabaciones"
    ).expect("No se pudo crear el histograma DURACION_REFRESCO_SNAPSHOT");

    // Histograma para las operaciones de concatenación FFmpeg
    pub static ref DURACION_CONCAT_FFMPEG: Histogram = register_histogram!(
        "vigilante_duracion_concat_ffmpeg_segundos",
        "Duración de las operaciones de concatenación con FFmpeg"
    ).expect("No se pudo crear el histograma DURACION_CONCAT_FFMPEG");

    // Contadores para operaciones exitosas
    pub static ref REFRESCO_SNAPSHOT_EXITOSO: IntCounter = register_int_counter!(
        "vigilante_refresco_snapshot_exitoso_total",
        "Número total de refrescos de snapshot exitosos"
    ).expect("No se pudo crear el contador REFRESCO_SNAPSHOT_EXITOSO");

    pub static ref CONCAT_FFMPEG_EXITOSO: IntCounter = register_int_counter!(
        "vigilante_concat_ffmpeg_exitoso_total",
        "Número total de concatenaciones FFmpeg exitosas"
    ).expect("No se pudo crear el contador CONCAT_FFMPEG_EXITOSO");

    // Contadores para operaciones fallidas
    pub static ref REFRESCO_SNAPSHOT_FALLIDO: IntCounter = register_int_counter!(
        "vigilante_refresco_snapshot_fallido_total",
        "Número total de refrescos de snapshot fallidos"
    ).expect("No se pudo crear el contador REFRESCO_SNAPSHOT_FALLIDO");

    pub static ref CONCAT_FFMPEG_FALLIDO: IntCounter = register_int_counter!(
        "vigilante_concat_ffmpeg_fallido_total",
        "Número total de concatenaciones FFmpeg fallidas"
    ).expect("No se pudo crear el contador CONCAT_FFMPEG_FALLIDO");

    // Contadores para timeouts y reintentos de ffmpeg
    pub static ref FFMPEG_TIMEOUT: IntCounter = register_int_counter!(
        "vigilante_ffmpeg_timeout_total",
        "Número total de timeouts en procesos ffmpeg"
    ).expect("No se pudo crear el contador FFMPEG_TIMEOUT");

    pub static ref FFMPEG_REINTENTO: IntCounter = register_int_counter!(
        "vigilante_ffmpeg_reintento_total",
        "Número total de reintentos de procesos ffmpeg"
    ).expect("No se pudo crear el contador FFMPEG_REINTENTO");

    // Gauge para el conteo actual de archivos en el snapshot
    pub static ref CONTEO_ARCHIVOS_SNAPSHOT: IntGauge = register_int_gauge!(
        "vigilante_conteo_archivos_snapshot",
        "Número actual de archivos en el snapshot de grabaciones"
    ).expect("No se pudo crear el gauge CONTEO_ARCHIVOS_SNAPSHOT");
}

/// Gather all metrics and encode them in Prometheus format
pub fn gather_metrics() -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    let mut buffer = Vec::new();
    encoder.encode(&metric_families, &mut buffer)?;
    Ok(String::from_utf8(buffer)?)
}