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
        // Permitir desactivar el audio AAC en la grabación MP4 si prefieres solo video
        let record_with_audio_aac = std::env::var("RECORD_WITH_AUDIO_AAC")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let default_pipeline = format!(concat!(
            // Fuente RTSP
            "rtspsrc location={} latency=2000 protocols=tcp name=src ",

            // Video: selecciona RTP de video H264 por caps
            "src. ! application/x-rtp,media=video,encoding-name=H264 ! rtph264depay ! h264parse config-interval=-1 ! tee name=t_video ",

            // Grabación MP4 - video con caps explícitos antes del muxer
            "t_video. ! queue name=recq max-size-buffers=50 max-size-time=1000000000 max-size-bytes=10000000 ! ", // Reducido buffers y tiempo para menor latencia
            "video/x-h264,stream-format=avc,alignment=au ! mp4mux name=mux faststart=false streamable=true fragment-duration=1000 ! ", // Evita buffering de faststart durante grabación continua
            "filesink location=\"{}\" sync=false ",

            // Audio: selecciona RTP de audio PCMA (ALaw); si tu cámara usa PCMU, cambia encoding-name=PCMU y depay/decoder
            "src. ! application/x-rtp,media=audio,encoding-name=PCMA ! rtppcmadepay ! alawdec ! audioconvert ! audioresample ! tee name=t_audio ",

            // Rama de audio hacia grabación MP4 (AAC)
            "t_audio. ! queue max-size-buffers=20 max-size-time=1000000000 ! voaacenc ! aacparse ! queue ! mux.audio_0 ", // Reducido buffers y tiempo

            // Rama de audio hacia streaming MP3 (live)
            "t_audio. ! queue max-size-buffers=20 max-size-time=3000000000 ! lamemp3enc ! appsink name=audio_mp3_sink sync=false max-buffers=20 drop=true ",

            // Detección de movimiento (GRAY8 downscaled)
            "t_video. ! queue max-size-buffers=10 max-size-time=1000000000 ! avdec_h264 ! videoconvert ! videoscale ! ",
            "video/x-raw,format=GRAY8,width=320,height=180 ! appsink name=detector emit-signals=true sync=false max-buffers=1 drop=true ",

            // MJPEG high quality
            "t_video. ! queue max-size-buffers=15 max-size-time=2000000000 ! avdec_h264 ! videoconvert ! videoscale ! ",
            "video/x-raw,width=1280,height=720 ! videorate ! video/x-raw,framerate=15/1 ! jpegenc quality=85 ! ",
            "appsink name=mjpeg_sink sync=false max-buffers=1 drop=true ",

            // MJPEG low quality
            "t_video. ! queue max-size-buffers=10 max-size-time=1000000000 ! avdec_h264 ! videoconvert ! videoscale ! ",
            "video/x-raw,width=640,height=360 ! videorate ! video/x-raw,framerate=8/1 ! jpegenc quality=70 ! ",
            "appsink name=mjpeg_low_sink sync=false max-buffers=1 drop=true "
        ), camera_url, daily_s);

        // Si se desea grabar audio AAC dentro del MP4, adjuntar rama al muxer dinámicamente via plantilla ENV.
        // Nota: En el pipeline por defecto ya está el muxer creado; aquí sólo avisamos cómo activarlo con plantilla.
        if record_with_audio_aac {
            println!("ℹ️ RECORD_WITH_AUDIO_AAC=true: Para incluir audio en MP4 usa plantilla GST_PIPELINE_TEMPLATE con rama 't_audio. ! voaacenc ! aacparse ! queue ! mux.audio_0'.");
        }

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

        // Añadir sondas (probes) a la rama de grabación para diagnosticar CAPS/BUFFERS
        if let Some(recq) = pipeline.by_name("recq") {
            if let Some(srcpad) = recq.static_pad("src") {
                use std::sync::{Arc, atomic::{AtomicU64, Ordering}};
                use gst::{PadProbeType, PadProbeReturn, EventView, PadProbeData};
                let buf_count = Arc::new(AtomicU64::new(0));
                let caps_seen = Arc::new(AtomicU64::new(0));
                let buf_count_cl = buf_count.clone();
                let caps_seen_cl = caps_seen.clone();
                srcpad.add_probe(PadProbeType::EVENT_DOWNSTREAM, move |_pad, info| {
                    if let Some(PadProbeData::Event(ref ev)) = info.data {
                        if let EventView::Caps(c) = ev.view() {
                            if caps_seen_cl.fetch_add(1, Ordering::Relaxed) == 0 {
                                let caps = c.caps();
                                println!("🎛️ CAPS en recq/src -> mux: {}", caps.to_string());
                            }
                        }
                    }
                    PadProbeReturn::Ok
                });
                srcpad.add_probe(PadProbeType::BUFFER, move |_pad, _info| {
                    let n = buf_count_cl.fetch_add(1, Ordering::Relaxed) + 1;
                    if n == 1 { println!("🎥 Buffers hacia mp4mux: {}", n); }
                    PadProbeReturn::Ok
                });
            } else {
                eprintln!("⚠️ No se encontró pad src en recq para instrumentación");
            }
        } else {
            eprintln!("⚠️ No se encontró 'recq' para instrumentación de rama MP4");
        }

        // Probe en la salida del mp4mux para verificar salida hacia filesink
        if let Some(mux) = pipeline.by_name("mux") {
            if let Some(srcpad) = mux.static_pad("src") {
                use std::sync::{Arc, atomic::{AtomicU64, Ordering}};
                use gst::{PadProbeType, PadProbeReturn};
                let out_count = Arc::new(AtomicU64::new(0));
                let out_count_cl = out_count.clone();
                srcpad.add_probe(PadProbeType::BUFFER, move |_pad, _info| {
                    let n = out_count_cl.fetch_add(1, Ordering::Relaxed) + 1;
                    if n == 1 { println!("💽 Buffers después de mp4mux hacia filesink: {}", n); }
                    PadProbeReturn::Ok
                });
            }
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

        // Verificar que el archivo esté creciendo de forma periódica
        tokio::spawn({
            let daily_path = daily_path.clone();
            let duration = duration_until_midnight;
            async move {
                use tokio::time::{sleep, Duration, Instant};
                let start = Instant::now();
                let mut last = 0u64;
                let mut stagnant_checks = 0u32;
                // primera espera breve para permitir escritura inicial
                sleep(Duration::from_secs(15)).await;
                while start.elapsed() < duration {
                    match std::fs::metadata(&daily_path) {
                        Ok(meta) => {
                            let size = meta.len();
                            if size > last {
                                let delta = size - last;
                                println!("📈 Archivo creciendo: {} (+{} bytes)", size, delta);
                                last = size;
                                stagnant_checks = 0;
                            } else {
                                stagnant_checks += 1;
                                if stagnant_checks == 3 {
                                    eprintln!("⚠️ Archivo no crece ({} bytes) tras 3 chequeos. Verificar rama hacia mp4mux/filesink.", size);
                                }
                            }
                        }
                        Err(e) => eprintln!("⚠️ No se puede acceder al archivo de video: {}", e),
                    }
                    sleep(Duration::from_secs(30)).await;
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
    use std::{env, sync::{Arc, Mutex}, time::{Duration, Instant}};

    let detector_sink = pipeline
        .by_name("detector")
        .unwrap()
        .downcast::<gst_app::AppSink>()
        .unwrap();

    // Config desde ENV
    let diff_min: u8 = env::var("MOTION_PIXEL_DIFF_MIN").ok().and_then(|v| v.parse().ok()).unwrap_or(20);
    let percent_threshold: f32 = env::var("MOTION_PIXEL_PERCENT").ok().and_then(|v| v.parse().ok()).unwrap_or(5.0);
    let debounce_ms: u64 = env::var("MOTION_DEBOUNCE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(1500);
    let ignore_rect_env = env::var("MOTION_IGNORE_RECT").ok(); // "x,y,w,h" en coords del frame 320x180

    #[derive(Default)]
    struct BgModel {
        bg: Vec<f32>, // fondo como float para EMA
        last_emit: Option<Instant>,
        ignore: Option<(usize, usize, usize, usize)>,
    }
    let model = Arc::new(Mutex::new(BgModel::default()));

    // Parse ignore rect
    if let Some(s) = ignore_rect_env {
        if let Some(mut st) = model.lock().ok() {
            let parts: Vec<_> = s.split(',').collect();
            if parts.len() == 4 {
                let x = parts[0].trim().parse().unwrap_or(0);
                let y = parts[1].trim().parse().unwrap_or(0);
                let w = parts[2].trim().parse().unwrap_or(0);
                let h = parts[3].trim().parse().unwrap_or(0);
                st.ignore = Some((x, y, w, h));
            }
        }
    }

    detector_sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;

                let width = 320usize;
                let height = 180usize;
                let data = map.as_slice();
                if data.len() < width * height { return Ok(gst::FlowSuccess::Ok); }

                let mut st = model.lock().unwrap();
                // Inicializar fondo si hace falta
                if st.bg.len() != width * height {
                    st.bg = data[..width*height].iter().map(|&v| v as f32).collect();
                    st.last_emit = None;
                    return Ok(gst::FlowSuccess::Ok);
                }

                // EMA para el fondo
                let alpha = 0.02f32; // peso de actualización del fondo
                let step = 3usize; // muestreo
                let mut changed = 0usize;
                let mut total = 0usize;
                let ignore = st.ignore;

                for y in (0..height).step_by(step) {
                    for x in (0..width).step_by(step) {
                        if let Some((ix, iy, iw, ih)) = ignore {
                            if x >= ix && x < ix+iw && y >= iy && y < iy+ih { continue; }
                        }
                        let idx = y*width + x;
                        let prev = st.bg[idx];
                        let cur = data[idx] as f32;
                        let diff = (cur - prev).abs() as f32;
                        if diff >= diff_min as f32 { changed += 1; }
                        // actualizar fondo
                        st.bg[idx] = prev * (1.0 - alpha) + cur * alpha;
                        total += 1;
                    }
                }
                let percent = (changed as f32) * 100.0 / (total.max(1) as f32);

                if percent >= percent_threshold {
                    let now = Instant::now();
                    let allow = match st.last_emit { Some(t) => now.duration_since(t) >= Duration::from_millis(debounce_ms), None => true };
                    if allow {
                        st.last_emit = Some(now);
                        let ts = Local::now().format("%H:%M:%S").to_string();
                        println!("🚶 Movimiento detectado ({}% cambios) a las {}", percent as u32, ts);
                    }
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
