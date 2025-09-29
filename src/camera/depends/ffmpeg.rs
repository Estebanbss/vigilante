//! Pipeline de FFmpeg para captura de video.
//!
//! Maneja la configuración y ejecución del pipeline de GStreamer
//! para grabación continua de video.

use crate::AppState;
use crate::error::{Result, VigilanteError};
use std::sync::Arc;
use gstreamer as gst;
use gstreamer::prelude::*;
use bytes::Bytes;
use crate::camera::depends::motion::MotionDetector;
use tokio;

pub struct CameraPipeline {
    pub pipeline: Option<gst::Pipeline>,
    pub mjpeg_tx: tokio::sync::broadcast::Sender<Bytes>,
    pub motion_detector: Arc<MotionDetector>,
    pub context: Arc<AppState>,
}impl CameraPipeline {
    pub fn new(context: Arc<AppState>, motion_detector: Arc<MotionDetector>) -> Self {
        let mjpeg_tx = context.streaming.mjpeg_tx.clone();
        Self {
            pipeline: None,
            mjpeg_tx,
            motion_detector,
            context,
        }
    }

    pub async fn warm_up(&mut self) -> Result<()> {
        let pipeline = gst::Pipeline::new();

        // Source RTSP
        let source = gst::ElementFactory::make("rtspsrc")
            .build()
            .map_err(|e| VigilanteError::GStreamer(format!("Failed to create rtspsrc: {}", e)))?;
        source.set_property("location", self.context.camera_rtsp_url());

        // Decode
        let decode = gst::ElementFactory::make("decodebin")
            .build()
            .map_err(|e| VigilanteError::GStreamer(format!("Failed to create decodebin: {}", e)))?;

        // Tee for branching
        let tee = gst::ElementFactory::make("tee").build().map_err(|_| VigilanteError::GStreamer("Failed to create tee".to_string()))?;

        // MJPEG branch
        let queue_mjpeg = gst::ElementFactory::make("queue").build().map_err(|_| VigilanteError::GStreamer("Failed to create queue_mjpeg".to_string()))?;
        let videoconvert_mjpeg = gst::ElementFactory::make("videoconvert").build().map_err(|_| VigilanteError::GStreamer("Failed to create videoconvert_mjpeg".to_string()))?;
        let jpegenc = gst::ElementFactory::make("jpegenc").build().map_err(|_| VigilanteError::GStreamer("Failed to create jpegenc".to_string()))?;
        let appsink_mjpeg = gst::ElementFactory::make("appsink").build().map_err(|_| VigilanteError::GStreamer("Failed to create appsink_mjpeg".to_string()))?;
        appsink_mjpeg.set_property("emit-signals", true);

        // Recording branch
        let queue_rec = gst::ElementFactory::make("queue").build().map_err(|_| VigilanteError::GStreamer("Failed to create queue_rec".to_string()))?;
        let videoconvert_rec = gst::ElementFactory::make("videoconvert").build().map_err(|_| VigilanteError::GStreamer("Failed to create videoconvert_rec".to_string()))?;
        let x264enc = gst::ElementFactory::make("x264enc").build().map_err(|_| VigilanteError::GStreamer("Failed to create x264enc".to_string()))?;
        let mp4mux = gst::ElementFactory::make("matroskamux").build().map_err(|_| VigilanteError::GStreamer("Failed to create matroskamux".to_string()))?;
        mp4mux.set_property("writing-app", "vigilante"); // Opcional, para metadata
        let filesink = gst::ElementFactory::make("filesink").build().map_err(|_| VigilanteError::GStreamer("Failed to create filesink".to_string()))?;
        let timestamp = crate::camera::depends::utils::CameraUtils::format_timestamp();
        let date = timestamp.split('_').next().unwrap(); // YYYY-MM-DD
        let dir = self.context.storage_path().join(date);
        std::fs::create_dir_all(&dir).map_err(VigilanteError::Io)?;
        let filename = format!("{}.mkv", date);
        let path = dir.join(&filename);
        filesink.set_property("location", path.to_str().unwrap());

        // Log recording start
        log::info!("📹 Grabación iniciada: {}", path.display());

        // Add elements
        pipeline.add_many([&source, &decode, &tee, &queue_mjpeg, &videoconvert_mjpeg, &jpegenc, &appsink_mjpeg, &queue_rec, &videoconvert_rec, &x264enc, &mp4mux, &filesink]).map_err(|_| VigilanteError::GStreamer("Failed to add elements".to_string()))?;

        // Motion branch
        let queue_motion = gst::ElementFactory::make("queue").build().map_err(|_| VigilanteError::GStreamer("Failed to create queue_motion".to_string()))?;
        let videoconvert_motion = gst::ElementFactory::make("videoconvert").build().map_err(|_| VigilanteError::GStreamer("Failed to create videoconvert_motion".to_string()))?;
        let appsink_motion = gst::ElementFactory::make("appsink").build().map_err(|_| VigilanteError::GStreamer("Failed to create appsink_motion".to_string()))?;
        appsink_motion.set_property("emit-signals", true);
        appsink_motion.set_property("caps", gst::Caps::builder("video/x-raw").build());

        // Add motion elements
        pipeline.add_many([&queue_motion, &videoconvert_motion, &appsink_motion]).map_err(|_| VigilanteError::GStreamer("Failed to add motion elements".to_string()))?;
        gst::Element::link_many([&queue_motion, &videoconvert_motion, &appsink_motion]).map_err(|_| VigilanteError::GStreamer("Failed to link motion".to_string()))?;
        // Connect motion signal
        let detector_clone = Arc::clone(&self.motion_detector);
        appsink_motion.connect("new-sample", false, move |values| {
            let detector = Arc::clone(&detector_clone);
            let sample = values[1].get::<gst::Sample>().unwrap();
            let buffer = sample.buffer().unwrap();
            let map = buffer.map_readable().unwrap();
            let frame = map.as_slice().to_vec();
            tokio::spawn(async move {
                let _ = detector.detect_motion(&frame).await;
            });
            None
        });

        // Link static parts
        gst::Element::link_many([&queue_mjpeg, &videoconvert_mjpeg, &jpegenc, &appsink_mjpeg]).map_err(|_| VigilanteError::GStreamer("Failed to link MJPEG".to_string()))?;
        gst::Element::link_many([&queue_rec, &videoconvert_rec, &x264enc, &mp4mux, &filesink]).map_err(|_| VigilanteError::GStreamer("Failed to link recording".to_string()))?;

        // Connect MJPEG signal
        let context_weak = Arc::downgrade(&self.context);
        appsink_mjpeg.connect("new-sample", false, move |values| {
            let context = match context_weak.upgrade() {
                Some(c) => c,
                None => return None,
            };
            let sample = values[1].get::<gst::Sample>().unwrap();
            let buffer = sample.buffer().unwrap();
            let map = buffer.map_readable().unwrap();
            let data = map.as_slice();
            let _ = context.streaming.mjpeg_tx.send(Bytes::copy_from_slice(data));
            None
        });

        // Connect decode to tee (dynamic pads)
        let tee_weak = tee.downgrade();
        decode.connect_pad_added(move |_, pad| {
            let tee = match tee_weak.upgrade() {
                Some(t) => t,
                None => return,
            };
            let sink_pad = tee.request_pad_simple("sink_%u").unwrap();
            pad.link(&sink_pad).unwrap();
        });

        // Link source to decode
        source.link(&decode).map_err(|_| "Failed to link source to decode")?;

        // Store pipeline locally and set running flag
        log::info!("🔧 About to store pipeline in AppState");
        self.pipeline = Some(pipeline.clone());
        *self.context.gstreamer.pipeline_running.lock().unwrap() = true;
        log::info!("🔧 Pipeline running flag set to true");
        Ok(())
    }

    pub async fn start_recording(&self) -> Result<()> {
        log::info!("🔧 About to start recording pipeline");
        if let Some(ref pipeline) = self.pipeline {
            log::info!("🔧 Setting pipeline state to Playing");
            pipeline.set_state(gst::State::Playing).map_err(|_| VigilanteError::GStreamer("Failed to start pipeline".to_string()))?;
            log::info!("🔧 Pipeline started successfully");
            
            // Start periodic recording status logging
            let context = Arc::clone(&self.context);
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(300)); // Every 5 minutes
                loop {
                    interval.tick().await;
                    // Check if recording file exists and log its status
                    let timestamp = crate::camera::depends::utils::CameraUtils::format_timestamp();
                    let date = timestamp.split('_').next().unwrap();
                    let dir = context.storage_path().join(date);
                    let filename = format!("{}.mkv", date);
                    let path = dir.join(&filename);
                    
                    if path.exists() {
                        if let Ok(metadata) = std::fs::metadata(&path) {
                            let size_mb = metadata.len() / (1024 * 1024);
                            log::info!("✅ Archivo creciendo: {} ({} MB) - path: {}", filename, size_mb, path.display());
                        }
                    }
                }
            });
            
            Ok(())
        } else {
            log::info!("🔧 Pipeline not warmed up");
            Err(VigilanteError::GStreamer("Pipeline not warmed up".to_string()))
        }
    }
}