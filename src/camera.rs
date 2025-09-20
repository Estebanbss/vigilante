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
            // Single RTSP source with tee to: recording(mp4), detector(appsink), mjpeg(appsink)
            let daily_s = daily_path.to_string_lossy();
            let enable_hls = false; // Deshabilitado para simplificar
            let (segments_s, playlist_s) = (String::new(), String::new());

            let pipeline_str = format!(
                concat!(
                    "uridecodebin3 uri={} name=src ",
                    "tee name=t ",
                    "tee name=tee_audio ",
                    "t. ! queue leaky=downstream ! videoconvert ! x264enc bitrate=500 ! h264parse config-interval=-1 ! video/x-h264,stream-format=avc,alignment=au ! mux.video_0 ",
                    "mp4mux name=mux streamable=true faststart=true fragment-duration=1000 fragment-mode=dash-or-mss ! filesink location=\"{}\" sync=false append=false ",
                    "t. ! queue leaky=downstream max-size-buffers=1 ! videoconvert ! videoscale ! video/x-raw,format=GRAY8,width=320,height=180 ! appsink name=detector emit-signals=true sync=false max-buffers=1 drop=true ",
                    "t. ! queue leaky=downstream max-size-buffers=10 max-size-time=300000000 ! videoconvert ! videoscale ! video/x-raw,width=1280,height=720 ! videorate ! video/x-raw,framerate=15/1 ! jpegenc quality=90 ! appsink name=mjpeg_sink sync=false max-buffers=1 drop=true ",
                    "t. ! queue leaky=downstream max-size-buffers=5 max-size-time=200000000 ! videoconvert ! videoscale ! video/x-raw,width=640,height=360 ! videorate ! video/x-raw,framerate=10/1 ! jpegenc quality=70 ! appsink name=mjpeg_low_sink sync=false max-buffers=1 drop=true ",
                    "tee_audio. ! queue leaky=downstream max-size-buffers=5 max-size-time=200000000 ! audioconvert ! audioresample ! voaacenc bitrate=32000 ! mp4mux name=mux ! filesink location={}/recording_{}.mp4 ",
                    "tee_audio. ! queue leaky=downstream ! opusenc bitrate=16000 ! webmmux streamable=true ! appsink name=audio_webm_sink sync=false max-buffers=50 drop=true"
                ),
                camera_url, daily_s, day_dir.display(), day_dir_name
            );            println!("📷 Recording pipeline: {}", pipeline_str);
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

        // Manejo dinámico de pads de decodebin3 para video/audio raw
        if let Some(src) = pipeline.by_name("src") {
            let pipeline_weak = pipeline.downgrade();
            src.connect_pad_added(move |_src, pad| {
                let Some(pipeline) = pipeline_weak.upgrade() else { return; };
                let pad_caps = pad.current_caps().unwrap_or_else(|| pad.query_caps(None));
                let caps_str = pad_caps
                    .structure(0)
                    .map(|s| s.to_string())
                    .unwrap_or_default();

                let name = pad.name();
                println!("Pad added: {} caps: {}", name, caps_str);

                if caps_str.contains("video/x-raw") {
                    println!("🎥 Linking raw video pad to tee");
                    if let Some(t) = pipeline.by_name("t") {
                        if let Some(sinkpad) = t.request_pad_simple("sink_%u") {
                            let _ = pad.link(&sinkpad);
                            println!("✅ Raw video linked to tee");
                        }
                    }
                } else if caps_str.contains("audio/x-raw") {
                    println!("🔊 Linking raw audio pad to tee_audio");
                    if let Some(tee_audio) = pipeline.by_name("tee_audio") {
                        if let Some(sinkpad) = tee_audio.request_pad_simple("sink_%u") {
                            let _ = pad.link(&sinkpad);
                            println!("✅ Raw audio linked to tee_audio");
                        }
                    }
                } else {
                    eprintln!("⚠️ Pad no reconocido: {} -> conectando a fakesink para evitar not-linked", caps_str);
                    let q = gst::ElementFactory::make("queue").name(&format!("q_unknown_{}", name)).build().unwrap();
                    let fs = gst::ElementFactory::make("fakesink").name(&format!("fs_unknown_{}", name)).build().unwrap();
                    fs.set_property_from_str("sync", "false");
                    fs.set_property_from_str("async", "false");
                    pipeline.add_many(&[&q, &fs]).ok();
                    gst::Element::link_many(&[&q, &fs]).ok();
                    let _ = pad.link(&q.static_pad("sink").unwrap());
                    for e in [&q, &fs] { e.sync_state_with_parent().ok(); }
                }
            });
        }            // Get the appsink element for event detection
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
                            let mut day_dir = state_for_cb.storage_path.join(&day_dir_name);
                            
                            // Usar el mismo fallback que para las grabaciones
                            if !day_dir.exists() {
                                day_dir = std::path::PathBuf::from("/tmp").join("vigilante").join(&day_dir_name);
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

            // Configurar callback para audio WebM live
            if let Some(audio_sink) = pipeline.by_name("audio_webm_sink") {
                let audio_sink_app = audio_sink.downcast::<gst_app::AppSink>().unwrap();
                let tx_audio = state.audio_webm_tx.clone();
                audio_sink_app.set_callbacks(
                    gst_app::AppSinkCallbacks::builder()
                        .new_sample(move |s| {
                            let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                            let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                            let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                            let data = Bytes::copy_from_slice(map.as_ref());
                            let _ = tx_audio.send(data);
                            Ok(gst::FlowSuccess::Ok)
                        })
                        .build(),
                );
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

