use crate::AppState;
use bytes::Bytes;
use chrono::Local;
use gstreamer::{self as gst, prelude::*, MessageView, Pipeline};
use gstreamer_app as gst_app;
use std::sync::Arc;

pub async fn start_camera_pipeline(camera_url: String, state: Arc<AppState>) {
    loop {
        let now = chrono::Local::now();

        // Crear carpeta por día: DD-MM-YY
        let day_dir_name = now.format("%d-%m-%y").to_string();
        let mut day_dir = state.storage_path.join(&day_dir_name);
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

        // Pipeline por defecto (si no se provee plantilla por ENV/archivo)
        let default_pipeline = format!(concat!(
            "rtspsrc location={} latency=2000 protocols=tcp name=src ",
            "src. ! rtph264depay ! h264parse ! tee name=t_video ",
            "t_video. ! queue max-size-buffers=500 max-size-time=10000000000 max-size-bytes=100000000 ! mp4mux name=mux faststart=true ! filesink location=\"{}\" sync=false ",
            "src. ! rtppcmadepay ! alawdec ! audioconvert ! audioresample ! tee name=t_audio ",
            "t_audio. ! queue max-size-buffers=50 max-size-time=5000000000 ! voaacenc bitrate=64000 ! queue ! mux.audio_0 ",
            "t_audio. ! queue max-size-buffers=20 max-size-time=3000000000 ! lamemp3enc ! appsink name=audio_mp3_sink sync=false max-buffers=20 drop=true ",
            "t_video. ! queue max-size-buffers=10 max-size-time=1000000000 ! avdec_h264 ! videoconvert ! videoscale ! video/x-raw,format=GRAY8,width=320,height=180 ! appsink name=detector emit-signals=true sync=false max-buffers=1 drop=true ",
            "t_video. ! queue max-size-buffers=15 max-size-time=2000000000 ! avdec_h264 ! videoconvert ! videoscale ! video/x-raw,width=1280,height=720 ! videorate ! video/x-raw,framerate=15/1 ! jpegenc quality=85 ! appsink name=mjpeg_sink sync=false max-buffers=1 drop=true ",
            "t_video. ! queue max-size-buffers=10 max-size-time=1000000000 ! avdec_h264 ! videoconvert ! videoscale ! video/x-raw,width=640,height=360 ! videorate ! video/x-raw,framerate=8/1 ! jpegenc quality=70 ! appsink name=mjpeg_low_sink sync=false max-buffers=1 drop=true "
        ), camera_url, daily_s);

        // Plantilla configurable: ENV `GST_PIPELINE_TEMPLATE` o archivo `GST_PIPELINE_TEMPLATE_FILE`
        let pipeline_str = match std::env::var("GST_PIPELINE_TEMPLATE_FILE")
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .or_else(|| std::env::var("GST_PIPELINE_TEMPLATE").ok())
        {
            Some(tpl) => {
                // Reemplazar placeholders básicos
                tpl.replace("{CAMERA_URL}", &camera_url)
                    .replace("{FILE_PATH}", &daily_s)
            }
            None => default_pipeline,
        };

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

        // Callbacks (tolerantes a ausencia de elementos por plantilla)
        if pipeline.by_name("detector").is_some() {
            setup_motion_detection(&pipeline, &state);
        }
        if pipeline.by_name("mjpeg_sink").is_some() || pipeline.by_name("mjpeg_low_sink").is_some() {
            setup_mjpeg_sinks(&pipeline, &state);
        }
        if pipeline.by_name("audio_mp3_sink").is_some() {
            setup_audio_mp3_sink(&pipeline, &state);
        }

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

        // Guardar pipeline para otros handlers
        *state.pipeline.lock().await = Some(pipeline.clone());
        println!("🎬 Grabación activa - esperando datos...");

        // Verificar que el archivo esté creciendo
        tokio::spawn({
            let daily_path = daily_path.clone();
            async move {
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                if let Ok(metadata) = std::fs::metadata(&daily_path) {
                    let size = metadata.len();
                    if size > 1_000 {
                        println!("📈 Archivo de video está creciendo: {} bytes", size);
                    } else {
                        eprintln!("⚠️ Archivo de video muy pequeño: {} bytes - posible problema de grabación", size);
                    }
                } else {
                    eprintln!("⚠️ No se puede acceder al archivo de video - problema de creación");
                }
            }
        });

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
                    MessageView::StateChanged(sc) => {
                        if let Some(src) = sc.src() {
                            let name = src.name();
                            if name.contains("rtspsrc") {
                                println!("📶 RTSP state: {:?} -> {:?}", sc.old(), sc.current());
                            }
                        }
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

fn setup_motion_detection(pipeline: &Pipeline, _state: &Arc<AppState>) {
    let detector_sink = pipeline
        .by_name("detector")
        .unwrap()
        .downcast::<gst_app::AppSink>()
        .unwrap();

    detector_sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;

                // Procesar frame para detección de movimiento
                let width = 320;
                let height = 180;
                let _stride = width;
                let data = map.as_slice();

                // Calcular promedio de brillo para detección simple de movimiento
                let mut total_brightness: u64 = 0;
                for &pixel in data {
                    total_brightness += pixel as u64;
                }
                let avg_brightness = total_brightness / (width * height) as u64;

                // Heurística simple: umbral de brillo
                if avg_brightness < 100 {
                    let now = Local::now().format("%H:%M:%S").to_string();
                    println!("🚶 Movimiento detectado a las {}", now);
                }

                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
}

fn setup_mjpeg_sinks(pipeline: &Pipeline, state: &Arc<AppState>) {
    // MJPEG high quality
    let mjpeg_sink = pipeline
        .by_name("mjpeg_sink")
        .unwrap()
        .downcast::<gst_app::AppSink>()
        .unwrap();
    let tx = state.mjpeg_tx.clone();
    mjpeg_sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                let data = Bytes::copy_from_slice(map.as_ref());
                let _ = tx.send(data);
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );

    // MJPEG low quality
    let mjpeg_low_sink = pipeline
        .by_name("mjpeg_low_sink")
        .unwrap()
        .downcast::<gst_app::AppSink>()
        .unwrap();
    let tx_low = state.mjpeg_low_tx.clone();
    mjpeg_low_sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                let data = Bytes::copy_from_slice(map.as_ref());
                let _ = tx_low.send(data);
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
}
fn setup_audio_mp3_sink(pipeline: &Pipeline, state: &Arc<AppState>) {
    let audio_sink_app = pipeline
        .by_name("audio_mp3_sink")
        .unwrap()
        .downcast::<gst_app::AppSink>()
        .unwrap();
    let tx_audio = state.audio_mp3_tx.clone();
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
