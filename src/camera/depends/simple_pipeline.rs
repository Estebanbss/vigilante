use crate::AppState;
use bytes::Bytes;
use chrono::Local;
use gstreamer::{self as gst, prelude::*, MessageView, Pipeline};
use gstreamer_app as gst_app;
use std::sync::Arc;

pub async fn start_simple_pipeline(
    state: Arc<AppState>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let camera_url = state.camera.rtsp_url.clone();

    loop {
        let now = chrono::Local::now();

        // Crear carpeta por día: DD-MM-YY
        let day_dir_name = now.format("%d-%m-%y").to_string();
        let mut day_dir = state.storage.path.join(&day_dir_name);
        if let Err(e) = std::fs::create_dir_all(&day_dir) {
            eprintln!(
                "❌ No se pudo crear la carpeta {}: {}. Usando /tmp como fallback",
                day_dir.display(),
                e
            );
            day_dir = std::path::PathBuf::from("/tmp")
                .join("vigilante")
                .join(&day_dir_name);
            if let Err(e) = std::fs::create_dir_all(&day_dir) {
                eprintln!(
                    "❌ Tampoco se pudo crear el directorio fallback {}: {}",
                    day_dir.display(),
                    e
                );
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                continue;
            }
        }

        // Siguiente número incremental del día
        let date_str = now.format("%Y-%m-%d").to_string();
        let mut next_num = 1;
        if let Ok(entries) = std::fs::read_dir(&day_dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.starts_with(&date_str) && name.ends_with(".mp4") {
                        if let Some(num_str) = name
                            .strip_prefix(&format!("{}-", date_str))
                            .and_then(|s| s.strip_suffix(".mp4"))
                        {
                            if let Ok(num) = num_str.parse::<i32>() {
                                next_num = next_num.max(num + 1);
                            }
                        }
                    }
                }
            }
        }
        let daily_filename = format!("{}-{}.mp4", date_str, next_num);
        let daily_path = day_dir.join(&daily_filename);

        // Tiempo hasta medianoche
        let next_midnight = (now + chrono::Duration::days(1))
            .date_naive()
            .and_time(chrono::NaiveTime::MIN);
        let duration_until_midnight = next_midnight
            .signed_duration_since(now.naive_local())
            .to_std()
            .unwrap_or(std::time::Duration::from_secs(86400));

        println!(
            "📹 Iniciando grabación diaria: {} (hasta medianoche: {:.0}s)",
            daily_filename,
            duration_until_midnight.as_secs_f64()
        );
        println!("📁 Carpeta de grabación: {}", day_dir.display());
        println!("🎥 Archivo de video: {}", daily_path.display());
        println!("📡 URL RTSP: {}", camera_url);

        let daily_s = daily_path.to_string_lossy();

        // Pipeline simplificado que funcionaba antes
        let pipeline_str = format!(concat!(
            // Fuente RTSP
            "rtspsrc location={} latency=2000 protocols=tcp name=src ",

            // Video: selecciona RTP de video H264 por caps
            "src. ! application/x-rtp,media=video,encoding-name=H264 ! rtph264depay ! h264parse config-interval=-1 ! tee name=t_video ",

            // Grabación MP4 - video con caps explícitos antes del muxer
            "t_video. ! queue name=recq max-size-buffers=50 max-size-time=1000000000 max-size-bytes=10000000 ! ",
            "video/x-h264,stream-format=avc,alignment=au ! mp4mux name=mux faststart=false streamable=true fragment-duration=1000 ! ",
            "filesink location=\"{}\" sync=false ",

            // Audio: selecciona RTP de audio PCMA (ALaw)
            "src. ! application/x-rtp,media=audio,encoding-name=PCMA ! rtppcmadepay ! alawdec ! audioconvert ! audioresample ! tee name=t_audio ",

            // Rama de audio hacia streaming MP3 (live)
            "t_audio. ! queue max-size-buffers=20 max-size-time=3000000000 ! lamemp3enc ! appsink name=audio_mp3_sink sync=false max-buffers=20 drop=true ",

            // Detección de movimiento (GRAY8 downscaled)
            "t_video. ! queue max-size-buffers=10 max-size-time=1000000000 ! avdec_h264 ! videoconvert ! videoscale ! ",
            "video/x-raw,format=GRAY8,width=320,height=180 ! appsink name=detector emit-signals=true sync=false max-buffers=1 drop=true ",

            // MJPEG high quality
            "t_video. ! queue max-size-buffers=15 max-size-time=2000000000 ! avdec_h264 ! videoconvert ! videoscale ! ",
            "video/x-raw,width=1280,height=720 ! videorate ! video/x-raw,framerate=15/1 ! jpegenc quality=85 ! ",
            "appsink name=mjpeg_sink sync=false max-buffers=1 drop=true "
        ), camera_url, daily_s);

        println!("📷 Pipeline: {}", pipeline_str);
        println!("🔄 Creando pipeline GStreamer...");
        let pipeline = match gst::parse::launch(&pipeline_str) {
            Ok(element) => {
                println!("✅ Pipeline creado exitosamente");
                element.downcast::<Pipeline>().unwrap()
            }
            Err(err) => {
                eprintln!("❌ Error al crear el pipeline: {}", err);
                eprintln!("⏳ Reintentando en 10 segundos...");
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                continue;
            }
        };

        // Setup MJPEG sink
        if let Some(mjpeg_sink) = pipeline.by_name("mjpeg_sink") {
            let mjpeg_sink = mjpeg_sink.downcast::<gst_app::AppSink>().unwrap();
            let tx_mjpeg = state.streaming.mjpeg_tx.clone();
            mjpeg_sink.set_callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |s| {
                        let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                        let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                        let data = Bytes::copy_from_slice(map.as_ref());
                        log::debug!("Emitiendo MJPEG frame, {} bytes", data.len());
                        let _ = tx_mjpeg.send(data);
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );
            log::info!("📺 MJPEG sink configured and streaming");
        }

        // Setup audio sink
        if let Some(audio_sink) = pipeline.by_name("audio_mp3_sink") {
            let audio_sink = audio_sink.downcast::<gst_app::AppSink>().unwrap();
            let tx_audio = state.streaming.audio_mp3_tx.clone();
            audio_sink.set_callbacks(
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
            log::info!("🎵 Audio MP3 sink configured");
        }

        // Setup motion detection
        if let Some(detector_sink) = pipeline.by_name("detector") {
            let detector_sink = detector_sink.downcast::<gst_app::AppSink>().unwrap();
            detector_sink.set_callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |sink| {
                        let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                        let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;

                        let width = 320usize;
                        let height = 180usize;
                        let data = map.as_slice();
                        if data.len() < width * height {
                            return Ok(gst::FlowSuccess::Ok);
                        }

                        // Simple motion detection - just log for now
                        let ts = Local::now().format("%H:%M:%S").to_string();
                        log::info!("🚶 Motion detection frame processed at {}", ts);

                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );
            log::info!("🚶 Motion detector configured");
        }

        // Set audio as available
        *state.streaming.audio_available.lock().unwrap() = true;
        *state.streaming.last_audio_timestamp.lock().unwrap() = Some(std::time::Instant::now());

        // Bus y arranque
        let bus = pipeline.bus().unwrap();
        println!("▶️ Iniciando pipeline...");
        match pipeline.set_state(gst::State::Playing) {
            Ok(_) => println!("✅ Pipeline iniciado correctamente"),
            Err(e) => {
                eprintln!("❌ Error al iniciar pipeline: {}", e);
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        }

        // Loop principal: hasta medianoche o error del bus
        let start_time = std::time::Instant::now();
        let mut should_restart = false;
        while start_time.elapsed() < duration_until_midnight {
            let mut iter = bus.iter_timed(gst::ClockTime::from_seconds(1));
            match iter.next() {
                Some(msg) => match msg.view() {
                    MessageView::Eos(_) => {
                        println!("🔁 Fin del stream (EOS), reiniciando...");
                        should_restart = true;
                        break;
                    }
                    MessageView::Error(err) => {
                        eprintln!("❌ Error del pipeline: {}", err.error());
                        if let Some(debug) = err.debug() {
                            eprintln!("🧩 Debug: {}", debug);
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
                    _ => {}
                },
                None => {}
            }
        }

        // Detener y reiniciar si corresponde
        if let Err(e) = pipeline.set_state(gst::State::Null) {
            eprintln!("❌ Error al detener pipeline: {}", e);
        }
        if should_restart {
            println!("⏳ Reiniciando por error en 5 segundos...");
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        } else {
            println!("🕛 Nuevo día - creando nuevo archivo...");
        }
    }
}
