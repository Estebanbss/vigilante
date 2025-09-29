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
use gstreamer as gst;

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
    gst::init()?;

    let detector = Arc::new(depends::motion::MotionDetector::new(state.clone()));
    let mut pipeline = depends::ffmpeg::CameraPipeline::new(state.clone(), Arc::clone(&detector));
    pipeline.warm_up().await?;
    pipeline.start_recording().await?;

    let streamer = depends::mjpeg::MjpegStreamer::new(state.clone());
    streamer.start_streaming().await?;

    // Loop for any additional processing, but motion is handled in pipeline
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    }
}