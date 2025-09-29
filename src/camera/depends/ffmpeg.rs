//! Pipeline de FFmpeg para captura de video.
//!
//! Maneja la configuración y ejecución del pipeline de GStreamer
//! para grabación continua de video.

use crate::camera::depends::motion::MotionDetector;
use crate::error::{Result, VigilanteError};
use crate::AppState;
use bytes::Bytes;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_rtsp::RTSPLowerTrans;
use std::sync::Arc;
use tokio;
use tokio::runtime::Handle;

#[derive(Debug)]
pub struct CameraPipeline {
    pub pipeline: Option<gst::Pipeline>,
    pub mjpeg_tx: tokio::sync::broadcast::Sender<Bytes>,
    pub motion_detector: Arc<MotionDetector>,
    pub context: Arc<AppState>,
}
impl CameraPipeline {
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

        let source = gst::ElementFactory::make("rtspsrc")
            .build()
            .map_err(|e| VigilanteError::GStreamer(format!("Failed to create rtspsrc: {}", e)))?;
        source.set_property("location", self.context.camera_rtsp_url());
        source.set_property("latency", 1000u32);
        source.set_property("protocols", RTSPLowerTrans::TCP);
        source.set_property("do-rtcp", true);

        let rtph264depay = gst::ElementFactory::make("rtph264depay")
            .build()
            .map_err(|e| {
                VigilanteError::GStreamer(format!("Failed to create rtph264depay: {}", e))
            })?;

        let h264parse = gst::ElementFactory::make("h264parse")
            .build()
            .map_err(|e| VigilanteError::GStreamer(format!("Failed to create h264parse: {}", e)))?;
        h264parse.set_property("config-interval", -1i32);

        let tee = gst::ElementFactory::make("tee")
            .build()
            .map_err(|_| VigilanteError::GStreamer("Failed to create tee".to_string()))?;

        let queue_rec = gst::ElementFactory::make("queue")
            .build()
            .map_err(|_| VigilanteError::GStreamer("Failed to create queue_rec".to_string()))?;
        let capsfilter_rec = gst::ElementFactory::make("capsfilter")
            .build()
            .map_err(|_| {
                VigilanteError::GStreamer("Failed to create capsfilter_rec".to_string())
            })?;
        let rec_caps = gst::Caps::builder("video/x-h264")
            .field("stream-format", "avc")
            .field("alignment", "au")
            .build();
        capsfilter_rec.set_property("caps", &rec_caps);
        let mux = gst::ElementFactory::make("matroskamux")
            .build()
            .map_err(|_| VigilanteError::GStreamer("Failed to create matroskamux".to_string()))?;
        mux.set_property("writing-app", "vigilante");
        let filesink = gst::ElementFactory::make("filesink")
            .build()
            .map_err(|_| VigilanteError::GStreamer("Failed to create filesink".to_string()))?;
        let timestamp = crate::camera::depends::utils::CameraUtils::format_timestamp();
        let date = timestamp.split('_').next().unwrap();
        let dir = self.context.storage_path().join(date);
        std::fs::create_dir_all(&dir).map_err(VigilanteError::Io)?;
        let filename = format!("{}.mkv", date);
        let path = dir.join(&filename);
        filesink.set_property("location", path.to_str().unwrap());
        filesink.set_property("sync", &false);

        log::info!("📹 Grabación iniciada: {}", path.display());

        let runtime_handle = Handle::current();

        let queue_motion = gst::ElementFactory::make("queue")
            .build()
            .map_err(|_| VigilanteError::GStreamer("Failed to create queue_motion".to_string()))?;
        let avdec_motion = gst::ElementFactory::make("avdec_h264")
            .build()
            .map_err(|_| VigilanteError::GStreamer("Failed to create avdec_motion".to_string()))?;
        let videoconvert_motion =
            gst::ElementFactory::make("videoconvert")
                .build()
                .map_err(|_| {
                    VigilanteError::GStreamer("Failed to create videoconvert_motion".to_string())
                })?;
        let videoscale_motion = gst::ElementFactory::make("videoscale")
            .build()
            .map_err(|_| {
                VigilanteError::GStreamer("Failed to create videoscale_motion".to_string())
            })?;
        let capsfilter_motion = gst::ElementFactory::make("capsfilter")
            .build()
            .map_err(|_| {
                VigilanteError::GStreamer("Failed to create capsfilter_motion".to_string())
            })?;
        let motion_caps = gst::Caps::builder("video/x-raw")
            .field("format", "GRAY8")
            .field("width", 320i32)
            .field("height", 180i32)
            .build();
        capsfilter_motion.set_property("caps", &motion_caps);
        let appsink_motion = gst_app::AppSink::builder().build();
        appsink_motion.set_property("emit-signals", &true);
        appsink_motion.set_property("sync", &false);
        appsink_motion.set_max_buffers(1);
        appsink_motion.set_drop(true);

        let queue_live = gst::ElementFactory::make("queue")
            .build()
            .map_err(|_| VigilanteError::GStreamer("Failed to create queue_live".to_string()))?;
        let mp4mux = gst::ElementFactory::make("mp4mux")
            .build()
            .map_err(|_| VigilanteError::GStreamer("Failed to create mp4mux".to_string()))?;
        mp4mux.set_property("streamable", &true);
        let appsink_live = gst_app::AppSink::builder().build();
        appsink_live.set_property("emit-signals", &true);
        appsink_live.set_property("sync", &false);
        appsink_live.set_max_buffers(10);
        appsink_live.set_drop(false);

        // Audio elements (PCMA)
        let rtppcmadepay = gst::ElementFactory::make("rtppcmadepay")
            .build()
            .map_err(|_| VigilanteError::GStreamer("Failed to create rtppcmadepay".to_string()))?;
        let alawdec = gst::ElementFactory::make("alawdec")
            .build()
            .map_err(|_| VigilanteError::GStreamer("Failed to create alawdec".to_string()))?;
        let audioconvert = gst::ElementFactory::make("audioconvert")
            .build()
            .map_err(|_| VigilanteError::GStreamer("Failed to create audioconvert".to_string()))?;
        let audioresample = gst::ElementFactory::make("audioresample")
            .build()
            .map_err(|_| VigilanteError::GStreamer("Failed to create audioresample".to_string()))?;
        let voaacenc = gst::ElementFactory::make("voaacenc")
            .build()
            .map_err(|_| VigilanteError::GStreamer("Failed to create voaacenc".to_string()))?;
        voaacenc.set_property("bitrate", 128000i32); // 128 kbps AAC
        let appsink_audio = gst_app::AppSink::builder().build();
        appsink_audio.set_property("emit-signals", &true);
        appsink_audio.set_property("sync", &false);
        appsink_audio.set_max_buffers(2); // Reduce buffering
        appsink_audio.set_drop(false); // Don't drop buffers

        pipeline
            .add_many([
                &source,
                &rtph264depay,
                &h264parse,
                &tee,
                &queue_rec,
                &capsfilter_rec,
                &mux,
                &filesink,
                &queue_motion,
                &avdec_motion,
                &videoconvert_motion,
                &videoscale_motion,
                &capsfilter_motion,
                appsink_motion.upcast_ref(),
                &queue_live,
                &mp4mux,
                appsink_live.upcast_ref(),
                &rtppcmadepay,
                &alawdec,
                &audioconvert,
                &audioresample,
                &voaacenc,
            ])
            .map_err(|_| VigilanteError::GStreamer("Failed to add elements".to_string()))?;

        let rtph264depay_clone = rtph264depay.clone();
        let rtppcmadepay_clone = rtppcmadepay.clone();
        let alawdec_clone = alawdec.clone();
        let audioconvert_clone = audioconvert.clone();
        let audioresample_clone = audioresample.clone();
        let voaacenc_clone = voaacenc.clone();
        let mp4mux_clone = mp4mux.clone();
        source.connect_pad_added(move |_, src_pad| {
            log::info!("🔧 RTSP source created new pad: {:?}", src_pad.name());

            if let Some(caps) = src_pad.current_caps() {
                if let Some(structure) = caps.structure(0) {
                    if let Ok(media) = structure.get::<&str>("media") {
                        match media {
                            "video" => {
                                log::info!("🔧 Found video pad from RTSP source, linking to rtph264depay");
                                let sink_pad = rtph264depay_clone.static_pad("sink");
                                if let Some(sink_pad) = sink_pad {
                                    if let Err(e) = src_pad.link(&sink_pad) {
                                        log::error!("🔧 Failed to link RTSP src pad to rtph264depay sink: {:?}", e);
                                    } else {
                                        log::info!("🔧 Successfully linked RTSP src pad to rtph264depay sink");
                                    }
                                }
                            }
                            "audio" => {
                                log::info!("🔊 Found audio pad from RTSP source, caps: {:?}", caps);
                                log::info!("🔊 Linking audio branch dynamically");
                                let sink_pad = rtppcmadepay_clone.static_pad("sink");
                                if let Some(sink_pad) = sink_pad {
                                    if let Err(e) = src_pad.link(&sink_pad) {
                                        log::error!("🔊 Failed to link RTSP audio pad to rtppcmadepay sink: {:?}", e);
                                    } else {
                                        log::info!("🔊 Successfully linked RTSP audio pad to rtppcmadepay sink");
                                        // Now link the audio branch
                                        if let Err(e) = gst::Element::link_many([
                                            &rtppcmadepay_clone,
                                            &alawdec_clone,
                                            &audioconvert_clone,
                                            &audioresample_clone,
                                            &voaacenc_clone,
                                        ]) {
                                            log::error!("🔊 Failed to link audio branch: {:?}", e);
                                        } else {
                                            log::info!("🔊 Successfully linked audio branch");
                                            // Link voaacenc to mp4mux audio pad
                                            let mp4mux_audio_pad = mp4mux_clone.request_pad_simple("sink_%u").unwrap();
                                            let voaacenc_src_pad = voaacenc_clone.static_pad("src").unwrap();
                                            if let Err(e) = voaacenc_src_pad.link(&mp4mux_audio_pad) {
                                                log::error!("🔊 Failed to link lamemp3enc to mp4mux: {:?}", e);
                                            } else {
                                                log::info!("🔊 Successfully linked audio to mp4mux");
                                            }
                                        }
                                    }
                                }
                            }
                            other => {
                                log::info!("🔊 Ignoring RTSP pad '{}' with media '{}'", src_pad.name(), other);
                            }
                        }
                        return;
                    }
                }
            }

            log::warn!("🔧 RTSP pad created without media info: {:?}", src_pad.name());
        });

        gst::Element::link_many([&rtph264depay, &h264parse]).map_err(|_| {
            VigilanteError::GStreamer("Failed to link depay to h264parse".to_string())
        })?;
        h264parse.link(&tee).map_err(|_| {
            VigilanteError::GStreamer("Failed to link h264parse to tee".to_string())
        })?;

        gst::Element::link_many([&queue_rec, &capsfilter_rec, &mux, &filesink]).map_err(|_| {
            VigilanteError::GStreamer("Failed to link recording branch".to_string())
        })?;

        gst::Element::link_many([
            &queue_motion,
            &avdec_motion,
            &videoconvert_motion,
            &videoscale_motion,
            &capsfilter_motion,
            appsink_motion.upcast_ref(),
        ])
        .map_err(|_| VigilanteError::GStreamer("Failed to link motion branch".to_string()))?;

        gst::Element::link_many([&mp4mux, appsink_live.upcast_ref()]).map_err(|_| {
            VigilanteError::GStreamer("Failed to link live branch".to_string())
        })?;

        let detector_for_motion = Arc::clone(&self.motion_detector);
        let motion_handle = runtime_handle.clone();
        let motion_callbacks = gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = match sink.pull_sample() {
                    Ok(sample) => sample,
                    Err(err) => {
                        log::warn!("⚠️ Failed to pull motion sample: {:?}", err);
                        return Err(gst::FlowError::Error);
                    }
                };

                let buffer = match sample.buffer() {
                    Some(buffer) => buffer,
                    None => {
                        log::warn!("⚠️ Motion sample missing buffer");
                        return Ok(gst::FlowSuccess::Ok);
                    }
                };

                let map = match buffer.map_readable() {
                    Ok(map) => map,
                    Err(err) => {
                        log::warn!("⚠️ Failed to map motion buffer: {:?}", err);
                        return Err(gst::FlowError::Error);
                    }
                };

                let frame = map.as_slice().to_vec();
                log::debug!("🚶 Motion frame received, size: {} bytes", frame.len());
                let detector = Arc::clone(&detector_for_motion);
                let _ = motion_handle.spawn(async move {
                    let _ = detector.detect_motion(&frame).await;
                });

                Ok(gst::FlowSuccess::Ok)
            })
            .build();
        appsink_motion.set_callbacks(motion_callbacks);
        log::info!("🚶 Motion appsink callbacks configured");

        let context_weak = Arc::downgrade(&self.context);
        let live_callbacks = gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                log::info!("📹 Live callback called");
                let context = match context_weak.upgrade() {
                    Some(c) => c,
                    None => {
                        log::warn!("📹 Live context dropped, stopping callback");
                        return Err(gst::FlowError::Flushing);
                    }
                };

                let sample = match sink.pull_sample() {
                    Ok(sample) => sample,
                    Err(err) => {
                        log::warn!("📹 Failed to pull live sample: {:?}", err);
                        return Err(gst::FlowError::Error);
                    }
                };

                let buffer = match sample.buffer() {
                    Some(buffer) => buffer,
                    None => {
                        log::warn!("📹 Live sample missing buffer");
                        return Ok(gst::FlowSuccess::Ok);
                    }
                };

                let map = match buffer.map_readable() {
                    Ok(map) => map,
                    Err(err) => {
                        log::warn!("📹 Failed to map live buffer: {:?}", err);
                        return Err(gst::FlowError::Error);
                    }
                };

                let data = Bytes::copy_from_slice(map.as_slice());

                log::info!("📹 Live MP4 fragment received, size: {} bytes", data.len());

                match context.streaming.mjpeg_tx.send(data) {
                    Ok(_) => log::debug!("📹 Live frame sent to broadcast channel"),
                    Err(e) => log::warn!(
                        "📹 Failed to send live frame to broadcast channel: {:?}",
                        e
                    ),
                }

                Ok(gst::FlowSuccess::Ok)
            })
            .build();
        appsink_live.set_callbacks(live_callbacks);
        log::info!("📺 Live appsink callbacks configured");



        let tee_src_pad_rec = tee.request_pad_simple("src_%u").unwrap();
        let queue_rec_sink_pad = queue_rec.static_pad("sink").unwrap();
        tee_src_pad_rec.link(&queue_rec_sink_pad).unwrap();
        log::info!("🔧 Linked tee to recording queue");

        let tee_src_pad_motion = tee.request_pad_simple("src_%u").unwrap();
        let queue_motion_sink_pad = queue_motion.static_pad("sink").unwrap();
        tee_src_pad_motion.link(&queue_motion_sink_pad).unwrap();
        log::info!("🔧 Linked tee to motion queue");

        let tee_src_pad_live = tee.request_pad_simple("src_%u").unwrap();
        let queue_live_sink_pad = queue_live.static_pad("sink").unwrap();
        tee_src_pad_live.link(&queue_live_sink_pad).unwrap();
        log::info!("🔧 Linked tee to live queue");

        queue_live.link(&mp4mux).unwrap();
        log::info!("🔧 Linked live queue to mp4mux");

        let mp4mux_src_pad = mp4mux.static_pad("src").unwrap();
        let appsink_live_sink_pad = appsink_live.static_pad("sink").unwrap();
        mp4mux_src_pad.link(&appsink_live_sink_pad).unwrap();
        log::info!("🔧 Linked mp4mux src to live appsink sink");

        log::info!("🔧 About to set pipeline running flag");
        self.pipeline = Some(pipeline.clone());
        log::info!("🔧 Pipeline stored locally, about to set flag");
        *self.context.gstreamer.pipeline_running.lock().unwrap() = true;
        log::info!("🔧 Pipeline running flag set to true successfully");
        log::info!("🔧 warm_up function completed successfully");
        Ok(())
    }

    pub async fn start_recording(&self) -> Result<()> {
        log::info!("🔧 Start recording called, pipeline: {:?}", self.pipeline.is_some());
        if let Some(ref pipeline) = self.pipeline {
            log::info!("🔧 Pipeline exists, setting state to Playing");
            pipeline
                .set_state(gst::State::Playing)
                .map_err(|_| VigilanteError::GStreamer("Failed to start pipeline".to_string()))?;
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
                            log::info!(
                                "✅ Archivo creciendo: {} ({} MB) - path: {}",
                                filename,
                                size_mb,
                                path.display()
                            );
                        }
                    }
                }
            });

            Ok(())
        } else {
            log::info!("🔧 Pipeline not found in start_recording");
            Err(VigilanteError::GStreamer(
                "Pipeline not warmed up".to_string(),
            ))
        }
    }
}
