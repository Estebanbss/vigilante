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

        // Pipeline simplificado - sin audio por ahora para evitar complejidades
        let pipeline_str = format!(
            concat!(
                "rtspsrc location={} latency=2000 protocols=tcp ! ",
                "rtph264depay ! h264parse ! tee name=t_video ",
                
                // Branch 1: Recording to MP4 (sin recodificar H.264)
                "t_video. ! queue leaky=2 max-size-buffers=100 max-size-time=5000000000 ! ",
                "h264parse config-interval=-1 ! ",
                "mp4mux ! filesink location=\"{}\" sync=false ",
                
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
                "jpegenc quality=70 ! appsink name=mjpeg_low_sink sync=false max-buffers=1 drop=true"
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