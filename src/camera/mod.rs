//! Módulo de cámara para Vigilante.
//!
//! Proporciona una API ligera para gestión de pipelines de cámara,
//! delegando la lógica pesada a los submódulos en `depends/`.

pub mod depends;

pub use depends::ffmpeg::CameraPipeline;
pub use depends::mjpeg::MjpegStreamer;
pub use depends::motion::MotionDetector;
pub use depends::utils::CameraUtils;

use crate::AppState;
use gstreamer as gst;
use gstreamer::prelude::*;
use std::sync::Arc;

/// Manager principal para la cámara.
#[derive(Clone)]
pub struct CameraManager {
    // Aquí iría la lógica del manager
}

impl CameraManager {
    pub fn new() -> Self {
        Self {}
    }

    pub async fn start_pipeline(&self) {
        // Lógica para iniciar el pipeline
    }
}

impl Default for CameraManager {
    fn default() -> Self {
        Self::new()
    }
}

pub async fn start_camera_pipeline(
    _camera_rtsp_url: String,
    state: Arc<AppState>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Set pipeline running flag immediately when task starts
    log::info!("🔧 Camera pipeline task started, warming up");
    // *state.gstreamer.pipeline_running.lock().unwrap() = true;
    // log::info!("🔧 Pipeline running flag set at task start");

    // Create motion detector
    let motion_detector = Arc::new(MotionDetector::new(Arc::clone(&state)));

    // Create and setup camera pipeline using the new logic
    let mut camera_pipeline_inner = CameraPipeline::new(Arc::clone(&state), Arc::clone(&motion_detector));

    // Warm up the pipeline (create elements and link them)
    match camera_pipeline_inner.warm_up().await {
        Ok(_) => log::info!("🔧 Pipeline warmed up successfully"),
        Err(e) => {
            log::error!("❌ Failed to warm up pipeline: {}", e);
            *state.gstreamer.pipeline_running.lock().unwrap() = false;
            return Err(Box::new(e));
        }
    }

    // Now wrap in Arc
    let camera_pipeline = Arc::new(camera_pipeline_inner);
    *state.camera_pipeline.lock().unwrap() = Some(Arc::clone(&camera_pipeline));

    // Start recording (set pipeline to PLAYING state)
    match camera_pipeline.start_recording().await {
        Ok(_) => log::info!("🎬 Recording started successfully"),
        Err(e) => {
            log::error!("❌ Failed to start recording: {}", e);
            *state.gstreamer.pipeline_running.lock().unwrap() = false;
            return Err(e.into());
        }
    }

    // Now set the flag
    *state.gstreamer.pipeline_running.lock().unwrap() = true;
    log::info!("🔧 Pipeline running flag set after successful start");

    // Keep the task alive with health checks
    log::info!("🔧 Camera pipeline task running, monitoring pipeline...");
    let mut last_health_check = std::time::Instant::now();
    let last_mjpeg_frame = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));

    // Spawn a task to monitor MJPEG frames
    let state_clone = Arc::clone(&state);
    let last_mjpeg_frame_clone = Arc::clone(&last_mjpeg_frame);
    tokio::spawn(async move {
        let mut rx = state_clone.streaming.mjpeg_tx.subscribe();
        while let Ok(_) = rx.recv().await {
            *last_mjpeg_frame_clone.lock().unwrap() = std::time::Instant::now();
        }
    });

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;

        // Health check every 30 seconds
        if last_health_check.elapsed() >= std::time::Duration::from_secs(30) {
            last_health_check = std::time::Instant::now();

            // Check pipeline state
            if let Some(ref pipeline) = camera_pipeline.pipeline {
                // Get current state - pipeline.state() returns (result, current_state, pending_state)
                let (_, current_state, _) = pipeline.state(gst::ClockTime::NONE);
                match current_state {
                    gst::State::Playing => {
                        log::debug!("✅ Pipeline health check: OK (Playing)");
                    }
                    pipeline_state => {
                        log::warn!(
                            "⚠️ Pipeline health check: State is {:?}, expected Playing",
                            pipeline_state
                        );
                        // Try to restart pipeline
                        log::info!("🔄 Attempting to restart pipeline...");
                        if let Err(e) = pipeline.set_state(gst::State::Playing) {
                            log::error!("❌ Failed to restart pipeline: {}", e);
                            *state.gstreamer.pipeline_running.lock().unwrap() = false;
                            return Err(e.into());
                        }
                    }
                }
            } else {
                log::error!("❌ Pipeline reference lost during health check");
                *state.gstreamer.pipeline_running.lock().unwrap() = false;
                return Err("Pipeline reference lost".into());
            }

            // Check if MJPEG frames are still being received (within last 10 seconds)
            let time_since_last_frame = last_mjpeg_frame.lock().unwrap().elapsed();
            if time_since_last_frame > std::time::Duration::from_secs(10) {
                log::warn!(
                    "⚠️ No MJPEG frames received in {:.1}s - possible stream issue",
                    time_since_last_frame.as_secs_f64()
                );
            } else {
                log::debug!(
                    "📹 MJPEG stream active (last frame {:.1}s ago)",
                    time_since_last_frame.as_secs_f64()
                );
            }
        }
    }
}

pub async fn restart_camera_pipeline(
    state: Arc<AppState>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    log::info!("🔄 Reiniciando pipeline de cámara completamente...");

    // Detener el pipeline actual si existe
    {
        let mut pipeline_running = state.gstreamer.pipeline_running.lock().unwrap();
        if *pipeline_running {
            log::info!("🔧 Deteniendo pipeline actual...");
            *pipeline_running = false;
        }
    }

    // Limpiar el pipeline anterior
    {
        let mut camera_pipeline_guard = state.camera_pipeline.lock().unwrap();
        if let Some(ref pipeline_arc) = *camera_pipeline_guard {
            // Intentar detener el pipeline de GStreamer
            if let Some(ref gst_pipeline) = pipeline_arc.pipeline {
                let _ = gst_pipeline.set_state(gst::State::Null);
            }
        }
        *camera_pipeline_guard = None;
    }

    // Esperar un poco para que se liberen los recursos
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Crear motion detector
    let motion_detector = Arc::new(MotionDetector::new(Arc::clone(&state)));

    // Crear y configurar nuevo pipeline de cámara
    let mut camera_pipeline_inner = CameraPipeline::new(Arc::clone(&state), Arc::clone(&motion_detector));

    // Warm up the pipeline (crear elementos y enlazarlos)
    match camera_pipeline_inner.warm_up().await {
        Ok(_) => log::info!("🔧 Nuevo pipeline warmed up successfully"),
        Err(e) => {
            log::error!("❌ Failed to warm up new pipeline: {}", e);
            return Err(Box::new(e));
        }
    }

    // Envolver en Arc
    let camera_pipeline = Arc::new(camera_pipeline_inner);
    *state.camera_pipeline.lock().unwrap() = Some(Arc::clone(&camera_pipeline));

    // Iniciar grabación (poner pipeline en estado PLAYING)
    match camera_pipeline.start_recording().await {
        Ok(_) => log::info!("🎬 Nuevo pipeline de grabación iniciado successfully"),
        Err(e) => {
            log::error!("❌ Failed to start new recording: {}", e);
            return Err(e.into());
        }
    }

    // Marcar como corriendo
    *state.gstreamer.pipeline_running.lock().unwrap() = true;
    log::info!("✅ Pipeline de cámara reiniciado completamente");

    Ok(())
}
