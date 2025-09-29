//! Módulo de cámara para Vigilante.
//!
//! Proporciona una API ligera para gestión de pipelines de cámara,
//! delegando la lógica pesada a los submódulos en `depends/`.

pub mod depends;

pub use depends::ffmpeg::CameraPipeline;
pub use depends::mjpeg::MjpegStreamer;
pub use depends::motion::MotionDetector;
pub use depends::utils::CameraUtils;

use std::sync::Arc;
use crate::AppState;

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

pub async fn start_camera_pipeline(_camera_rtsp_url: String, state: Arc<AppState>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Set pipeline running flag immediately when task starts
    log::info!("🔧 Camera pipeline task started, setting pipeline_running=true");
    *state.gstreamer.pipeline_running.lock().unwrap() = true;
    log::info!("🔧 Pipeline running flag set at task start");

    let detector = Arc::new(depends::motion::MotionDetector::new(state.clone()));
    let mut pipeline = depends::ffmpeg::CameraPipeline::new(state.clone(), Arc::clone(&detector));
    pipeline.warm_up().await?;
    pipeline.start_recording().await?;

    let streamer = depends::mjpeg::MjpegStreamer::new(state.clone());
    streamer.start_streaming().await?;

    // Mark audio as available (for now, since camera pipeline indicates system is working)
    log::info!("🔧 About to set audio available flag");
    *state.streaming.audio_available.lock().unwrap() = true;
    *state.streaming.last_audio_timestamp.lock().unwrap() = Some(std::time::Instant::now());
    log::info!("🔧 Audio available flag set to true");

    // Loop for any additional processing, but motion is handled in pipeline
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    }
}