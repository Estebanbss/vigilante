//! Pipeline de FFmpeg para captura de video.
//!
//! Maneja la configuración y ejecución del pipeline de GStreamer
//! para grabación continua de video.

use crate::AppState;
use crate::error::{Result, VigilanteError};
use std::sync::Arc;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer::Fraction;
use gstreamer_rtsp::RTSPLowerTrans;
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
        source.set_property("latency", 0u32); // Minimum latency for real-time streaming
        source.set_property("protocols", RTSPLowerTrans::TCP); // Use TCP protocol for RTSP
        source.set_property("do-rtcp", true);

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

        // MJPEG branch
        let queue_mjpeg = gst::ElementFactory::make("queue").build().map_err(|_| VigilanteError::GStreamer("Failed to create queue_mjpeg".to_string()))?;
        let videoconvert_mjpeg = gst::ElementFactory::make("videoconvert").build().map_err(|_| VigilanteError::GStreamer("Failed to create videoconvert_mjpeg".to_string()))?;
        let videoscale_mjpeg = gst::ElementFactory::make("videoscale").build().map_err(|_| VigilanteError::GStreamer("Failed to create videoscale_mjpeg".to_string()))?;
        let videorate_mjpeg = gst::ElementFactory::make("videorate").build().map_err(|_| VigilanteError::GStreamer("Failed to create videorate_mjpeg".to_string()))?;
        let capsfilter_mjpeg = gst::ElementFactory::make("capsfilter").build().map_err(|_| VigilanteError::GStreamer("Failed to create capsfilter_mjpeg".to_string()))?;
        let mjpeg_caps = gst::Caps::builder("video/x-raw")
            .field("width", 1280i32)
            .field("height", 720i32)
            .field("framerate", Fraction::new(15, 1))
            .field("format", "I420")
            .build();
        capsfilter_mjpeg.set_property("caps", &mjpeg_caps);

        let jpegenc = gst::ElementFactory::make("jpegenc").build().map_err(|_| VigilanteError::GStreamer("Failed to create jpegenc".to_string()))?;
        jpegenc.set_property("quality", 85i32);

        let appsink_mjpeg = gst::ElementFactory::make("appsink").build().map_err(|_| VigilanteError::GStreamer("Failed to create appsink_mjpeg".to_string()))?;
        appsink_mjpeg.set_property("emit-signals", true);
        appsink_mjpeg.set_property("sync", false);
        appsink_mjpeg.set_property("max-buffers", 1u32);
        appsink_mjpeg.set_property("drop", true);

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

        // Set up dynamic pad linking for rtspsrc -> rtph264depay
        // This must be done BEFORE adding elements to pipeline
        let rtph264depay_clone = rtph264depay.clone();
        source.connect_pad_added(move |_, src_pad| {
            log::info!("🔧 RTSP source created new pad: {:?}", src_pad.name());

            // Check if this is a video pad by examining caps
            let caps = src_pad.current_caps();
            if let Some(caps) = caps {
                if let Some(structure) = caps.structure(0) {
                    if let Ok(media) = structure.get::<&str>("media") {
                        if media == "video" {
                            log::info!("🔧 Found video pad from RTSP source, linking to rtph264depay");
                            let sink_pad = rtph264depay_clone.static_pad("sink").unwrap();
                            if let Err(e) = src_pad.link(&sink_pad) {
                                log::error!("🔧 Failed to link RTSP src pad to rtph264depay sink: {:?}", e);
                            } else {
                                log::info!("🔧 Successfully linked RTSP src pad to rtph264depay sink");
                            }
                        }
                    }
                }
            } else {
                log::warn!("🔧 RTSP pad created without caps, will try to link anyway");
                // Try to link anyway if no caps available yet
                let sink_pad = rtph264depay_clone.static_pad("sink");
                if let Some(sink_pad) = sink_pad {
                    if let Err(e) = src_pad.link(&sink_pad) {
                        log::error!("🔧 Failed to link RTSP src pad to rtph264depay sink (no caps): {:?}", e);
                    } else {
                        log::info!("🔧 Successfully linked RTSP src pad to rtph264depay sink (no caps check)");
                    }
                }
            }
        });

        // Add elements (include rtph264depay in the add_many call)
        pipeline.add_many([
            &source,
            &rtph264depay,
            &h264parse,
            &avdec_h264,
            &tee,
            &queue_mjpeg,
            &videoconvert_mjpeg,
            &videoscale_mjpeg,
            &videorate_mjpeg,
            &capsfilter_mjpeg,
            &jpegenc,
            &appsink_mjpeg,
            &queue_rec,
            &videoconvert_rec,
            &x264enc,
            &mp4mux,
            &filesink,
        ]).map_err(|_| VigilanteError::GStreamer("Failed to add elements".to_string()))?;

        // Link the decode chain
        gst::Element::link_many([&rtph264depay, &h264parse, &avdec_h264]).map_err(|_| VigilanteError::GStreamer("Failed to link decode chain".to_string()))?;

        // Link avdec_h264 directly to tee
        avdec_h264.link(&tee).map_err(|_| VigilanteError::GStreamer("Failed to link avdec_h264 to tee".to_string()))?;

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
        gst::Element::link_many([
            &queue_mjpeg,
            &videoconvert_mjpeg,
            &videoscale_mjpeg,
            &videorate_mjpeg,
            &capsfilter_mjpeg,
            &jpegenc,
            &appsink_mjpeg,
        ]).map_err(|_| VigilanteError::GStreamer("Failed to link MJPEG".to_string()))?;
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
            log::info!("📹 MJPEG frame received, size: {} bytes", data.len());

            let result = context.streaming.mjpeg_tx.send(Bytes::copy_from_slice(data));
            match result {
                Ok(_) => log::info!("📹 MJPEG frame sent to broadcast channel"),
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