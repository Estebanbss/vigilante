use crate::AppState;
use gstreamer::{ self as gst, prelude::*, Pipeline, MessageView };
use gstreamer_app as gst_app;
use bytes::Bytes;
use std::sync::Arc;
use std::fs::{OpenOptions};
use std::io::Write;
use chrono::{Local};

// This function will handle the GStreamer pipeline for 24/7 recording and event detection
pub async fn start_camera_pipeline(camera_url: String, state: Arc<AppState>) {
    loop {
        let now = chrono::Local::now();
        // Crear carpeta por día con formato DD-MM-YY (ej: 17-09-25)
        let day_dir_name = now.format("%d-%m-%y").to_string();
        let day_dir = state.storage_path.join(&day_dir_name);
        if let Err(e) = std::fs::create_dir_all(&day_dir) {
            eprintln!("❌ No se pudo crear la carpeta diaria {}: {}", day_dir.display(), e);
        }

        // Archivo diario con nombre YYYY-MM-DD.mp4 dentro de la carpeta del día
        let daily_filename = format!("{}.mp4", now.format("%Y-%m-%d"));
        let daily_path = day_dir.join(&daily_filename);
        
        // Calcular cuánto tiempo falta para medianoche
        let next_midnight = (now + chrono::Duration::days(1))
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_local_timezone(chrono::Local)
            .single()
            .unwrap();
        let duration_until_midnight = (next_midnight - now).to_std().unwrap_or(std::time::Duration::from_secs(24 * 60 * 60));
        
        println!("📹 Iniciando grabación diaria: {} (hasta medianoche: {:?})", daily_filename, duration_until_midnight);
            println!("📁 Carpeta de grabación: {}", day_dir.display());
            println!("🎥 Archivo MP4: {}", daily_path.display());
            println!("📡 URL RTSP: {}", camera_url);

            // Pipeline para grabación continua en archivo diario
            // Single RTSP source with tee to: recording(mp4), detector(appsink), mjpeg(appsink), and optional HLS
            let daily_s = daily_path.to_string_lossy();
            let enable_hls = state.enable_hls;
            let (hls_part, _created): (String, bool) = if enable_hls {
                let hls_dir = state.storage_path.join("hls");
                let mut created = false;
                if let Err(e) = std::fs::create_dir_all(&hls_dir) {
                    eprintln!("❌ No se pudo crear el directorio HLS: {}", e);
                } else {
                    created = true;
                }
                let segments = hls_dir.join("segment-%05d.ts");
                let playlist = hls_dir.join("stream.m3u8");
                let segments_s = segments.to_string_lossy();
                let playlist_s = playlist.to_string_lossy();
                (
                    format!(
                        concat!(
                            // HLS pipeline simplificada: video directo a hlssink2
                            " t. ! queue ! h264parse config-interval=1 ! video/x-h264,stream-format=byte-stream,alignment=au ! hlssink2 target-duration=2 max-files=5 playlist-length=5 location=\"{segments}\" playlist-location=\"{playlist}\" "
                        ),
                        segments = segments_s,
                        playlist = playlist_s,
                    ),
                    created,
                )
            } else { (String::new(), false) };

            let pipeline_str = format!(
                concat!(
                    "rtspsrc location={camera_url} protocols=tcp do-rtsp-keep-alive=true latency=300 retry=5 timeout=20000000000 drop-on-latency=true name=src ",
                    // Tee de video (alimentado dinámicamente en pad-added)
                    "tee name=t ",
                    // Audio se maneja dinámicamente en pad-added de rtspsrc (PCMA/PCMU/OPUS/AAC)
                    // recording branch: MP4 con video Y audio
                    // PASSTHROUGH: usar H.264 directo desde la cámara (evita re-encode y reduce CPU)
                    // Aseguramos formato AVC/au para MP4
                    "t. ! queue ! h264parse config-interval=-1 ! video/x-h264,stream-format=avc,alignment=au,profile=baseline ! mux.video_0 ",
                    // El audio hacia el MP4 se conecta dinámicamente cuando se detecta audio
                    // MP4 fragmentado compatible con streaming progresivo
                    "mp4mux name=mux streamable=true faststart=true fragment-duration=1000 fragment-mode=dash-or-mss ",
                    "! filesink location=\"{daily}\" sync=false append=false ",
                    // detector branch: decodificar a GRAY8 reducido para análisis rápido
                    "t. ! queue leaky=downstream max-size-buffers=1 ! decodebin ! videoconvert ! videoscale ! video/x-raw,format=GRAY8,width=640,height=360 ! appsink name=detector emit-signals=true sync=false max-buffers=1 drop=true ",
                    // mjpeg branch: decode->scale->jpeg->appsink (solo video)
                    // Alta resolución (1080p) con FPS limitado a 15 para suavidad sin excesiva carga
                    "t. ! queue leaky=downstream max-size-buffers=10 max-size-time=300000000 ! decodebin ! videoconvert ! videoscale ! video/x-raw,width=1920,height=1080 ! videorate ! video/x-raw,framerate=15/1 ! jpegenc quality=90 ! appsink name=mjpeg_sink sync=false max-buffers=1 drop=true ",
                    // Rama MJPEG low (480p@10fps) para clientes con menos ancho de banda/CPU
                    "t. ! queue leaky=downstream max-size-buffers=5 max-size-time=200000000 ! decodebin ! videoconvert ! videoscale ! video/x-raw,width=854,height=480 ! videorate ! video/x-raw,framerate=10/1 ! jpegenc quality=70 ! appsink name=mjpeg_low_sink sync=false max-buffers=1 drop=true",
                    "{hls_part}"
                ),
                camera_url = camera_url,
                daily = daily_s,
                hls_part = hls_part,
            );

            println!("📷 Recording pipeline: {}", pipeline_str);
            println!("🔄 Creando pipeline GStreamer...");
            let pipeline = match gst::parse::launch(&pipeline_str) {
                Ok(element) => {
                    println!("✅ Pipeline creado exitosamente");
                    element.downcast::<Pipeline>().unwrap()
                },
                Err(err) => {
                    eprintln!("❌ Error al crear el pipeline: {}", err);
                    eprintln!("🔍 Verificando conexión RTSP...");
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                    continue;
                }
            };

            // Manejo dinámico de pads de rtspsrc para video (H264) y audio (PCMA/PCMU/OPUS/AAC)
            if let Some(rtspsrc) = pipeline.by_name("src") {
                let pipeline_weak = pipeline.downgrade();
                let state_clone = state.clone();
                rtspsrc.connect_pad_added(move |_src, pad| {
                    let Some(pipeline) = pipeline_weak.upgrade() else { return; };
                    let pad_caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
                    let caps_str = pad_caps
                        .structure(0)
                        .map(|s| s.to_string())
                        .unwrap_or_default();

                    let name = pad.name();
                    if !caps_str.contains("application/x-rtp") {
                        // Si no es RTP (caso raro), para evitar errores de no-vinculación, drenar a fakesink
                        eprintln!("ℹ️ Pad no-RTP detectado en rtspsrc: {} - {} -> lo conectamos a fakesink", name, caps_str);
                        let q = gst::ElementFactory::make("queue").name(&format!("q_unknown_{}", name)).build().unwrap();
                        let fs = gst::ElementFactory::make("fakesink").name(&format!("fs_unknown_{}", name)).build().unwrap();
                        fs.set_property_from_str("sync", "false");
                        fs.set_property_from_str("async", "false");
                        pipeline.add_many(&[&q, &fs]).ok();
                        gst::Element::link_many(&[&q, &fs]).ok();
                        if let Some(sinkpad) = q.static_pad("sink") { let _ = pad.link(&sinkpad); }
                        for e in [&q, &fs] { e.sync_state_with_parent().ok(); }
                        return;
                    }

                    if caps_str.contains("media=video") && caps_str.contains("encoding-name=H264") {
                        println!("🎥 Pad de video detectado: {} - {}", name, caps_str);
                        // Construir rama: queue->rtph264depay->h264parse->tee t
                        let qv = gst::ElementFactory::make("queue").name(&format!("qv_{}", name)).build().unwrap();
                        let depay = gst::ElementFactory::make("rtph264depay").name(&format!("vdepay_{}", name)).build().unwrap();
                        let parse = gst::ElementFactory::make("h264parse").name(&format!("vparse_{}", name)).build().unwrap();
                        parse.set_property_from_str("config-interval", "1");
                        pipeline.add_many(&[&qv, &depay, &parse]).ok();
                        gst::Element::link_many(&[&qv, &depay, &parse]).ok();
                        // link a tee t
                        if let Some(t) = pipeline.by_name("t") { if let Some(sinkpad) = t.request_pad_simple("sink_%u") { if let Some(srcpad) = parse.static_pad("src") { let _=srcpad.link(&sinkpad);} } }
                        // Link desde rtspsrc pad a qv
                        if let Some(sinkpad) = qv.static_pad("sink") { let _ = pad.link(&sinkpad); }
                        for e in [&qv, &depay, &parse] { e.sync_state_with_parent().ok(); }
                        println!("✅ Video enlazado a tee principal");
                        return;
                    }

                    // Si la cámara expone otro códec de video que no manejamos, linkear a fakesink para evitar "not-linked"
                    if caps_str.contains("media=video") {
                        eprintln!("⚠️ Video RTP no soportado por ahora: {} -> {}. Conectando a fakesink para mantener RTSP vivo.", name, caps_str);
                        let q = gst::ElementFactory::make("queue").name(&format!("qv_unknown_{}", name)).build().unwrap();
                        let fs = gst::ElementFactory::make("fakesink").name(&format!("fsv_unknown_{}", name)).build().unwrap();
                        fs.set_property_from_str("sync", "false");
                        fs.set_property_from_str("async", "false");
                        pipeline.add_many(&[&q, &fs]).ok();
                        gst::Element::link_many(&[&q, &fs]).ok();
                        if let Some(sinkpad) = q.static_pad("sink") { let _ = pad.link(&sinkpad); }
                        for e in [&q, &fs] { e.sync_state_with_parent().ok(); }
                        return;
                    }

                    if !(caps_str.contains("media=audio")) { return; }
                    println!("🔊 Pad de audio detectado: {} - {}", name, caps_str);

                    // Construimos la rama de audio según el encoding-name
                    let is_pcma = caps_str.contains("encoding-name=PCMA");
                    let is_pcmu = caps_str.contains("encoding-name=PCMU");
                    let is_opus = caps_str.contains("encoding-name=OPUS");
                    let is_aac  = caps_str.contains("encoding-name=MPEG4-GENERIC");

                    // Caso especial AAC por RTP: usar passthrough a MP4 (sin re-encode)
                    if is_aac {
                        println!("🔊 AAC por RTP detectado, conectando passthrough a MP4 (mux.audio_0)");
                        let q = gst::ElementFactory::make("queue").name(&format!("qa_{}", name)).build().unwrap();
                        let depay = gst::ElementFactory::make("rtpmp4gdepay").name(&format!("depay_{}", name)).build().unwrap();
                        let aacparse = gst::ElementFactory::make("aacparse").name(&format!("aacparse_{}", name)).build().unwrap();
                        pipeline.add_many(&[&q, &depay, &aacparse]).ok();
                        gst::Element::link_many(&[&q, &depay, &aacparse]).ok();
                        if let Some(sinkpad) = q.static_pad("sink") { let _ = pad.link(&sinkpad); }
                        if let Some(mux) = pipeline.by_name("mux") {
                            if let Some(sinkpad) = mux.request_pad_simple("audio_0") {
                                if let Some(srcpad) = aacparse.static_pad("src") { let _ = srcpad.link(&sinkpad); }
                            }
                        }
                        for e in [&q, &depay, &aacparse] { e.sync_state_with_parent().ok(); }
                        println!("✅ Audio AAC conectado a MP4");
                        return;
                    }

                    // Crear elementos para PCMA/PCMU/OPUS (decodificar a raw y luego dividir a AAC(opcional)/Opus)
                    let queue = gst::ElementFactory::make("queue").name(&format!("qa_{}", name)).build().ok();
                    let depay = if is_pcma {
                        gst::ElementFactory::make("rtppcmadepay").name(&format!("depay_{}", name)).build().ok()
                    } else if is_pcmu {
                        gst::ElementFactory::make("rtppcmudepay").name(&format!("depay_{}", name)).build().ok()
                    } else if is_opus {
                        gst::ElementFactory::make("rtpopusdepay").name(&format!("depay_{}", name)).build().ok()
                    } else { None };

                    if queue.is_none() || depay.is_none() {
                        // Audio no soportado -> fakesink
                        eprintln!("⚠️ Audio RTP no soportado por ahora: {} -> {}. Conectando a fakesink para mantener RTSP vivo.", name, caps_str);
                        let q = gst::ElementFactory::make("queue").name(&format!("qa_unknown_{}", name)).build().unwrap();
                        let fs = gst::ElementFactory::make("fakesink").name(&format!("fsa_unknown_{}", name)).build().unwrap();
                        fs.set_property_from_str("sync", "false");
                        fs.set_property_from_str("async", "false");
                        pipeline.add_many(&[&q, &fs]).ok();
                        gst::Element::link_many(&[&q, &fs]).ok();
                        if let Some(sinkpad) = q.static_pad("sink") { let _ = pad.link(&sinkpad); }
                        for e in [&q, &fs] { e.sync_state_with_parent().ok(); }
                        return;
                    }

                    let queue = queue.unwrap();
                    let depay = depay.unwrap();
                    let alawdec = if is_pcma { Some(gst::ElementFactory::make("alawdec").build().unwrap()) } else { None };
                    let mulawdec = if is_pcmu { Some(gst::ElementFactory::make("mulawdec").build().unwrap()) } else { None };
                    let opusdec = if is_opus { Some(gst::ElementFactory::make("opusdec").build().unwrap()) } else { None };
                    let audioconvert = gst::ElementFactory::make("audioconvert").build().unwrap();
                    let audioresample = gst::ElementFactory::make("audioresample").build().unwrap();
                    let capsfilter = gst::ElementFactory::make("capsfilter").build().unwrap();
                    let _ = capsfilter.set_property(
                        "caps",
                        &gst::Caps::builder("audio/x-raw")
                            .field("rate", 48000i32)
                            .field("channels", 2i32)
                            .build(),
                    );
                    let tee = gst::ElementFactory::make("tee").name(&format!("tee_audio_dyn_{}", name)).build().unwrap();

                    // Rama AAC para MP4 (re-encode)
                    let qa1 = gst::ElementFactory::make("queue").build().unwrap();
                    let voaacenc = gst::ElementFactory::make("voaacenc").build().unwrap();
                    voaacenc.set_property_from_str("bitrate", "128000");
                    let aacparse = gst::ElementFactory::make("aacparse").build().unwrap();

                    // Rama Opus/WebM para live audio
                    let qa2 = gst::ElementFactory::make("queue").build().unwrap();
                    let opusenc = gst::ElementFactory::make("opusenc").build().unwrap();
                    opusenc.set_property_from_str("bitrate", "64000");
                    let webmmux = gst::ElementFactory::make("webmmux").name(&format!("webma_dyn_{}", name)).build().unwrap();
                    webmmux.set_property_from_str("streamable", "true");
                    let audio_sink = gst::ElementFactory::make("appsink").name(&format!("audio_webm_sink_dyn_{}", name)).build().unwrap();
                    audio_sink.set_property_from_str("sync", "false");
                    audio_sink.set_property_from_str("max-buffers", "50");
                    audio_sink.set_property_from_str("drop", "true");

                    // Agregar y callbacks
                    let audio_sink_app = audio_sink.clone().downcast::<gst_app::AppSink>().unwrap();
                    let txa = state_clone.audio_webm_tx.clone();
                    audio_sink_app.set_callbacks(
                        gst_app::AppSinkCallbacks::builder()
                            .new_sample(move |s| {
                                let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                                let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                                let data = Bytes::copy_from_slice(map.as_ref());
                                let _ = txa.send(data);
                                Ok(gst::FlowSuccess::Ok)
                            })
                            .build(),
                    );

                    let mut all_elements: Vec<gst::Element> = vec![
                        queue.clone().upcast(),
                        depay.clone().upcast(),
                    ];
                    if let Some(ref d) = alawdec { all_elements.push(d.clone().upcast()); }
                    if let Some(ref d) = mulawdec { all_elements.push(d.clone().upcast()); }
                    if let Some(ref d) = opusdec { all_elements.push(d.clone().upcast()); }
                    all_elements.extend_from_slice(&[audioconvert.clone().upcast(), audioresample.clone().upcast(), capsfilter.clone().upcast(), tee.clone().upcast(), qa1.clone().upcast(), voaacenc.clone().upcast(), aacparse.clone().upcast(), qa2.clone().upcast(), opusenc.clone().upcast(), webmmux.clone().upcast(), audio_sink.clone().upcast()]);

                    pipeline.add_many(&all_elements).ok();

                    // Enlaces
                    gst::Element::link_many(&[&queue, &depay]).ok();
                    let mut last: gst::Element = depay.clone();
                    if let Some(ref d) = alawdec { gst::Element::link_many(&[&last, d]).ok(); last = d.clone(); }
                    if let Some(ref d) = mulawdec { gst::Element::link_many(&[&last, d]).ok(); last = d.clone(); }
                    if let Some(ref d) = opusdec { gst::Element::link_many(&[&last, d]).ok(); last = d.clone(); }
                    gst::Element::link_many(&[&last, &audioconvert, &audioresample, &capsfilter, &tee]).ok();

                    // Rama 1: MP4 (AAC)
                    gst::Element::link_many(&[&tee, &qa1, &voaacenc, &aacparse]).ok();
                    if let Some(mux) = pipeline.by_name("mux") {
                        if let Some(sinkpad) = mux.request_pad_simple("audio_0") {
                            if let Some(srcpad) = aacparse.static_pad("src") { let _ = srcpad.link(&sinkpad); }
                        }
                    }

                    // Rama 2: WebM/Opus
                    gst::Element::link_many(&[&tee, &qa2, &opusenc, &webmmux]).ok();
                    if let Some(srcpad) = webmmux.static_pad("src") {
                        if let Some(sinkpad) = audio_sink.static_pad("sink") { let _ = srcpad.link(&sinkpad); }
                    }

                    // Conectar el pad de rtspsrc a la cola inicial
                    if let Some(sinkpad) = queue.static_pad("sink") { let _ = pad.link(&sinkpad); }

                    for e in all_elements { e.sync_state_with_parent().ok(); }
                    println!("🔊 Audio dinámico enlazado: {}", caps_str);
                });
            }
        
            // Get the appsink element for event detection
            let appsink = pipeline
                .by_name("detector")
                .unwrap()
                .downcast::<gst_app::AppSink>()
                .unwrap();

            // Set appsink callbacks to analyze frames
            let state_for_cb = state.clone();
            use std::sync::{Mutex as StdMutex};
            use std::time::{Instant};
            let prev_frame: Arc<StdMutex<Option<Vec<u8>>>> = Arc::new(StdMutex::new(None));
            let last_event_time: Arc<StdMutex<Option<Instant>>> = Arc::new(StdMutex::new(None));
            let prev_frame_cb = prev_frame.clone();
            let last_event_time_cb = last_event_time.clone();
            appsink.set_callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |appsink| {
                        let sample = appsink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                        let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;

                        // Obtener dimensiones desde caps
                        let caps = sample.caps().ok_or(gst::FlowError::Error)?;
                        let s = caps.structure(0).ok_or(gst::FlowError::Error)?;
                        let width: i32 = s.get("width").unwrap_or(640);
                        let height: i32 = s.get("height").unwrap_or(360);
                        let _format: &str = s.get::<&str>("format").unwrap_or("GRAY8");

                        let frame = map.as_ref(); // GRAY8 plano

                        // Motion detection simple: diferencia absoluta promedio contra frame previo
                        let mut has_movement = false;
                        if frame.len() == (width as usize * height as usize) {
                            let mut prev_guard = prev_frame_cb.lock().unwrap();
                            if let Some(prev) = prev_guard.as_ref() {
                                let mut acc: u64 = 0;
                                // muestreo: cada 4 pixeles para reducir costo
                                let mut i = 0usize;
                                let mut count = 0u64;
                                let len = frame.len();
                                while i < len {
                                    let d = frame[i].abs_diff(prev[i]) as u64;
                                    acc += d;
                                    count += 1;
                                    i += 4; // sample step
                                }
                                if count > 0 {
                                    let mean = (acc as f64) / (count as f64);
                                    // umbral empírico
                                    has_movement = mean > 8.0;
                                }
                            }
                            // actualiza previo (copia)
                            *prev_guard = Some(frame.to_vec());
                        }

                        // Anti-spam: mínimo 2s entre eventos
                        let mut should_log = false;
                        if has_movement {
                            let mut last_guard = last_event_time_cb.lock().unwrap();
                            let now_i = Instant::now();
                            if let Some(last) = *last_guard {
                                if now_i.duration_since(last).as_millis() > 2000 { should_log = true; *last_guard = Some(now_i); }
                            } else { should_log = true; *last_guard = Some(now_i); }
                        }

                        if should_log {
                            let now = Local::now();
                            // Carpeta del día (DD-MM-YY) y archivo de log (YYYY-MM-DD-log.txt)
                            let day_dir_name = now.format("%d-%m-%y").to_string();
                            let day_dir = state_for_cb.storage_path.join(&day_dir_name);
                            if let Err(e) = std::fs::create_dir_all(&day_dir) {
                                eprintln!("❌ No se pudo crear la carpeta diaria {}: {}", day_dir.display(), e);
                            }
                            let log_filename = now.format("%Y-%m-%d-log.txt").to_string();
                            let log_path = day_dir.join(&log_filename);
                            if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&log_path) {
                                let timestamp = now.format("%H:%M:%S.%3f").to_string();
                                writeln!(file, "{} - se detectó movimiento", timestamp).ok();
                            }
                        }
                    
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );

            // MJPEG appsink: publish JPEG frames to broadcast channel
            if let Some(mjpeg_sink) = pipeline.by_name("mjpeg_sink") {
                let mjpeg_sink = mjpeg_sink.downcast::<gst_app::AppSink>().unwrap();
                let tx = state.mjpeg_tx.clone();
                mjpeg_sink.set_caps(Some(&gst::Caps::builder("image/jpeg").build()));
                mjpeg_sink.set_callbacks(
                    gst_app::AppSinkCallbacks::builder()
                        .new_sample(move |s| {
                            let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                            let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                            let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                            let data = Bytes::copy_from_slice(map.as_ref());
                            let _ = tx.send(data); // best-effort broadcast
                            Ok(gst::FlowSuccess::Ok)
                        })
                        .build(),
                );
            }

            // MJPEG LOW appsink: publish to low-quality broadcast channel
            if let Some(mjpeg_low_sink) = pipeline.by_name("mjpeg_low_sink") {
                let mjpeg_low_sink = mjpeg_low_sink.downcast::<gst_app::AppSink>().unwrap();
                let tx_low = state.mjpeg_low_tx.clone();
                mjpeg_low_sink.set_caps(Some(&gst::Caps::builder("image/jpeg").build()));
                mjpeg_low_sink.set_callbacks(
                    gst_app::AppSinkCallbacks::builder()
                        .new_sample(move |s| {
                            let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                            let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                            let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                            let data = Bytes::copy_from_slice(map.as_ref());
                            let _ = tx_low.send(data);
                            Ok(gst::FlowSuccess::Ok)
                        })
                        .build(),
                );
            }

            // Get the bus to receive messages from the pipeline
            let bus = pipeline.bus().unwrap();

            // Set the pipeline to "playing" state
            println!("▶️ Iniciando pipeline...");
            match pipeline.set_state(gst::State::Playing) {
                Ok(_) => println!("✅ Pipeline iniciado correctamente"),
                Err(e) => {
                    eprintln!("❌ Error al iniciar pipeline: {}", e);
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
            }

            // Store the pipeline in the shared state
            *state.pipeline.lock().await = Some(pipeline.clone());

            println!("🎬 Grabación activa - esperando datos...");
        
            // Verificar que el archivo está creciendo después de unos segundos
            tokio::spawn({
                let daily_path = daily_path.clone();
                async move {
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                    if let Ok(metadata) = std::fs::metadata(&daily_path) {
                        let size = metadata.len();
                        if size > 0 {
                            println!("✅ Archivo MP4 está creciendo: {} bytes", size);
                        } else {
                            eprintln!("⚠️ Archivo MP4 sigue vacío después de 10s - posible problema RTSP");
                        }
                    } else {
                        eprintln!("❌ No se puede leer el archivo MP4");
                    }
                }
            });

            // Esperar hasta medianoche o hasta que haya un error
            let start_time = std::time::Instant::now();
            let mut should_restart = false;
        
            while start_time.elapsed() < duration_until_midnight {
                let mut iter = bus.iter_timed(gst::ClockTime::from_seconds(1));
                match iter.next() {
                    Some(msg) => match msg.view() {
                        MessageView::Eos(_) => {
                            println!("⏹️ Fin del stream (EOS), reiniciando...");
                            should_restart = true;
                            break;
                        }
                        MessageView::Error(err) => {
                            eprintln!("❌ Error del pipeline: {}", err.error());
                            eprintln!("🔍 Debug info: {:?}", err.debug());
                            should_restart = true;
                            break;
                        }
                        MessageView::Warning(warn) => {
                            eprintln!("⚠️ Warning del pipeline: {}", warn.error());
                            eprintln!("🔍 Debug info: {:?}", warn.debug());
                        }
                        MessageView::StateChanged(state_change) => {
                            if let Some(src) = state_change.src().and_then(|s| s.downcast_ref::<gst::Element>()) {
                                if src.name().starts_with("rtspsrc") {
                                    println!("🔄 RTSP state: {:?} -> {:?}", 
                                        state_change.old(), state_change.current());
                                }
                            }
                        }
                        MessageView::StreamStart(_) => {
                            println!("🎬 Stream iniciado correctamente");
                        }
                        _ => (),
                    },
                    None => {
                        // Timeout normal, continuar
                    }
                }
            
                // Pequeña pausa para no sobrecargar el CPU
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }

            // Detener el pipeline actual
            println!("🛑 Deteniendo pipeline...");
            let _ = pipeline.set_state(gst::State::Null);
        
            if should_restart {
                println!("🔄 Reiniciando pipeline por error...");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            } else {
                println!("🕛 Medianoche alcanzada, creando nuevo archivo diario...");
            }
        }
    }

