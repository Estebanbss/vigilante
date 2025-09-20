use crate::AppState;
use gstreamer::{ self as gst, prelude::*, Pipeline, MessageView };
use gstreamer_app as gst_app;
use bytes::Bytes;
use std::sync::Arc;
use std::fs::{OpenOptions};
use std::io::Write;
use chrono::{Local};

pub async fn start_camera_pipeline(camera_url: String, state: Arc<AppState>) {
    loop {
        let now = chrono::Local::now();
        
        // Crear carpeta por día con formato DD-MM-YY
        let day_dir_name = now.format("%d-%m-%y").to_string();
        let mut day_dir = state.storage_path.join(&day_dir_name);
        
        // Intentar crear el directorio, si falla usar /tmp como fallback
        if let Err(e) = std::fs::create_dir_all(&day_dir) {
            eprintln!("❌ No se pudo crear la carpeta {}: {}. Usando /tmp como fallback", day_dir.display(), e);
            day_dir = std::path::PathBuf::from("/tmp").join("vigilante").join(&day_dir_name);
            if let Err(e) = std::fs::create_dir_all(&day_dir) {
                eprintln!("❌ Tampoco se pudo crear el directorio fallback {}: {}", day_dir.display(), e);
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                continue;
            }
        }

        // Archivo diario con nombre YYYY-MM-DD.mp4
        let daily_filename = format!("{}.mp4", now.format("%Y-%m-%d"));
        let daily_path = day_dir.join(&daily_filename);
        
        // Calcular tiempo hasta medianoche
        let next_midnight = (now + chrono::Duration::days(1))
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_local_timezone(chrono::Local)
            .single()
            .unwrap();
        let duration_until_midnight = (next_midnight - now).to_std()
            .unwrap_or(std::time::Duration::from_secs(24 * 60 * 60));
        
        println!("📹 Iniciando grabación diaria: {} (hasta medianoche: {:?})", daily_filename, duration_until_midnight);
        println!("📁 Carpeta de grabación: {}", day_dir.display());
        println!("🎥 Archivo MP4: {}", daily_path.display());
        println!("📡 URL RTSP: {}", camera_url);

        let daily_s = daily_path.to_string_lossy();

        // Pipeline con audio y video
        let pipeline_str = format!(
            concat!(
                // Fuente RTSP
                "rtspsrc location={} latency=2000 protocols=tcp name=src ",
                
                // Video: H.264 depayload
                "src. ! rtph264depay ! h264parse ! tee name=t_video ",
                
                // Branch 1: Recording to MP4 con muxer compartido
                "t_video. ! queue leaky=2 max-size-buffers=100 max-size-time=5000000000 ! ",
                "h264parse config-interval=-1 ! mp4mux name=mux ! filesink location=\"{}\" sync=false ",
                
                // Branch 2: Motion detection (decode + grayscale)
                "t_video. ! queue leaky=2 max-size-buffers=3 ! ",
                "avdec_h264 ! videoconvert ! videoscale ! ",
                "video/x-raw,format=GRAY8,width=320,height=180 ! ",
                "appsink name=detector emit-signals=true sync=false max-buffers=1 drop=true ",
                
                // Branch 3: MJPEG streaming (decode + encode)
                "t_video. ! queue leaky=2 max-size-buffers=5 ! ",
                "avdec_h264 ! videoconvert ! videoscale ! ",
                "video/x-raw,width=1280,height=720 ! videorate ! video/x-raw,framerate=15/1 ! ",
                "jpegenc quality=85 ! appsink name=mjpeg_sink sync=false max-buffers=1 drop=true ",
                
                // Branch 4: MJPEG low quality
                "t_video. ! queue leaky=2 max-size-buffers=3 ! ",
                "avdec_h264 ! videoconvert ! videoscale ! ",
                "video/x-raw,width=640,height=360 ! videorate ! video/x-raw,framerate=8/1 ! ",
                "jpegenc quality=70 ! appsink name=mjpeg_low_sink sync=false max-buffers=1 drop=true "
                
                // Audio se conectará dinámicamente cuando aparezca el pad
            ),
            camera_url, daily_s
        );

        println!("📷 Pipeline: {}", pipeline_str);
        println!("🔄 Creando pipeline GStreamer...");
        
        let pipeline = match gst::parse::launch(&pipeline_str) {
            Ok(element) => {
                println!("✅ Pipeline creado exitosamente");
                element.downcast::<Pipeline>().unwrap()
            },
            Err(err) => {
                eprintln!("❌ Error al crear el pipeline: {}", err);
                eprintln!("🔍 Reintentando en 10 segundos...");
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                continue;
            }
        };

        // Configurar dynamic pads para rtspsrc
        setup_dynamic_audio(&pipeline, &state);
        
        // Configurar motion detection
        setup_motion_detection(&pipeline, &state);
        
        // Configurar MJPEG sinks
        setup_mjpeg_sinks(&pipeline, &state);

        // Get bus
        let bus = pipeline.bus().unwrap();

        // Iniciar pipeline
        println!("▶️ Iniciando pipeline...");
        match pipeline.set_state(gst::State::Playing) {
            Ok(_) => println!("✅ Pipeline iniciado correctamente"),
            Err(e) => {
                eprintln!("❌ Error al iniciar pipeline: {}", e);
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        }

        // Guardar pipeline en estado compartido
        *state.pipeline.lock().await = Some(pipeline.clone());

        println!("🎬 Grabación activa - esperando datos...");
    
        // Verificar archivo después de 15 segundos
        tokio::spawn({
            let daily_path = daily_path.clone();
            async move {
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                if let Ok(metadata) = std::fs::metadata(&daily_path) {
                    let size = metadata.len();
                    if size > 1000 {
                        println!("✅ Archivo MP4 está creciendo: {} bytes", size);
                    } else {
                        eprintln!("⚠️ Archivo MP4 muy pequeño: {} bytes", size);
                    }
                } else {
                    eprintln!("❌ No se puede acceder al archivo MP4");
                }
            }
        });

        // Loop principal - esperar hasta medianoche o error
        let start_time = std::time::Instant::now();
        let mut should_restart = false;
    
        while start_time.elapsed() < duration_until_midnight {
            // Procesar mensajes del bus con timeout de 1 segundo
            let mut iter = bus.iter_timed(gst::ClockTime::from_seconds(1));
            
            match iter.next() {
                Some(msg) => {
                    match msg.view() {
                        MessageView::Eos(_) => {
                            println!("⏹️ Fin del stream (EOS), reiniciando...");
                            should_restart = true;
                            break;
                        }
                        MessageView::Error(err) => {
                            eprintln!("❌ Error del pipeline: {}", err.error());
                            if let Some(debug) = err.debug() {
                                eprintln!("🔍 Debug: {}", debug);
                            }
                            should_restart = true;
                            break;
                        }
                        MessageView::Warning(warn) => {
                            eprintln!("⚠️ Warning: {}", warn.error());
                        }
                        MessageView::StreamStart(_) => {
                            println!("🎬 Stream iniciado correctamente");
                        }
                        MessageView::StateChanged(sc) => {
                            if let Some(src) = sc.src() {
                                let name = src.name();
                                if name.contains("rtspsrc") {
                                    println!("🔄 RTSP state: {:?} -> {:?}", sc.old(), sc.current());
                                }
                            }
                        }
                        _ => {}
                    }
                }
                None => {
                    // Timeout normal - continuar
                }
            }
        
            // Pausa pequeña para no sobrecargar CPU
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        // Detener pipeline
        println!("🛑 Deteniendo pipeline...");
        let _ = pipeline.set_state(gst::State::Null);
    
        if should_restart {
            println!("🔄 Reiniciando por error en 5 segundos...");
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        } else {
            println!("🕛 Nuevo día - creando nuevo archivo...");
        }
    }
}

fn setup_dynamic_audio(pipeline: &Pipeline, state: &Arc<AppState>) {
    let Some(rtspsrc) = pipeline.by_name("src") else {
        eprintln!("❌ No se encontró rtspsrc");
        return;
    };

    let pipeline_weak = pipeline.downgrade();
    let state_clone = state.clone();
    
    rtspsrc.connect_pad_added(move |_src, src_pad| {
        let Some(pipeline) = pipeline_weak.upgrade() else { return; };
        
        let pad_caps = src_pad.current_caps()
            .or_else(|| Some(src_pad.query_caps(None::<&gst::Caps>)));
        
        if let Some(caps) = pad_caps {
            let structure = caps.structure(0).unwrap();
            let media_type = structure.name();
            
            if media_type.starts_with("application/x-rtp") {
                if let Ok(encoding_name) = structure.get::<&str>("encoding-name") {
                    match encoding_name {
                        "H264" => {
                            println!("🎥 Pad de video H.264 detectado (ya conectado)");
                        }
                        "PCMA" | "PCMU" | "L16" | "OPUS" | "MP4A-LATM" => {
                            println!("🎵 Pad de audio detectado: {}", encoding_name);
                            if let Err(e) = handle_audio_pad(&pipeline, src_pad, encoding_name, &state_clone) {
                                eprintln!("⚠️ Error configurando audio: {}", e);
                            }
                        }
                        _ => {
                            println!("🔍 Pad desconocido: {}", encoding_name);
                        }
                    }
                }
            }
        }
    });
}

fn handle_audio_pad(pipeline: &Pipeline, src_pad: &gst::Pad, encoding: &str, state: &Arc<AppState>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (depayloader_name, decoder_name) = match encoding {
        "PCMA" => ("rtppcmadepay", Some("alawdec")),
        "PCMU" => ("rtppcmudepay", Some("mulawdec")), 
        "L16" => ("rtpL16depay", None),
        "OPUS" => ("rtpopusdepay", None),
        "MP4A-LATM" => ("rtpmp4adepay", None),
        _ => return Ok(())
    };

    println!("🔊 Configurando audio: {} -> {}", encoding, depayloader_name);

    // Crear elementos de audio dinámicamente
    let depay = gst::ElementFactory::make(depayloader_name).build()?;
    let convert = gst::ElementFactory::make("audioconvert").build()?;
    let resample = gst::ElementFactory::make("audioresample").build()?;
    let tee = gst::ElementFactory::make("tee").name("tee_audio").build()?;

    // Crear decoder si es necesario para G.711
    if let Some(decoder_name) = decoder_name {
        let decoder = gst::ElementFactory::make(decoder_name).build()?;
        pipeline.add_many([&depay, &decoder, &convert, &resample, &tee])?;
        gst::Element::link_many([&depay, &decoder, &convert, &resample, &tee])?;
    } else {
        pipeline.add_many([&depay, &convert, &resample, &tee])?;
        gst::Element::link_many([&depay, &convert, &resample, &tee])?;
    }

    // Conectar el pad de origen
    let sink_pad = depay.static_pad("sink").unwrap();
    src_pad.link(&sink_pad)?;

    // Crear branches de audio
    create_audio_branches(pipeline, &tee, state)?;

    // Sincronizar estado
    depay.sync_state_with_parent()?;
    if let Some(decoder_name) = decoder_name {
        // Sincronizar el decoder si existe
        if let Some(decoder) = pipeline.children().iter().find(|e| {
            e.name().starts_with(decoder_name)
        }) {
            decoder.sync_state_with_parent()?;
        }
    }
    convert.sync_state_with_parent()?;
    resample.sync_state_with_parent()?;
    tee.sync_state_with_parent()?;

    println!("✅ Audio configurado correctamente");
    Ok(())
}

fn create_audio_branches(pipeline: &Pipeline, tee: &gst::Element, state: &Arc<AppState>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Branch 1: AAC para grabación MP4
    // Create queue without leaky property to avoid enum binding issues
    let queue1 = gst::ElementFactory::make("queue")
        .property("max-size-buffers", 10u32)
        .property("max-size-time", 3000000000u64) // 3 seconds max
        .build()?;
    
    let aacenc = gst::ElementFactory::make("voaacenc")
        .property("bitrate", 64000i32) // 64kbps para buena calidad
        .build()?;
    
    pipeline.add_many([&queue1, &aacenc])?;
    gst::Element::link_many([&queue1, &aacenc])?;
    
    let tee_pad1 = tee.request_pad_simple("src_%u").unwrap();
    let queue_pad1 = queue1.static_pad("sink").unwrap();
    tee_pad1.link(&queue_pad1)?;
    
    // Conectar al mux del MP4
    if let Some(mux) = pipeline.by_name("mux") {
        if let (Some(aac_src), Some(mux_sink)) = (
            aacenc.static_pad("src"),
            mux.request_pad_simple("audio_0")
        ) {
            if let Err(e) = aac_src.link(&mux_sink) {
                eprintln!("⚠️ Error conectando audio AAC al MP4: {}", e);
            } else {
                println!("🔗 Audio AAC conectado al MP4");
            }
        } else {
            eprintln!("⚠️ No se pudieron obtener los pads para conectar audio al MP4");
        }
    }

    // Branch 2: Opus para streaming en tiempo real
    let queue2 = gst::ElementFactory::make("queue")
        .property("max-size-buffers", 5u32)
        .property("max-size-time", 2000000000u64) // 2 seconds max
        .build()?;
    
    let opusenc = gst::ElementFactory::make("opusenc")
        .property("bitrate", 32000i32) // 32kbps para streaming eficiente
        .build()?;
    let webmmux = gst::ElementFactory::make("webmmux")
        .property("streamable", true)
        .build()?;
    let appsink = gst::ElementFactory::make("appsink")
        .name("audio_webm_sink")
        .property("sync", false)
        .property("max-buffers", 20u32)
        .property("drop", true)
        .build()?;

    pipeline.add_many([&queue2, &opusenc, &webmmux, &appsink])?;
    gst::Element::link_many([&queue2, &opusenc, &webmmux, &appsink])?;

    let tee_pad2 = tee.request_pad_simple("src_%u").unwrap();
    let queue_pad2 = queue2.static_pad("sink").unwrap();
    tee_pad2.link(&queue_pad2)?;

    // Configurar callback para audio WebM streaming
    let audio_sink_app = appsink.clone().downcast::<gst_app::AppSink>().unwrap();
    let tx_audio = state.audio_webm_tx.clone();
    audio_sink_app.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |s| {
                let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                let data = Bytes::copy_from_slice(map.as_ref());
                let _ = tx_audio.send(data); // Best effort broadcast
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );

    // Sincronizar estados
    for element in [&queue1, &aacenc, &queue2, &opusenc, &webmmux, &appsink] {
        element.sync_state_with_parent()?;
    }

    println!("🎵 Branches de audio creados: AAC para MP4, Opus para streaming");
    Ok(())
}

fn setup_motion_detection(pipeline: &Pipeline, state: &Arc<AppState>) {
    let Some(appsink) = pipeline.by_name("detector") else {
        eprintln!("❌ No se encontró el appsink 'detector'");
        return;
    };
    
    let Ok(appsink) = appsink.downcast::<gst_app::AppSink>() else {
        eprintln!("❌ Error al convertir detector a AppSink");
        return;
    };

    let state_for_cb = state.clone();
    use std::sync::{Mutex as StdMutex};
    use std::time::Instant;
    
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

                // Obtener dimensiones
                let caps = sample.caps().ok_or(gst::FlowError::Error)?;
                let s = caps.structure(0).ok_or(gst::FlowError::Error)?;
                let width: i32 = s.get("width").unwrap_or(320);
                let height: i32 = s.get("height").unwrap_or(180);

                let frame = map.as_ref();
                let expected_size = (width * height) as usize;
                
                if frame.len() != expected_size {
                    return Ok(gst::FlowSuccess::Ok); // Skip malformed frame
                }

                // Detección de movimiento simple
                let mut has_movement = false;
                let mut prev_guard = prev_frame_cb.lock().unwrap();
                
                if let Some(prev) = prev_guard.as_ref() {
                    if prev.len() == frame.len() {
                        let mut acc: u64 = 0;
                        let mut count = 0u64;
                        
                        // Muestrear cada 16 píxeles para eficiencia
                        for i in (0..frame.len()).step_by(16) {
                            let diff = frame[i].abs_diff(prev[i]) as u64;
                            acc += diff;
                            count += 1;
                        }
                        
                        if count > 0 {
                            let mean_diff = (acc as f64) / (count as f64);
                            has_movement = mean_diff > 12.0; // Threshold ajustable
                        }
                    }
                }
                
                // Actualizar frame previo
                *prev_guard = Some(frame.to_vec());
                drop(prev_guard);

                // Log de eventos con anti-spam
                if has_movement {
                    let mut should_log = false;
                    let mut last_guard = last_event_time_cb.lock().unwrap();
                    let now_i = Instant::now();
                    
                    if let Some(last) = *last_guard {
                        if now_i.duration_since(last).as_secs() >= 3 {
                            should_log = true;
                            *last_guard = Some(now_i);
                        }
                    } else {
                        should_log = true;
                        *last_guard = Some(now_i);
                    }
                    drop(last_guard);

                    if should_log {
                        let now = Local::now();
                        let day_dir_name = now.format("%d-%m-%y").to_string();
                        let mut day_dir = state_for_cb.storage_path.join(&day_dir_name);
                        
                        // Fallback si no existe el directorio
                        if !day_dir.exists() {
                            day_dir = std::path::PathBuf::from("/tmp").join("vigilante").join(&day_dir_name);
                        }
                        
                        let log_filename = now.format("%Y-%m-%d-log.txt").to_string();
                        let log_path = day_dir.join(&log_filename);
                        
                        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&log_path) {
                            let timestamp = now.format("%H:%M:%S").to_string();
                            let _ = writeln!(file, "{} - Movimiento detectado", timestamp);
                        }
                        
                        println!("🚶 Movimiento detectado a las {}", now.format("%H:%M:%S"));
                    }
                }
            
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
}

fn setup_mjpeg_sinks(pipeline: &Pipeline, state: &Arc<AppState>) {
    // MJPEG alta calidad
    if let Some(mjpeg_sink) = pipeline.by_name("mjpeg_sink") {
        if let Ok(mjpeg_sink) = mjpeg_sink.downcast::<gst_app::AppSink>() {
            let tx = state.mjpeg_tx.clone();
            mjpeg_sink.set_callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |s| {
                        let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                        let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                        let data = Bytes::copy_from_slice(map.as_ref());
                        let _ = tx.send(data); // Best effort
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );
        }
    }

    // MJPEG baja calidad
    if let Some(mjpeg_low_sink) = pipeline.by_name("mjpeg_low_sink") {
        if let Ok(mjpeg_low_sink) = mjpeg_low_sink.downcast::<gst_app::AppSink>() {
            let tx_low = state.mjpeg_low_tx.clone();
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
    }
}