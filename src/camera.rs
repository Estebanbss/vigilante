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
                        " t. ! queue ! h264parse config-interval=1 ! video/x-h264,stream-format=byte-stream,alignment=au ",
                        "! hlssink2 target-duration=2 max-files=5 playlist-length=5 location=\"{segments}\" playlist-location=\"{playlist}\""
                    ),
                    segments = segments_s,
                    playlist = playlist_s,
                ),
                created,
            )
        } else { (String::new(), false) };

    let pipeline_str = format!(
            concat!(
                "rtspsrc location={camera_url} protocols=tcp do-rtsp-keep-alive=true latency=100 retry=5 timeout=20000000000 ",
                "! rtph264depay ! h264parse config-interval=1 name=h264 ",
                "! tee name=t ",
                // recording branch: fragmentado para navegación en tiempo real
                "t. ! queue ! mp4mux name=mux streamable=true faststart=true fragment-duration=2000 fragment-mode=first-moov-then-finalise ",
                "! filesink location=\"{daily}\" sync=false append=false ",
                // detector branch: decodificar a GRAY8 reducido para análisis rápido
                "t. ! queue leaky=downstream max-size-buffers=1 ! decodebin ! videoconvert ! videoscale ! video/x-raw,format=GRAY8,width=640,height=360 ! appsink name=detector emit-signals=true sync=false max-buffers=1 drop=true ",
                // mjpeg branch: decode->scale->jpeg->appsink
                "t. ! queue leaky=downstream max-size-buffers=1 ! decodebin ! videoconvert ! videoscale ! video/x-raw,width=1280,height=720 ",
                "! jpegenc quality=85 ! appsink name=mjpeg_sink sync=false max-buffers=1 drop=true",
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
