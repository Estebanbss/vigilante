//! Pipeline de FFmpeg para captura de video.
//!
//! Maneja la configuración y ejecución del pipeline de GStreamer
//! para grabación continua de video.

use crate::camera::depends::motion::MotionDetector;
use crate::error::{Result, VigilanteError};
use crate::metrics::depends::collectors::MetricsCollector;
use crate::state::LatencySnapshot;
use crate::AppState;
use bytes::Bytes;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_rtsp::RTSPLowerTrans;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio;
use tokio::runtime::Handle;
use tokio::time::timeout;

/// Verifica la conectividad RTSP antes de iniciar grabación
async fn check_rtsp_connectivity(rtsp_url: &str) -> Result<()> {
    log::info!("🔍 Verificando conectividad RTSP: {}", rtsp_url);
    
    // Crear un pipeline de prueba simple para verificar conectividad
    let pipeline_str = format!("rtspsrc location={} latency=1000 ! fakesink", rtsp_url);
    
    let pipeline = gst::Pipeline::new();
    let launch_result = gst::parse::launch(&pipeline_str);
    
    match launch_result {
        Ok(bin) => {
            pipeline.add(&bin).map_err(|_| {
                VigilanteError::GStreamer("Failed to add RTSP test pipeline".to_string())
            })?;
            
            // Intentar iniciar el pipeline de prueba
            match timeout(Duration::from_secs(10), async {
                match pipeline.set_state(gst::State::Playing) {
                    Ok(_) => {
                        // Esperar un poco para ver si se conecta
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        
                        // Verificar si hay errores
                        let bus = pipeline.bus().unwrap();
                        let mut has_error = false;
                        
                        for _ in 0..10 {
                            if let Some(msg) = bus.timed_pop(100 * gst::ClockTime::MSECOND) {
                                match msg.view() {
                                    gst::MessageView::Error(_) => {
                                        has_error = true;
                                        break;
                                    }
                                    gst::MessageView::StateChanged(_) => {
                                        // Estado cambió, probablemente conectando
                                    }
                                    _ => {}
                                }
                            }
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        
                        // Detener el pipeline de prueba
                        let _ = pipeline.set_state(gst::State::Null);
                        
                        if has_error {
                            Err(VigilanteError::GStreamer("RTSP connection failed".to_string()))
                        } else {
                            Ok(())
                        }
                    }
                    Err(_) => Err(VigilanteError::GStreamer("Failed to start RTSP test pipeline".to_string()))
                }
            }).await {
                Ok(result) => result,
                Err(_) => {
                    log::warn!("⏰ Timeout verificando conectividad RTSP");
                    Err(VigilanteError::GStreamer("RTSP connectivity check timeout".to_string()))
                }
            }
        }
        Err(_) => Err(VigilanteError::GStreamer("Failed to create RTSP test pipeline".to_string()))
    }
}

/// Verifica que el pipeline completo se puede inicializar
async fn check_pipeline_initialization(_context: &Arc<AppState>) -> Result<()> {
    log::info!("🔧 Verificando inicialización del pipeline completo");
    
    // Intentar crear los elementos básicos del pipeline
    let _source = gst::ElementFactory::make("rtspsrc")
        .build()
        .map_err(|_| VigilanteError::GStreamer("Failed to create rtspsrc for test".to_string()))?;
    
    // Intentar configurar propiedades básicas
    let _decodebin = gst::ElementFactory::make("decodebin")
        .build()
        .map_err(|_| VigilanteError::GStreamer("Failed to create decodebin for test".to_string()))?;
    
    log::info!("✅ Pipeline básico se puede inicializar");
    Ok(())
}

/// Implementa reintentos con backoff exponencial
async fn retry_with_backoff<F, Fut, T>(
    operation: F,
    max_attempts: u32,
    base_delay_ms: u64,
    operation_name: &str
) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut attempt = 1;
    
    loop {
        log::info!("🔄 Intento {} de {} para {}", attempt, max_attempts, operation_name);
        
        match operation().await {
            Ok(result) => {
                log::info!("✅ {} exitoso en intento {}", operation_name, attempt);
                return Ok(result);
            }
            Err(e) => {
                if attempt >= max_attempts {
                    log::error!("❌ {} falló después de {} intentos: {:?}", operation_name, max_attempts, e);
                    return Err(e);
                }
                
                let delay_ms = base_delay_ms * (2u64.pow(attempt - 1));
                let delay_ms = delay_ms.min(30000); // Máximo 30 segundos
                
                log::warn!("⚠️  {} falló (intento {}): {:?}. Reintentando en {}ms", 
                          operation_name, attempt, e, delay_ms);
                
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                attempt += 1;
            }
        }
    }
}

#[derive(Debug)]
pub struct CameraPipeline {
    pub pipeline: Option<gst::Pipeline>,
    pub motion_detector: Arc<MotionDetector>,
    pub context: Arc<AppState>,
}

#[derive(Debug, Clone)]
struct LatencyUpdate {
    latency_ms: f64,
    snapshot: LatencySnapshot,
    should_log: bool,
}

#[derive(Debug, Default)]
struct LatencyTracker {
    origin_instant: Option<Instant>,
    origin_pts: Option<gst::ClockTime>,
    snapshot: LatencySnapshot,
    samples_since_log: u32,
}

impl LatencyTracker {
    fn update(&mut self, pts: gst::ClockTime, now: Instant) -> Option<LatencyUpdate> {
        let pts_ns = pts.nseconds();

        if self.origin_pts.is_none() || self.origin_instant.is_none() {
            self.origin_pts = Some(pts);
            self.origin_instant = Some(now);
            self.snapshot.samples = 0;
            self.snapshot.last_ms = Some(0.0);
            self.snapshot.ewma_ms = Some(0.0);
            self.snapshot.min_ms = Some(0.0);
            self.snapshot.max_ms = Some(0.0);
            return Some(LatencyUpdate {
                latency_ms: 0.0,
                snapshot: self.snapshot.clone(),
                should_log: false,
            });
        }

        let origin_pts_ns = self.origin_pts.unwrap().nseconds();
        let origin_instant = self.origin_instant.unwrap();

        let stream_elapsed_ms = if pts_ns >= origin_pts_ns {
            (pts_ns - origin_pts_ns) as f64 / 1_000_000.0
        } else {
            0.0
        };

        let real_elapsed_ms = now.duration_since(origin_instant).as_secs_f64() * 1_000.0;
        let mut latency_ms = real_elapsed_ms - stream_elapsed_ms;
        if latency_ms.is_nan() || latency_ms.is_infinite() {
            latency_ms = 0.0;
        }
        if latency_ms < 0.0 {
            latency_ms = 0.0;
        }

        self.snapshot.samples = self.snapshot.samples.saturating_add(1);
        self.snapshot.last_ms = Some(latency_ms);
        self.snapshot.min_ms = Some(match self.snapshot.min_ms {
            Some(current) => current.min(latency_ms),
            None => latency_ms,
        });
        self.snapshot.max_ms = Some(match self.snapshot.max_ms {
            Some(current) => current.max(latency_ms),
            None => latency_ms,
        });

        let ewma = match self.snapshot.ewma_ms {
            Some(current) => 0.65 * current + 0.35 * latency_ms,
            None => latency_ms,
        };
        self.snapshot.ewma_ms = Some(ewma);

        self.samples_since_log += 1;
        let should_log = if self.samples_since_log >= 300 {
            self.samples_since_log = 0;
            true
        } else {
            false
        };

        Some(LatencyUpdate {
            latency_ms,
            snapshot: self.snapshot.clone(),
            should_log,
        })
    }
}
impl CameraPipeline {
    pub fn new(context: Arc<AppState>, motion_detector: Arc<MotionDetector>) -> Self {
        Self {
            pipeline: None,
            motion_detector,
            context,
        }
    }

    pub async fn warm_up(&mut self) -> Result<()> {
        let pipeline = gst::Pipeline::new();

        {
            let mut segments_guard = self.context.streaming.mp4_init_segments.lock().unwrap();
            segments_guard.clear();

            let mut complete_guard = self.context.streaming.mp4_init_complete.lock().unwrap();
            *complete_guard = false;

            let mut tail_guard = self.context.streaming.mp4_init_scan_tail.lock().unwrap();
            tail_guard.clear();

            let mut warned_guard = self
                .context
                .streaming
                .mp4_init_warned_before_moov
                .lock()
                .unwrap();
            *warned_guard = false;
        }

        let source = gst::ElementFactory::make("rtspsrc")
            .build()
            .map_err(|e| VigilanteError::GStreamer(format!("Failed to create rtspsrc: {}", e)))?;
        source.set_property("location", self.context.camera_rtsp_url());
        source.set_property("latency", 60u32);
        source.set_property("protocols", RTSPLowerTrans::TCP);
        source.set_property("do-rtcp", true);
        if source.find_property("drop-on-late").is_some() {
            source.set_property("drop-on-late", &true);
        }
        if source.find_property("buffer-mode").is_some() {
            source.set_property_from_str("buffer-mode", "none");
        }

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
        log::info!("🔧 Recording caps set: {:?}", rec_caps);
        let mux = gst::ElementFactory::make("matroskamux")
            .build()
            .map_err(|_| VigilanteError::GStreamer("Failed to create matroskamux".to_string()))?;
        mux.set_property("writing-app", "vigilante");
        let filesink = gst::ElementFactory::make("filesink")
            .build()
            .map_err(|_| VigilanteError::GStreamer("Failed to create filesink".to_string()))?;
        
        // 🔍 PRE-FLIGHT CHECKS: Verificar conectividad antes de crear archivos
        log::info!("🔍 Ejecutando verificaciones previas antes de crear archivo de grabación");
        
        // Verificar inicialización del pipeline
        if let Err(e) = check_pipeline_initialization(&self.context).await {
            log::error!("❌ Falló verificación de inicialización del pipeline: {:?}", e);
            return Err(e);
        }
        
        // Verificar conectividad RTSP con reintentos
        if let Err(e) = retry_with_backoff(
            || check_rtsp_connectivity(self.context.camera_rtsp_url()),
            3, // máximo 3 intentos
            2000, // delay base de 2 segundos
            "verificación RTSP"
        ).await {
            log::error!("❌ Falló verificación de conectividad RTSP después de reintentos: {:?}", e);
            return Err(VigilanteError::GStreamer(format!("RTSP connectivity check failed: {:?}", e)));
        }
        
        log::info!("✅ Todas las verificaciones previas pasaron. Procediendo con creación de archivo.");
        
        let timestamp = crate::camera::depends::utils::CameraUtils::format_timestamp();
        let date = timestamp.split('_').next().unwrap();
        let dir = self.context.storage_path().join(date);
        std::fs::create_dir_all(&dir).map_err(VigilanteError::Io)?;
        
        // Find next available file number to prevent overwrites
        let mut next_num = 1;
        let mut existing_files = Vec::new();
        
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.ends_with(".mkv") {
                        // Check for exact date match (unnumbered file)
                        if name == format!("{}.mkv", date) {
                            existing_files.push(1);
                        }
                        // Check for numbered files
                        else if name.starts_with(&format!("{}-", date)) && name.ends_with(".mkv") {
                            if let Some(num_str) = name
                                .strip_prefix(&format!("{}-", date))
                                .and_then(|s| s.strip_suffix(".mkv"))
                            {
                                if let Ok(num) = num_str.parse::<i32>() {
                                    if num > 0 {
                                        existing_files.push(num);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        
        // Find the highest existing number
        for &num in &existing_files {
            next_num = next_num.max(num + 1);
        }
        
        // Check for very recent files (informational only - pre-flight checks prevent issues)
        let now = std::time::SystemTime::now();
        let mut recent_files_count = 0;
        let mut orphaned_small_files = 0;
        
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|_| std::fs::read_dir("/tmp").unwrap()) {
            if let Ok(entry) = entry {
                if let Ok(metadata) = entry.metadata() {
                    let file_size = metadata.len();
                    
                    // Check for very small files (< 1MB) that might be orphaned
                    if file_size < 1024 * 1024 {
                        orphaned_small_files += 1;
                    }
                    
                    if let Ok(modified) = metadata.modified() {
                        if let Ok(duration) = now.duration_since(modified) {
                            // Count files modified in the last 5 minutes
                            if duration.as_secs() < 300 {
                                recent_files_count += 1;
                            }
                        }
                    }
                }
            }
        }
        
        // Log warnings but don't block - pre-flight checks should prevent issues
        if recent_files_count >= 3 {
            log::warn!(
                "ℹ️  Información: {} archivos de grabación modificados en los últimos 5 minutos. Verificaciones previas deberían prevenir problemas.",
                recent_files_count
            );
        }
        
        if orphaned_small_files >= 5 {
            log::warn!(
                "ℹ️  Información: {} archivos pequeños (< 1MB) detectados. Pueden ser de grabaciones anteriores fallidas.",
                orphaned_small_files
            );
        }
        
        let filename = if next_num == 1 {
            format!("{}.mkv", date)
        } else {
            format!("{}-{}.mkv", date, next_num)
        };
        let path = dir.join(&filename);
        filesink.set_property("location", path.to_str().unwrap());
        filesink.set_property("sync", &false);
        // Avoid preroll blocking on live sources; let pipeline reach Playing without waiting
        if filesink.find_property("async").is_some() {
            filesink.set_property("async", &false);
        }

        log::info!("📹 Grabación iniciada: {}", path.display());

        let runtime_handle = Handle::current();
        let latency_tracker = Arc::new(StdMutex::new(LatencyTracker::default()));

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

        // --- MJPEG live branch (for multipart/x-mixed-replace endpoint) ---
        let queue_mjpeg = gst::ElementFactory::make("queue")
            .build()
            .map_err(|_| VigilanteError::GStreamer("Failed to create queue_mjpeg".to_string()))?;
        // Leaky to avoid backpressure on live preview
        queue_mjpeg.set_property_from_str("leaky", "downstream");
        queue_mjpeg.set_property("max-size-buffers", 2u32);
        queue_mjpeg.set_property("max-size-time", 80_000_000u64);

        let avdec_mjpeg = gst::ElementFactory::make("avdec_h264")
            .build()
            .map_err(|_| VigilanteError::GStreamer("Failed to create avdec_mjpeg".to_string()))?;
        let videoconvert_mjpeg = gst::ElementFactory::make("videoconvert")
            .build()
            .map_err(|_| VigilanteError::GStreamer("Failed to create videoconvert_mjpeg".to_string()))?;
        let videoscale_mjpeg = gst::ElementFactory::make("videoscale")
            .build()
            .map_err(|_| VigilanteError::GStreamer("Failed to create videoscale_mjpeg".to_string()))?;
        let capsfilter_mjpeg = gst::ElementFactory::make("capsfilter")
            .build()
            .map_err(|_| VigilanteError::GStreamer("Failed to create capsfilter_mjpeg".to_string()))?;
        let caps_mjpeg = gst::Caps::builder("video/x-raw")
            .field("width", 1280i32)
            .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
            .build();
        capsfilter_mjpeg.set_property("caps", &caps_mjpeg);

        let jpegenc = gst::ElementFactory::make("jpegenc")
            .build()
            .map_err(|_| VigilanteError::GStreamer("Failed to create jpegenc".to_string()))?;
        if jpegenc.find_property("quality").is_some() {
            jpegenc.set_property("quality", &85i32);
        }
        let appsink_mjpeg = gst_app::AppSink::builder().build();
        appsink_mjpeg.set_property("emit-signals", &true);
        appsink_mjpeg.set_property("sync", &false);
        appsink_mjpeg.set_max_buffers(1);
        appsink_mjpeg.set_drop(true);

        let queue_live = gst::ElementFactory::make("queue")
            .build()
            .map_err(|_| VigilanteError::GStreamer("Failed to create queue_live".to_string()))?;
        queue_live.set_property("max-size-time", 80_000_000u64);
        queue_live.set_property("max-size-buffers", 2u32);
        queue_live.set_property_from_str("leaky", "downstream");
        let mp4mux = gst::ElementFactory::make("mp4mux")
            .build()
            .map_err(|_| VigilanteError::GStreamer("Failed to create mp4mux".to_string()))?;
        mp4mux.set_property("streamable", &true);
        if mp4mux.find_property("fragment-duration").is_some() {
            mp4mux.set_property("fragment-duration", &120u32);
        } else {
            log::debug!("🔧 mp4mux missing fragment-duration property; skipping override");
        }
        if mp4mux.find_property("reserved-moov-update").is_some() {
            mp4mux.set_property("reserved-moov-update", &true);
        } else {
            log::debug!("🔧 mp4mux missing reserved-moov-update property; skipping override");
        }
        if mp4mux.find_property("presentation-time").is_some() {
            mp4mux.set_property("presentation-time", &true);
        } else {
            log::debug!("🔧 mp4mux missing presentation-time property; skipping override");
        }
        if log::log_enabled!(log::Level::Debug) {
            for pspec in mp4mux.list_properties() {
                log::debug!(
                    "🔧 mp4mux property available: {} ({})",
                    pspec.name(),
                    pspec.value_type().name()
                );
            }
        }
        let appsink_live = gst_app::AppSink::builder().build();
        appsink_live.set_property("emit-signals", &true);
        appsink_live.set_property("sync", &false);
        appsink_live.set_max_buffers(4);
        appsink_live.set_drop(true);

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
                &queue_mjpeg,
                &avdec_mjpeg,
                &videoconvert_mjpeg,
                &videoscale_mjpeg,
                &capsfilter_mjpeg,
                &jpegenc,
                appsink_mjpeg.upcast_ref(),
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

        log::info!("🔧 mp4mux pad templates:");
        for template in mp4mux.pad_template_list() {
            log::info!(
                "🔧 mp4mux pad template name='{}' direction={:?} presence={:?}",
                template.name_template(),
                template.direction(),
                template.presence()
            );
        }

        // Log matroskamux pad templates for debugging
        log::info!("🔧 matroskamux pad templates:");
        for template in mux.pad_template_list() {
            log::info!(
                "🔧 matroskamux pad template name='{}' direction={:?} presence={:?}",
                template.name_template(),
                template.direction(),
                template.presence()
            );
        }

        pipeline.set_state(gst::State::Ready).map_err(|_| {
            VigilanteError::GStreamer("Failed to set pipeline to Ready".to_string())
        })?;

        let mp4mux_audio_pad = mp4mux.request_pad_simple("audio_%u").ok_or_else(|| {
            VigilanteError::GStreamer(
                "Failed to request initial mp4mux audio pad (audio_%u)".to_string(),
            )
        })?;
        log::info!(
            "🔧 Reserved mp4mux audio pad for live branch: {}",
            mp4mux_audio_pad.name()
        );
        let mp4mux_audio_pad_weak = mp4mux_audio_pad.downgrade();

        let rtph264depay_clone = rtph264depay.clone();
        let rtppcmadepay_clone = rtppcmadepay.clone();
        let alawdec_clone = alawdec.clone();
        let audioconvert_clone = audioconvert.clone();
        let audioresample_clone = audioresample.clone();
        let voaacenc_clone = voaacenc.clone();
        let mp4mux_audio_pad_weak_clone = mp4mux_audio_pad_weak.clone();
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
                                            if let Some(mp4mux_audio_pad) =
                                                mp4mux_audio_pad_weak_clone.upgrade()
                                            {
                                                if mp4mux_audio_pad.is_linked() {
                                                    log::info!(
                                                        "🔊 mp4mux audio pad already linked, skipping"
                                                    );
                                                } else {
                                                    let voaacenc_src_pad =
                                                        voaacenc_clone.static_pad("src").unwrap();
                                                    if let Err(e) =
                                                        voaacenc_src_pad.link(&mp4mux_audio_pad)
                                                    {
                                                        log::error!(
                                                            "🔊 Failed to link voaacenc to mp4mux: {:?}",
                                                            e
                                                        );
                                                    } else {
                                                        log::info!(
                                                            "🔊 Successfully linked audio to mp4mux"
                                                        );
                                                    }
                                                }
                                            } else {
                                                log::error!(
                                                    "🔊 mp4mux audio pad weak ref unavailable during linking"
                                                );
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

        // --- Recording branch explicit linking ---
        // queue_rec -> capsfilter_rec
        gst::Element::link_many([&queue_rec, &capsfilter_rec]).map_err(|_| {
            VigilanteError::GStreamer("Failed to link recording queue to capsfilter".to_string())
        })?;

        // capsfilter_rec (src) -> matroskamux (request sink pad video_%u)
        let capsfilter_rec_src = capsfilter_rec
            .static_pad("src")
            .ok_or_else(|| VigilanteError::GStreamer("capsfilter_rec has no src pad".to_string()))?;
        let mux_video_pad = mux
            .request_pad_simple("video_%u")
            .ok_or_else(|| VigilanteError::GStreamer("Failed to request matroskamux video pad (video_%u)".to_string()))?;
        match capsfilter_rec_src.link(&mux_video_pad) {
            Ok(_) => log::info!("🔧 Linked recording capsfilter to matroskamux video pad: {}", mux_video_pad.name()),
            Err(e) => {
                log::error!("❌ Failed to link capsfilter_rec to matroskamux video pad: {:?}", e);
                return Err(VigilanteError::GStreamer("Failed to link recording caps to mux".to_string()));
            }
        }

        // matroskamux (src) -> filesink (sink)
        let mux_src = mux
            .static_pad("src")
            .ok_or_else(|| VigilanteError::GStreamer("matroskamux has no src pad".to_string()))?;
        let filesink_sink = filesink
            .static_pad("sink")
            .ok_or_else(|| VigilanteError::GStreamer("filesink has no sink pad".to_string()))?;
        match mux_src.link(&filesink_sink) {
            Ok(_) => log::info!("🔧 Linked matroskamux src to filesink"),
            Err(e) => {
                log::error!("❌ Failed to link matroskamux to filesink: {:?}", e);
                return Err(VigilanteError::GStreamer("Failed to link mux to filesink".to_string()));
            }
        }

        gst::Element::link_many([
            &queue_motion,
            &avdec_motion,
            &videoconvert_motion,
            &videoscale_motion,
            &capsfilter_motion,
            appsink_motion.upcast_ref(),
        ])
        .map_err(|_| VigilanteError::GStreamer("Failed to link motion branch".to_string()))?;

        // --- MJPEG branch explicit linking ---
        gst::Element::link_many([
            &queue_mjpeg,
            &avdec_mjpeg,
            &videoconvert_mjpeg,
            &videoscale_mjpeg,
            &capsfilter_mjpeg,
            &jpegenc,
            appsink_mjpeg.upcast_ref(),
        ])
        .map_err(|_| VigilanteError::GStreamer("Failed to link MJPEG branch".to_string()))?;

        gst::Element::link_many([&mp4mux, appsink_live.upcast_ref()])
            .map_err(|_| VigilanteError::GStreamer("Failed to link live branch".to_string()))?;

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

        // MJPEG callbacks: forward JPEG frames to broadcast channel
        let mjpeg_tx = self.context.streaming.mjpeg_tx.clone();
        let mjpeg_callbacks = gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = match sink.pull_sample() {
                    Ok(sample) => sample,
                    Err(err) => {
                        log::warn!("🖼️ Failed to pull MJPEG sample: {:?}", err);
                        return Err(gst::FlowError::Error);
                    }
                };

                let buffer = match sample.buffer() {
                    Some(buffer) => buffer,
                    None => {
                        log::warn!("🖼️ MJPEG sample missing buffer");
                        return Ok(gst::FlowSuccess::Ok);
                    }
                };

                let map = match buffer.map_readable() {
                    Ok(map) => map,
                    Err(err) => {
                        log::warn!("🖼️ Failed to map MJPEG buffer: {:?}", err);
                        return Err(gst::FlowError::Error);
                    }
                };

                let data = Bytes::copy_from_slice(map.as_slice());
                let _ = mjpeg_tx.send(data);
                Ok(gst::FlowSuccess::Ok)
            })
            .build();
        appsink_mjpeg.set_callbacks(mjpeg_callbacks);
        log::info!("🖼️ MJPEG appsink callbacks configured");

        let context_weak = Arc::downgrade(&self.context);
        let latency_tracker_for_live = Arc::clone(&latency_tracker);
        let latency_snapshot_handle = Arc::clone(&self.context.streaming.live_latency_snapshot);
        let live_callbacks = gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                log::debug!("📹 Live callback called");
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

                if let Some(pts) = buffer.pts() {
                    let now_instant = Instant::now();
                    if let Ok(mut tracker) = latency_tracker_for_live.lock() {
                        if let Some(update) = tracker.update(pts, now_instant) {
                            if let Ok(mut snapshot_guard) = latency_snapshot_handle.lock() {
                                *snapshot_guard = update.snapshot.clone();
                            }
                            MetricsCollector::record_live_latency(
                                update.latency_ms,
                                update.snapshot.ewma_ms,
                            );
                            if update.should_log {
                                let ewma_display = update
                                    .snapshot
                                    .ewma_ms
                                    .unwrap_or(update.latency_ms);
                                let min_display = update
                                    .snapshot
                                    .min_ms
                                    .unwrap_or(update.latency_ms);
                                let max_display = update
                                    .snapshot
                                    .max_ms
                                    .unwrap_or(update.latency_ms);
                                log::info!(
                                    "⚡ Latencia live {:.1} ms (EWMA {:.1} ms, min {:.1} ms, max {:.1} ms, muestras {})",
                                    update.latency_ms,
                                    ewma_display,
                                    min_display,
                                    max_display,
                                    update.snapshot.samples
                                );
                            }
                        }
                    }
                }

                let map = match buffer.map_readable() {
                    Ok(map) => map,
                    Err(err) => {
                        log::warn!("📹 Failed to map live buffer: {:?}", err);
                        return Err(gst::FlowError::Error);
                    }
                };

                let data = Bytes::copy_from_slice(map.as_slice());

                {
                    let slice = data.as_ref();
                    let mut has_ftyp = slice.windows(4).any(|w| w == b"ftyp");
                    let mut has_moov = slice.windows(4).any(|w| w == b"moov");
                    let mut has_moof = slice.windows(4).any(|w| w == b"moof");

                    {
                        let mut tail_guard = context
                            .streaming
                            .mp4_init_scan_tail
                            .lock()
                            .unwrap();

                        if !has_moov || !has_ftyp || !has_moof {
                            let mut combined = Vec::with_capacity(tail_guard.len() + slice.len());
                            combined.extend_from_slice(&tail_guard);
                            combined.extend_from_slice(slice);
                            if !has_moov {
                                has_moov = combined.windows(4).any(|w| w == b"moov");
                            }
                            if !has_ftyp {
                                has_ftyp = combined.windows(4).any(|w| w == b"ftyp");
                            }
                            if !has_moof {
                                has_moof = combined.windows(4).any(|w| w == b"moof");
                            }
                        }

                        tail_guard.clear();
                        let tail_len = slice.len().min(3);
                        if tail_len > 0 {
                            tail_guard.extend_from_slice(
                                &slice[slice.len().saturating_sub(tail_len)..]
                            );
                        }
                    }

                    let mut segments_guard = context
                        .streaming
                        .mp4_init_segments
                        .lock()
                        .unwrap();
                    let mut complete_guard = context
                        .streaming
                        .mp4_init_complete
                        .lock()
                        .unwrap();
                    let mut warned_guard = context
                        .streaming
                        .mp4_init_warned_before_moov
                        .lock()
                        .unwrap();

                    if !*complete_guard {
                        segments_guard.push(data.clone());

                        if segments_guard.len() <= 6 {
                            let preview_len = slice.len().min(48);
                            let mut hex_preview = String::new();
                            for byte in &slice[..preview_len] {
                                use std::fmt::Write;
                                let _ = write!(&mut hex_preview, "{:02X} ", byte);
                            }
                            log::debug!(
                                "📦 Init fragment {} preview ({} bytes): {}",
                                segments_guard.len(),
                                slice.len(),
                                hex_preview.trim_end()
                            );
                        }

                        if has_moov {
                            *complete_guard = true;
                            let total_size: usize = segments_guard.iter().map(|b| b.len()).sum();
                            log::info!(
                                "📦 Segmentos MP4 iniciales cacheados ({} fragmentos, {} bytes)",
                                segments_guard.len(),
                                total_size
                            );
                        } else if has_moof && !*warned_guard {
                            *warned_guard = true;
                            log::warn!(
                                "📦 Fragmento moof detectado antes de moov ({} fragmentos acumulados)",
                                segments_guard.len()
                            );
                        } else if has_ftyp {
                            log::debug!(
                                "📦 Fragmento ftyp cacheado ({} bytes)",
                                slice.len()
                            );
                        }
                    }
                }

                log::debug!("📹 Live MP4 fragment received, size: {} bytes", data.len());

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

    let tee_src_pad_mjpeg = tee.request_pad_simple("src_%u").unwrap();
    let queue_mjpeg_sink_pad = queue_mjpeg.static_pad("sink").unwrap();
    tee_src_pad_mjpeg.link(&queue_mjpeg_sink_pad).unwrap();
    log::info!("🔧 Linked tee to mjpeg queue");

        let queue_live_src_pad = queue_live.static_pad("src").unwrap();
        let mp4mux_video_pad = mp4mux.request_pad_simple("video_%u").ok_or_else(|| {
            log::error!("🔧 Failed to request mp4mux video pad (video_%u)");
            VigilanteError::GStreamer("Failed to request mp4mux video pad (video_%u)".to_string())
        })?;
        queue_live_src_pad.link(&mp4mux_video_pad).map_err(|err| {
            log::error!(
                "🔧 Failed to link live queue to mp4mux video pad: {:?}",
                err
            );
            VigilanteError::GStreamer("Failed to link live queue to mp4mux".to_string())
        })?;
        log::info!("🔧 Linked live queue to mp4mux video pad");

        let appsink_live_sink_pad = appsink_live
            .static_pad("sink")
            .ok_or_else(|| VigilanteError::GStreamer("Appsink has no sink pad".to_string()))?;
        let appsink_live_sink_pad = appsink_live_sink_pad.downgrade();
        mp4mux.connect_pad_added(move |_, pad| {
            if pad.direction() != gst::PadDirection::Src {
                return;
            }

            if let Some(sink_pad) = appsink_live_sink_pad.upgrade() {
                if sink_pad.is_linked() {
                    log::debug!("� Appsink sink pad already linked, skipping");
                    return;
                }

                match pad.link(&sink_pad) {
                    Ok(_) => log::info!("� Linked mp4mux src pad to live appsink"),
                    Err(err) => log::error!("📺 Failed to link mp4mux src pad: {:?}", err),
                }
            } else {
                log::warn!("� Appsink sink pad no longer available for linking");
            }
        });

        log::info!("🔧 About to set pipeline running flag");
        self.pipeline = Some(pipeline.clone());
        log::info!("🔧 Pipeline stored locally, about to set flag");
        *self.context.gstreamer.pipeline_running.lock().unwrap() = true;
        log::info!("🔧 Pipeline running flag set to true successfully");
        log::info!("🔧 warm_up function completed successfully");
        Ok(())
    }

    pub async fn start_recording(&self) -> Result<()> {
        log::info!(
            "🔧 Start recording called, pipeline: {:?}",
            self.pipeline.is_some()
        );
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
                    // Check if recording files exist and log their status
                    let timestamp = crate::camera::depends::utils::CameraUtils::format_timestamp();
                    let date = timestamp.split('_').next().unwrap();
                    let dir = context.storage_path().join(date);
                    
                    if let Ok(entries) = std::fs::read_dir(&dir) {
                        let mut latest_file = None;
                        let mut latest_modified = std::time::SystemTime::UNIX_EPOCH;
                        
                        for entry in entries.flatten() {
                            if let Some(name) = entry.file_name().to_str() {
                                if name.starts_with(&date) && name.ends_with(".mkv") {
                                    if let Ok(metadata) = entry.metadata() {
                                        if let Ok(modified) = metadata.modified() {
                                            if modified > latest_modified {
                                                latest_modified = modified;
                                                latest_file = Some((name.to_string(), entry.path(), metadata));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        
                        if let Some((filename, path, metadata)) = latest_file {
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
