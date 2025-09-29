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

        // Decode - use specific elements instead of decodebin for better control
        let rtph264depay = gst::ElementFactory::make("rtph264depay")
            .build()
            .map_err(|e| VigilanteError::GStreamer(format!("Failed to create rtph264depay: {}", e)))?;

        let h264parse = gst::ElementFactory::make("h264parse")
            .build()
            .map_err(|e| VigilanteError::GStreamer(format!("Failed to create h264parse: {}", e)))?;
        h264parse.set_property("config-interval", -1i32);

        let avdec_h264 = gst::ElementFactory::make("avdec_h264")
            .build()
            .map_err(|e| VigilanteError::GStreamer(format!("Failed to create avdec_h264: {}", e)))?;

        // Tee for branching
        let tee = gst::ElementFactory::make("tee").build().map_err(|_| VigilanteError::GStreamer("Failed to create tee".to_string()))?;
        pipeline.add_many([&rtph264depay, &h264parse, &avdec_h264]).map_err(|_| VigilanteError::GStreamer("Failed to add decode elements".to_string()))?;
        gst::Element::link_many([&rtph264depay, &h264parse, &avdec_h264]).map_err(|_| VigilanteError::GStreamer("Failed to link decode chain".to_string()))?;

        // Link source to rtph264depay with caps filter for H264
        let caps = gst::Caps::builder("application/x-rtp")
            .field("media", "video")
            .field("encoding-name", "H264")
            .build();
        source.link_filtered(&rtph264depay, &caps).map_err(|_| VigilanteError::GStreamer("Failed to link source to rtph264depay".to_string()))?;

        // Link avdec_h264 directly to tee
        avdec_h264.link(&tee).map_err(|_| VigilanteError::GStreamer("Failed to link avdec_h264 to tee".to_string()))?;

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

        // Add elements (update to include new decode elements)
        pipeline.add_many([&source, &rtph264depay, &h264parse, &avdec_h264, &tee, &queue_mjpeg, &videoconvert_mjpeg, &jpegenc, &appsink_mjpeg, &queue_rec, &videoconvert_rec, &x264enc, &mp4mux, &filesink]).map_err(|_| VigilanteError::GStreamer("Failed to add elements".to_string()))?;

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

            // Debug logging for MJPEG frames
            log::debug!("📹 MJPEG frame received, size: {} bytes", data.len());

            let result = context.streaming.mjpeg_tx.send(Bytes::copy_from_slice(data));
            match result {
                Ok(_) => log::debug!("📹 MJPEG frame sent to broadcast channel"),
                Err(e) => log::warn!("📹 Failed to send MJPEG frame to broadcast channel: {:?}", e),
            }
            None
        });



        // Link tee to branches (request source pads from tee)
        let tee_src_pad_mjpeg = tee.request_pad_simple("src_%u").unwrap();
        let queue_mjpeg_sink_pad = queue_mjpeg.static_pad("sink").unwrap();
        tee_src_pad_mjpeg.link(&queue_mjpeg_sink_pad).unwrap();
        log::info!("🔧 Linked tee to MJPEG queue");

        let tee_src_pad_rec = tee.request_pad_simple("src_%u").unwrap();
        let queue_rec_sink_pad = queue_rec.static_pad("sink").unwrap();
        tee_src_pad_rec.link(&queue_rec_sink_pad).unwrap();
        log::info!("🔧 Linked tee to recording queue");

        let tee_src_pad_motion = tee.request_pad_simple("src_%u").unwrap();
        let queue_motion_sink_pad = queue_motion.static_pad("sink").unwrap();
        tee_src_pad_motion.link(&queue_motion_sink_pad).unwrap();
        log::info!("🔧 Linked tee to motion queue");

        // Link is now done directly: avdec_h264 -> tee

        // Store pipeline locally and set running flag
        log::info!("🔧 About to set pipeline running flag");
        self.pipeline = Some(pipeline.clone());
        log::info!("🔧 Pipeline stored locally, about to set flag");
        *self.context.gstreamer.pipeline_running.lock().unwrap() = true;
        log::info!("🔧 Pipeline running flag set to true successfully");
        log::info!("🔧 warm_up function completed successfully");
        Ok(())
    }

    pub async fn start_recording(&self) -> Result<()> {
        log::info!("🔧 Start recording called");
        if let Some(ref pipeline) = self.pipeline {
            log::info!("🔧 Pipeline exists, setting state to Playing");
            pipeline.set_state(gst::State::Playing).map_err(|_| VigilanteError::GStreamer("Failed to start pipeline".to_string()))?;
            log::info!("🔧 Pipeline state set to Playing successfully");
            
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
            log::info!("🔧 Pipeline not found in start_recording");
            Err(VigilanteError::GStreamer("Pipeline not warmed up".to_string()))
        }
    }
}