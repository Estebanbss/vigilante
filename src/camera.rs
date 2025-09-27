use crate::AppState;
use bytes::Bytes;
use chrono::Local;
use gstreamer::{self as gst, prelude::*, MessageView, Pipeline};
use gstreamer_app as gst_app;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::time::{timeout, Duration};

fn log_to_file(writer: &Arc<StdMutex<Option<BufWriter<std::fs::File>>>>, emoji: &str, msg: String) {
    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let line = format!("[{}] {} {}", timestamp, emoji, msg);
    println!("{}", line);
    if let Ok(mut guard) = writer.lock() {
        if let Some(ref mut file_writer) = *guard {
            let _ = writeln!(file_writer, "{}", line);
            let _ = file_writer.flush();
        }
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut idx = 0usize;
    while size >= 1024.0 && idx < UNITS.len() - 1 {
        size /= 1024.0;
        idx += 1;
    }

    if idx == 0 {
        format!("{} {}", bytes, UNITS[idx])
    } else {
        format!("{:.2} {}", size, UNITS[idx])
    }
}

/// Ejecuta un comando ffmpeg con timeout y reintentos
async fn ejecutar_ffmpeg_con_timeout(
    args: &[&str],
    descripcion: &str,
    timeout_secs: u64,
    max_reintentos: u32,
) -> Result<(), String> {
    let mut ultimo_error = String::new();

    for intento in 1..=max_reintentos {
        println!("🎬 Ejecutando ffmpeg (intento {}/{}): {}", intento, max_reintentos, descripcion);

        let inicio = std::time::Instant::now();

        let resultado_timeout = timeout(
            Duration::from_secs(timeout_secs),
            tokio::process::Command::new("ffmpeg")
                .args(args)
                .status()
        ).await;

        let duracion = inicio.elapsed();

        match resultado_timeout {
            Ok(resultado_status) => {
                match resultado_status {
                    Ok(status) => {
                        if status.success() {
                            println!("✅ ffmpeg completado exitosamente en {:.2?}: {}", duracion, descripcion);

                            // Log estructurado para métricas
                            log::info!(
                                target: "metrics",
                                "ffmpeg_ejecutado_exitosamente comando=\"{}\" duracion_segundos={:.2} intentos={}",
                                descripcion, duracion.as_secs_f64(), intento
                            );

                            return Ok(());
                        } else {
                            ultimo_error = format!("ffmpeg falló con código de salida: {:?} (intento {}/{})",
                                         status.code(), intento, max_reintentos);
                            eprintln!("❌ {}", ultimo_error);
                        }
                    }
                    Err(e) => {
                        ultimo_error = format!("Error ejecutando ffmpeg: {} (intento {}/{})",
                                     e, intento, max_reintentos);
                        eprintln!("❌ {}", ultimo_error);
                    }
                }
            }
            Err(_) => {
                ultimo_error = format!("⏰ Timeout de {}s alcanzado ejecutando ffmpeg (intento {}/{}): {}",
                             timeout_secs, intento, max_reintentos, descripcion);
                eprintln!("❌ {}", ultimo_error);

                // Record timeout metric
                crate::metrics::FFMPEG_TIMEOUT.inc();

                // Log estructurado para métricas de timeout
                log::warn!(
                    target: "metrics",
                    "ffmpeg_timeout comando=\"{}\" duracion_timeout={:.0} intentos={}",
                    descripcion, timeout_secs as f64, intento
                );
            }
        }

        // Si no es el último intento, esperar antes del siguiente
        if intento < max_reintentos {
            crate::metrics::FFMPEG_REINTENTO.inc();
            let espera = Duration::from_secs(2u64.pow(intento - 1)); // Backoff exponencial: 1s, 2s, 4s...
            println!("⏳ Esperando {:.1?} antes del siguiente intento...", espera);
            tokio::time::sleep(espera).await;
        }
    }

    // Log estructurado para fallo final
    log::error!(
        target: "metrics",
        "ffmpeg_fallo_final comando=\"{}\" intentos_maximos={}",
        descripcion, max_reintentos
    );

    Err(format!("ffmpeg falló después de {} intentos: {}", max_reintentos, ultimo_error))
}

async fn concat_day_recordings(day_dir: &Path, date_str: &str) -> Result<(), String> {
    if !day_dir.exists() {
        return Ok(());
    }

    let started = std::time::Instant::now();
    let mut segments: Vec<PathBuf> = Vec::new();
    let mut existing_full: Option<PathBuf> = None;

    let entries = std::fs::read_dir(day_dir)
        .map_err(|e| format!("No se pudo leer {}: {}", day_dir.display(), e))?;

    for entry in entries {
        let entry =
            entry.map_err(|e| format!("Error leyendo archivos en {}: {}", day_dir.display(), e))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(name) => name,
            None => continue,
        };

        if !name.starts_with(&format!("{}-", date_str)) || !name.ends_with(".mp4") {
            continue;
        }

        if name.contains("-full") {
            existing_full = Some(path.clone());
            continue;
        }

        segments.push(path);
    }

    segments.sort();

    if segments.is_empty() {
        return Ok(());
    }

    let mut sources: Vec<PathBuf> = Vec::new();
    let mut previous_full_temp: Option<PathBuf> = None;

    if let Some(full_path) = existing_full {
        let temp_full = day_dir.join(format!("{}-full-prev.mp4", date_str));
        if let Err(err) = std::fs::rename(&full_path, &temp_full) {
            eprintln!(
                "⚠️ No se pudo preparar archivo final previo {}: {}",
                full_path.display(),
                err
            );
        } else {
            sources.push(temp_full.clone());
            previous_full_temp = Some(temp_full);
        }
    }

    sources.extend(segments.iter().cloned());

    let final_path = day_dir.join(format!("{}-full.mp4", date_str));

    if sources.len() <= 1 {
        if let Some(temp) = previous_full_temp {
            let _ = std::fs::remove_file(&final_path);
            if let Err(err) = std::fs::rename(&temp, &final_path) {
                return Err(format!(
                    "No se pudo consolidar archivo diario {} desde {}: {}",
                    final_path.display(),
                    temp.display(),
                    err
                ));
            }
        } else if let Some(source) = sources.first() {
            if source != &final_path {
                if let Err(err) = std::fs::rename(source, &final_path) {
                    return Err(format!(
                        "No se pudo renombrar segmento {} a {}: {}",
                        source.display(),
                        final_path.display(),
                        err
                    ));
                }
            }
        }

        let elapsed = started.elapsed();
        let elapsed_secs = elapsed.as_secs_f64();

        // Record metrics for simple consolidation
        crate::metrics::DURACION_CONCAT_FFMPEG.observe(elapsed_secs);
        crate::metrics::CONCAT_FFMPEG_EXITOSO.inc();

        println!(
            "✅ Archivo diario consolidado listo: {} (tardó {:.2?})",
            final_path.display(),
            elapsed
        );

        // Structured logging for metrics
        log::info!(
            target: "metrics",
            "concat_ffmpeg_completado operacion=consolidacion_simple duracion_segundos={} archivo=\"{}\"",
            elapsed_secs, final_path.display()
        );

        return Ok(());
    }

    let list_path = day_dir.join(format!("{}-concat.txt", date_str));
    {
        let mut list_file = std::fs::File::create(&list_path).map_err(|e| {
            format!(
                "No se pudo crear lista de concatenación {}: {}",
                list_path.display(),
                e
            )
        })?;
        for source in &sources {
            let source_str = source
                .to_str()
                .ok_or_else(|| format!("Ruta inválida para concat: {}", source.display()))?;
            let escaped = source_str.replace('\'', "\\'");
            writeln!(list_file, "file '{}'", escaped)
                .map_err(|e| format!("No se pudo escribir en {}: {}", list_path.display(), e))?;
        }
    }

    if previous_full_temp.is_some() {
        let _ = std::fs::remove_file(&final_path);
        println!(
            "🔁 Actualizando archivo concatenado diario {} con nuevos segmentos",
            final_path.display()
        );
    } else {
        println!(
            "🪄 Preparando concatenación de {} segmentos en {}",
            segments.len(),
            final_path.display()
        );
    }

    // Ejecutar ffmpeg con timeout y reintentos
    let descripcion = format!("concatenación de {} segmentos en {}", segments.len(), final_path.display());
    let list_path_str = list_path.to_string_lossy().to_string();
    let final_path_str = final_path.to_string_lossy().to_string();
    let args = vec![
        "-hide_banner",
        "-loglevel", "error",
        "-f", "concat",
        "-safe", "0",
        "-y",
        "-i", &list_path_str,
        "-c", "copy",
        &final_path_str,
    ];

    // Usar la función helper con timeout de 1 hora y máximo 3 reintentos
    if let Err(err) = ejecutar_ffmpeg_con_timeout(&args, &descripcion, 3600, 3).await {
        // Cleanup on failure
        if let Some(temp) = previous_full_temp.as_ref() {
            let _ = std::fs::rename(temp, final_path.clone());
        }

        let elapsed = started.elapsed();
        let elapsed_secs = elapsed.as_secs_f64();

        // Record metrics for failure (aunque ya se registraron en la función helper)
        crate::metrics::DURACION_CONCAT_FFMPEG.observe(elapsed_secs);
        crate::metrics::CONCAT_FFMPEG_FALLIDO.inc();

        eprintln!(
            "❌ Falló la concatenación ffmpeg: {} (duración total: {:.2?})",
            err,
            elapsed
        );

        // Structured logging for metrics
        log::error!(
            target: "metrics",
            "concat_ffmpeg_fallido duracion_segundos={} archivo=\"{}\" error=\"{}\"",
            elapsed_secs, final_path.display(), err
        );

        return Err(err);
    }

    let _ = std::fs::remove_file(&list_path);

    if let Some(temp) = previous_full_temp {
        if let Err(err) = std::fs::remove_file(&temp) {
            eprintln!(
                "⚠️ No se pudo eliminar archivo previo {}: {}",
                temp.display(),
                err
            );
        }
    }

    for segment in segments {
        if segment == final_path {
            continue;
        }
        match std::fs::remove_file(&segment) {
            Ok(_) => {
                println!(
                    "🧹 Segmento eliminado tras concatenación: {}",
                    segment.display()
                );
            }
            Err(err) => {
                eprintln!(
                    "⚠️ No se pudo eliminar segmento {}: {}",
                    segment.display(),
                    err
                );
            }
        }
    }

    let elapsed = started.elapsed();
    let elapsed_secs = elapsed.as_secs_f64();

    // Record metrics for successful ffmpeg concatenation
    crate::metrics::DURACION_CONCAT_FFMPEG.observe(elapsed_secs);
    crate::metrics::CONCAT_FFMPEG_EXITOSO.inc();

    println!(
        "✅ Archivo diario concatenado listo: {} (tardó {:.2?})",
        final_path.display(),
        elapsed
    );

    // Structured logging for metrics
    log::info!(
        target: "metrics",
        "concat_ffmpeg_completado operacion=concatenacion_completa duracion_segundos={} archivo=\"{}\"",
        elapsed_secs, final_path.display()
    );

    Ok(())
}

pub async fn start_camera_pipeline(camera_url: String, state: Arc<AppState>) {
    let state = Arc::clone(&state); // Clonamos para que tenga lifetime 'static
    loop {
        let cycle_started = std::time::Instant::now();
        
        // Reset audio availability at the start of each cycle
        *state.audio_available.lock().unwrap() = false;
        
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
                println!(
                    "⏱️ Ciclo de pipeline abortado en {:.2?} (falló preparación de carpeta)",
                    cycle_started.elapsed()
                );
                continue;
            }
        }

        // Siguiente número incremental del día
        let date_str = now.format("%Y-%m-%d").to_string();

        if let Err(err) = concat_day_recordings(&day_dir, &date_str).await {
            eprintln!(
                "⚠️ Falló concatenación previa en {}: {}",
                day_dir.display(),
                err
            );
        }

        let mut next_num = 1;

        // Usar find para obtener el número más alto de forma eficiente
        let find_cmd = format!(
            "find '{}' -maxdepth 1 -name '{}-*.mp4' -printf '%P\\n' | awk -F'-' '{{print $(NF)}}' | sed 's/\\.mp4$//' | sort -n | tail -1",
            day_dir.display(), date_str
        );
        if let Ok(output) = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&find_cmd)
            .output()
            .await
        {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if let Ok(max_num) = stdout.trim().parse::<i32>() {
                    next_num = max_num + 1;
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

        // Configurar logging a archivo del día
        let log_file_path = day_dir.join(format!("{}-log.txt", date_str));
        let log_file_result = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file_path);
        let log_writer: Arc<StdMutex<Option<BufWriter<std::fs::File>>>> = match log_file_result {
            Ok(file) => {
                println!("📝 Logging habilitado en: {}", log_file_path.display());
                Arc::new(StdMutex::new(Some(BufWriter::new(file))))
            }
            Err(e) => {
                eprintln!(
                    "⚠️ No se pudo crear archivo de log {}: {}. Logging solo a consola",
                    log_file_path.display(),
                    e
                );
                Arc::new(StdMutex::new(None))
            }
        };

        // Inicializar log_writer en el estado global para que lo usen los eventos ONVIF
        let log_file_result2 = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file_path);
        match log_file_result2 {
            Ok(file) => {
                *state.log_writer.lock().unwrap() = Some(BufWriter::new(file));
            }
            Err(e) => {
                eprintln!("⚠️ No se pudo inicializar log_writer global: {}", e);
                *state.log_writer.lock().unwrap() = None;
            }
        }

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

        // Grabación segmentada opcional (mejora carga inicial y saltos)
        let record_segment_seconds: u64 = std::env::var("RECORD_SEGMENT_SECONDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);

        // Fragmento MP4 configurable (ms) para mejorar start/seek manteniendo un solo archivo
        let mp4_fragment_ms: u64 = std::env::var("MP4_FRAGMENT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300);

        let pipeline_str_default = format!(concat!(
            // Fuente RTSP
            "rtspsrc location={} latency=2000 protocols=tcp name=src ",

            // Video: selecciona RTP de video H264 por caps
            "src. ! application/x-rtp,media=video,encoding-name=H264 ! rtph264depay ! h264parse config-interval=-1 ! tee name=t_video ",

            // Grabación MP4 continua
            "t_video. ! queue name=recq max-size-buffers=50 max-size-time=500000000 max-size-bytes=10000000 ! ",
            "video/x-h264,stream-format=avc,alignment=au ! mp4mux name=mux faststart=true streamable=true fragment-duration={} ! ",
            "filesink location=\"{}\" sync=false ",

            // Audio: selecciona RTP de audio PCMA (ALaw)
            "src. ! application/x-rtp,media=audio,encoding-name=PCMA ! rtppcmadepay ! alawdec ! audioconvert ! audioresample ! tee name=t_audio ",

            // Rama de audio hacia grabación MP4 (AAC)
            "t_audio. ! queue max-size-buffers=20 max-size-time=500000000 ! voaacenc ! aacparse ! queue ! mux.audio_0 ",

            // Rama de audio hacia streaming MP3 (live) - Optimizado para baja latencia
            "t_audio. ! queue max-size-buffers=10 max-size-time=20000000 leaky=downstream ! lamemp3enc quality=4 bitrate=128 ! appsink name=audio_mp3_sink sync=false max-buffers=5 drop=false ",

            // Detección de movimiento (GRAY8 downscaled)
            "t_video. ! queue max-size-buffers=10 max-size-time=500000000 ! avdec_h264 ! videoconvert ! videoscale ! ",
            "video/x-raw,format=GRAY8,width=320,height=180 ! appsink name=detector emit-signals=true sync=false max-buffers=1 drop=true ",

            // MJPEG high quality
            "t_video. ! queue max-size-buffers=15 max-size-time=500000000 ! avdec_h264 ! videoconvert ! videoscale ! ",
            "video/x-raw,width=1280,height=720 ! videorate ! video/x-raw,framerate=15/1 ! jpegenc quality=85 ! ",
            "appsink name=mjpeg_sink sync=false max-buffers=1 drop=true ",

            // MJPEG low quality
            "t_video. ! queue max-size-buffers=10 max-size-time=500000000 ! avdec_h264 ! videoconvert ! videoscale ! ",
            "video/x-raw,width=640,height=360 ! videorate ! video/x-raw,framerate=8/1 ! jpegenc quality=70 ! ",
            "appsink name=mjpeg_low_sink sync=false max-buffers=1 drop=true "
    ), camera_url, mp4_fragment_ms, daily_s);

        // Pipeline con splitmuxsink (segmentos)
        let segments_location = day_dir.join(format!("{}-%05d.mp4", date_str));
        let segments_location_s = segments_location.to_string_lossy();
        let max_size_time_ns = record_segment_seconds.saturating_mul(1_000_000_000);
        let pipeline_str_segmented = format!(concat!(
            // Fuente RTSP
            "rtspsrc location={} latency=2000 protocols=tcp name=src ",

            // Video
            "src. ! application/x-rtp,media=video,encoding-name=H264 ! rtph264depay ! h264parse config-interval=-1 ! tee name=t_video ",

            // Grabación segmentada con splitmuxsink (MP4)
            "t_video. ! queue name=recq max-size-buffers=50 max-size-time=500000000 max-size-bytes=10000000 ! ",
            "video/x-h264,stream-format=avc,alignment=au ! splitmuxsink name=spl muxer=mp4mux max-size-time={} location=\"{}\" ",

            // Audio
            "src. ! application/x-rtp,media=audio,encoding-name=PCMA ! rtppcmadepay ! alawdec ! audioconvert ! audioresample ! tee name=t_audio ",
            "t_audio. ! queue max-size-buffers=20 max-size-time=500000000 ! voaacenc ! aacparse ! queue ! spl.audio_0 ",

            // Rama de audio hacia streaming MP3 (live) - Optimizado para baja latencia
            "t_audio. ! queue max-size-buffers=10 max-size-time=20000000 leaky=downstream ! lamemp3enc quality=4 bitrate=128 ! appsink name=audio_mp3_sink sync=false max-buffers=5 drop=false ",

            // Detección de movimiento
            "t_video. ! queue max-size-buffers=10 max-size-time=500000000 ! avdec_h264 ! videoconvert ! videoscale ! ",
            "video/x-raw,format=GRAY8,width=320,height=180 ! appsink name=detector emit-signals=true sync=false max-buffers=1 drop=true ",

            // MJPEG high
            "t_video. ! queue max-size-buffers=15 max-size-time=500000000 ! avdec_h264 ! videoconvert ! videoscale ! ",
            "video/x-raw,width=1280,height=720 ! videorate ! video/x-raw,framerate=15/1 ! jpegenc quality=85 ! ",
            "appsink name=mjpeg_sink sync=false max-buffers=1 drop=true ",

            // MJPEG low
            "t_video. ! queue max-size-buffers=10 max-size-time=500000000 ! avdec_h264 ! videoconvert ! videoscale ! ",
            "video/x-raw,width=640,height=360 ! videorate ! video/x-raw,framerate=8/1 ! jpegenc quality=70 ! ",
            "appsink name=mjpeg_low_sink sync=false max-buffers=1 drop=true "
        ), camera_url, max_size_time_ns, segments_location_s);

        let default_pipeline = if record_segment_seconds > 0 {
            println!(
                "🧩 Segmentación activada: {} s por archivo",
                record_segment_seconds
            );
            pipeline_str_segmented
        } else {
            pipeline_str_default
        };

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
                println!(
                    "⏱️ Ciclo de pipeline abortado en {:.2?} (falló creación)",
                    cycle_started.elapsed()
                );
                continue;
            }
        };

        // Callbacks (tolerantes a ausencia de elementos por plantilla)
        if pipeline.by_name("detector").is_some() && state.enable_manual_motion_detection {
            println!("🎯 Activando detección manual de movimiento");
            setup_motion_detection(&pipeline, &state, &log_writer);
        } else if pipeline.by_name("detector").is_some() {
            println!(
                "📡 Usando solo detección nativa de cámara ONVIF (detección manual desactivada)"
            );
        }
        if pipeline.by_name("mjpeg_sink").is_some() || pipeline.by_name("mjpeg_low_sink").is_some()
        {
            setup_mjpeg_sinks(&pipeline, &state, &log_writer);
        }
        if pipeline.by_name("audio_mp3_sink").is_some() {
            setup_audio_mp3_sink(&pipeline, &state, &log_writer).await;
        }

        // Añadir sondas (probes) a la rama de grabación para diagnosticar CAPS/BUFFERS
        if let Some(recq) = pipeline.by_name("recq") {
            if let Some(srcpad) = recq.static_pad("src") {
                use gst::{EventView, PadProbeData, PadProbeReturn, PadProbeType};
                use std::sync::{
                    atomic::{AtomicU64, Ordering},
                    Arc,
                };
                let buf_count = Arc::new(AtomicU64::new(0));
                let caps_seen = Arc::new(AtomicU64::new(0));
                let buf_count_cl = buf_count.clone();
                let caps_seen_cl = caps_seen.clone();
                let _log_writer_cl = log_writer.clone();
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
                let _log_writer_cl2 = log_writer.clone();
                srcpad.add_probe(PadProbeType::BUFFER, move |_pad, _info| {
                    let n = buf_count_cl.fetch_add(1, Ordering::Relaxed) + 1;
                    if n == 1 {
                        println!("🎥 Buffers hacia mp4mux: {}", n);
                    }
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
                use gst::{PadProbeReturn, PadProbeType};
                use std::sync::{
                    atomic::{AtomicU64, Ordering},
                    Arc,
                };
                let out_count = Arc::new(AtomicU64::new(0));
                let out_count_cl = out_count.clone();
                let _log_writer_cl = log_writer.clone();
                srcpad.add_probe(PadProbeType::BUFFER, move |_pad, _info| {
                    let n = out_count_cl.fetch_add(1, Ordering::Relaxed) + 1;
                    if n == 1 {
                        println!("💽 Buffers después de mp4mux hacia filesink: {}", n);
                    }
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
                println!(
                    "⏱️ Ciclo de pipeline abortado en {:.2?} (falló arranque)",
                    cycle_started.elapsed()
                );
                continue;
            }
        }

        // Guardar pipeline para otros handlers
        *state.pipeline.lock().await = Some(pipeline.clone());
        println!("🎬 Grabación activa - esperando datos...");

        // Iniciar tarea de verificación de audio disponible
        let audio_check_state = state.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                interval.tick().await;
                
                let last_audio = *audio_check_state.last_audio_timestamp.lock().unwrap();
                let audio_available = *audio_check_state.audio_available.lock().unwrap();
                
                match last_audio {
                    Some(timestamp) => {
                        let elapsed = timestamp.elapsed();
                        // Si han pasado más de 10 segundos sin audio, marcar como no disponible
                        if elapsed > std::time::Duration::from_secs(10) && audio_available {
                            *audio_check_state.audio_available.lock().unwrap() = false;
                            println!("🔇 Audio no disponible - no se reciben datos desde hace {:.1?}", elapsed);
                            
                            // Log estructurado para métricas
                            log::warn!(
                                target: "metrics",
                                "audio_caido tiempo_sin_datos={:.1}",
                                elapsed.as_secs_f64()
                            );
                        }
                    }
                    None => {
                        // Si nunca hemos recibido audio y han pasado más de 60 segundos desde el inicio del ciclo
                        if cycle_started.elapsed() > std::time::Duration::from_secs(60) && audio_available {
                            *audio_check_state.audio_available.lock().unwrap() = false;
                            println!("🔇 Audio no disponible - nunca se recibió audio en los primeros 30 segundos");
                            
                            // Log estructurado para métricas
                            log::warn!(
                                target: "metrics",
                                "audio_nunca_recibido tiempo_espera=60.0"
                            );
                        }
                    }
                }
            }
        });

        // Verificar que la salida esté creciendo de forma periódica (soporta segmentación)
        let log_writer_spawn = log_writer.clone();
        tokio::spawn({
            let daily_path = daily_path.clone();
            let day_dir = day_dir.clone();
            let date_str = date_str.clone();
            let duration = duration_until_midnight;
            let record_segment_seconds = record_segment_seconds;
            let daily_filename = daily_filename.clone();
            let segmented = record_segment_seconds > 0;
            async move {
                use std::fs;
                use tokio::time::{sleep, Duration, Instant};
                let start = Instant::now();
                let mut last_size = 0u64;
                let mut last_file: Option<String> = None;
                let mut stagnant_checks = 0u32;
                // primera espera breve para permitir escritura inicial
                sleep(Duration::from_secs(15)).await;
                while start.elapsed() < duration {
                    let (current_file, cur_size, file_exists) = if segmented {
                        let mut latest: Option<(String, std::time::SystemTime, u64)> = None;

                        if let Ok(entries) = fs::read_dir(&day_dir) {
                            for entry_res in entries {
                                let Ok(entry) = entry_res else {
                                    continue;
                                };
                                let file_name = entry.file_name().to_string_lossy().to_string();
                                if !file_name.starts_with(&format!("{}-", date_str))
                                    || !file_name.ends_with(".mp4")
                                {
                                    continue;
                                }

                                let Ok(metadata) = entry.metadata() else {
                                    continue;
                                };
                                let mtime = metadata
                                    .modified()
                                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                                let size = metadata.len();

                                match &latest {
                                    Some((_, best_time, _)) if mtime <= *best_time => {}
                                    _ => latest = Some((file_name, mtime, size)),
                                }
                            }
                        }

                        if let Some((name, _ts, size)) = latest {
                            (name, size, true)
                        } else {
                            (daily_filename.clone(), 0, false)
                        }
                    } else {
                        match fs::metadata(&daily_path) {
                            Ok(meta) => (daily_filename.clone(), meta.len(), true),
                            Err(_) => (daily_filename.clone(), 0, false),
                        }
                    };

                    let current_path = day_dir.join(&current_file);
                    let label = if segmented { "segmento" } else { "archivo" };

                    let file_changed = last_file
                        .as_ref()
                        .map(|f| f.as_str() != current_file.as_str())
                        .unwrap_or(false);
                    let first_seen = last_file.is_none();
                    if cur_size > last_size || file_changed || first_seen {
                        let delta = if file_changed || first_seen {
                            cur_size
                        } else {
                            cur_size.saturating_sub(last_size)
                        };

                        if first_seen {
                            log_to_file(
                                &log_writer_spawn,
                                "🎞️",
                                format!(
                                    "Primer {} de grabación detectado: {}",
                                    label,
                                    current_path.display()
                                ),
                            );
                        } else if file_changed {
                            log_to_file(
                                &log_writer_spawn,
                                "🎞️",
                                format!("Nuevo {} de grabación: {}", label, current_path.display()),
                            );
                        }

                        log_to_file(
                            &log_writer_spawn,
                            "🎥",
                            format!(
                                "Grabación activa en {}: {} (+{})",
                                current_path.display(),
                                format_bytes(cur_size),
                                format_bytes(delta)
                            ),
                        );
                        last_size = cur_size;
                        last_file = Some(current_file.clone());
                        stagnant_checks = 0;
                    } else {
                        stagnant_checks += 1;
                        if !file_exists {
                            log_to_file(
                                &log_writer_spawn,
                                "⏳",
                                format!(
                                    "Esperando creación del {} de grabación en {}...",
                                    label,
                                    current_path.display()
                                ),
                            );
                        }
                        if stagnant_checks == 3 {
                            log_to_file(
                                &log_writer_spawn,
                                "⚠️",
                                format!(
                                    "Salida de grabación sin crecimiento para {} ({}).",
                                    current_path.display(),
                                    format_bytes(cur_size)
                                ),
                            );
                            stagnant_checks = 0;
                        }
                    }

                    sleep(Duration::from_secs(60)).await;
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
                        let error_msg = format!(
                            "{}: {}",
                            err.error(),
                            err.debug().unwrap_or_else(|| "".into())
                        );
                        eprintln!("❌ Error del pipeline: {}", error_msg);

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
            if let Err(err) = concat_day_recordings(&day_dir, &date_str).await {
                eprintln!(
                    "⚠️ Error al concatenar grabaciones del día {}: {}",
                    date_str, err
                );
            }
            println!("🕛 Nuevo día - creando nuevo archivo...");
        }

        println!(
            "⏱️ Ciclo completo de pipeline y housekeeping en {:.2?}",
            cycle_started.elapsed()
        );
    }
}

fn setup_motion_detection(
    pipeline: &Pipeline,
    _state: &Arc<AppState>,
    writer: &Arc<StdMutex<Option<BufWriter<std::fs::File>>>>,
) {
    use std::{
        env,
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    let detector_sink = pipeline
        .by_name("detector")
        .unwrap()
        .downcast::<gst_app::AppSink>()
        .unwrap();

    // Config desde ENV
    let diff_min: u8 = env::var("MOTION_PIXEL_DIFF_MIN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let percent_threshold: f32 = env::var("MOTION_PIXEL_PERCENT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5.0);
    let debounce_ms: u64 = env::var("MOTION_DEBOUNCE_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1500);
    let block_size: usize = env::var("MOTION_BLOCK_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(16);
    let ignore_rect_env = env::var("MOTION_IGNORE_RECT").ok(); // "x,y,w,h" en coords del frame 320x180
    let ignore_rects_env = env::var("MOTION_IGNORE_RECTS").ok(); // múltiples: "x1,y1,w1,h1;x2,y2,w2,h2"

    #[derive(Default)]
    struct BgModel {
        bg: Vec<f32>,       // fondo como float para EMA
        variance: Vec<f32>, // varianza por píxel para adaptación dinámica
        last_emit: Option<Instant>,
        ignore_rects: Vec<(usize, usize, usize, usize)>, // múltiples zonas ignoradas
        motion_history: Vec<(f32, f32)>, // historial de movimiento (dx, dy) para dirección
        frame_count: u64,                // contador de frames procesados
        last_frame_brightness: f32,      // brillo promedio del frame anterior
    }
    let model = Arc::new(Mutex::new(BgModel::default()));

    // Parse ignore rects (soporte para múltiples)
    let mut ignore_rects = Vec::new();

    // Parse single rect (legacy)
    if let Some(s) = ignore_rect_env {
        let parts: Vec<_> = s.split(',').collect();
        if parts.len() == 4 {
            let x = parts[0].trim().parse().unwrap_or(0);
            let y = parts[1].trim().parse().unwrap_or(0);
            let w = parts[2].trim().parse().unwrap_or(0);
            let h = parts[3].trim().parse().unwrap_or(0);
            ignore_rects.push((x, y, w, h));
        }
    }

    // Parse multiple rects
    if let Some(s) = ignore_rects_env {
        for rect_str in s.split(';') {
            let parts: Vec<_> = rect_str.split(',').collect();
            if parts.len() == 4 {
                let x = parts[0].trim().parse().unwrap_or(0);
                let y = parts[1].trim().parse().unwrap_or(0);
                let w = parts[2].trim().parse().unwrap_or(0);
                let h = parts[3].trim().parse().unwrap_or(0);
                ignore_rects.push((x, y, w, h));
            }
        }
    }

    if let Some(mut st) = model.lock().ok() {
        st.ignore_rects = ignore_rects;
    }

    let writer_cloned = writer.clone();
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
                    st.variance = vec![10.0; width * height]; // varianza inicial
                    st.last_emit = None;
                    st.motion_history.clear();
                    st.frame_count = 0;
                    st.last_frame_brightness = data.iter().map(|&x| x as f32).sum::<f32>() / data.len() as f32;
                    return Ok(gst::FlowSuccess::Ok);
                }

                st.frame_count += 1;

                // Calcular brillo actual y cambio de iluminación
                let current_brightness = data.iter().map(|&x| x as f32).sum::<f32>() / data.len() as f32;
                let brightness_change = (current_brightness - st.last_frame_brightness).abs();
                st.last_frame_brightness = current_brightness;

                // Análisis avanzado de movimiento por bloques
                let mut changed_blocks = 0usize;
                let mut total_blocks = 0usize;
                let mut block_vectors = Vec::new(); // para análisis de coherencia
                let mut total_motion_magnitude = 0.0f32;
                let mut max_block_motion = 0.0f32;
                let ignore_rects = st.ignore_rects.clone();

                // Procesar en bloques para análisis detallado
                for by in (0..height).step_by(block_size) {
                    for bx in (0..width).step_by(block_size) {
                        let block_end_y = (by + block_size).min(height);
                        let block_end_x = (bx + block_size).min(width);
                        let mut block_changed_pixels = 0usize;
                        let mut block_total_pixels = 0usize;
                        let mut block_motion = (0.0f32, 0.0f32);
                        let mut block_brightness_change = 0.0f32;

                        // Verificar si el bloque está en zona ignorada
                        let block_ignored = ignore_rects.iter().any(|(ix, iy, iw, ih)| {
                            bx < ix + iw && bx + block_size > *ix && by < iy + ih && by + block_size > *iy
                        });

                        if block_ignored { continue; }

                        // Analizar cada píxel en el bloque
                        for y in by..block_end_y {
                            for x in bx..block_end_x {
                                let idx = y * width + x;
                                let prev = st.bg[idx];
                                let cur = data[idx] as f32;
                                let diff = (cur - prev).abs();

                                // Adaptación dinámica del fondo
                                let var = st.variance[idx];
                                let adaptive_threshold = (diff_min as f32).max(var * 2.0);

                                if diff >= adaptive_threshold {
                                    block_changed_pixels += 1;

                                    // Estimar movimiento usando gradientes simples
                                    let dx = if x > 0 && x < width - 1 {
                                        (data[idx + 1] as f32 - data[idx - 1] as f32) * 0.5
                                    } else { 0.0 };
                                    let dy = if y > 0 && y < height - 1 {
                                        (data[idx + width] as f32 - data[idx - width] as f32) * 0.5
                                    } else { 0.0 };

                                    block_motion.0 += dx;
                                    block_motion.1 += dy;
                                }

                                // Actualizar fondo con alpha dinámico
                                let alpha = if var > 50.0 { 0.05 } else { 0.02 };
                                st.bg[idx] = prev * (1.0 - alpha) + cur * alpha;

                                // Actualizar varianza
                                let error = cur - prev;
                                st.variance[idx] = var * 0.99 + error * error * 0.01;

                                block_total_pixels += 1;
                                block_brightness_change += (cur - prev).abs();
                            }
                        }

                        // Calcular métricas del bloque
                        let block_change_percent = (block_changed_pixels as f32) * 100.0 / (block_total_pixels.max(1) as f32);
                        let _avg_block_brightness_change = block_brightness_change / block_total_pixels as f32;

                        // Normalizar vector de movimiento del bloque
                        if block_changed_pixels > 0 {
                            block_motion.0 /= block_changed_pixels as f32;
                            block_motion.1 /= block_changed_pixels as f32;
                        }

                        let block_motion_magnitude = (block_motion.0 * block_motion.0 + block_motion.1 * block_motion.1).sqrt();

                        // Decidir si el bloque tiene movimiento significativo
                        if block_change_percent >= 10.0 && block_motion_magnitude > 5.0 {
                            changed_blocks += 1;
                            block_vectors.push((block_motion.0, block_motion.1, block_motion_magnitude));
                            total_motion_magnitude += block_motion_magnitude;
                            max_block_motion = max_block_motion.max(block_motion_magnitude);
                        }

                        total_blocks += 1;
                    }
                }

                // Análisis global del movimiento
                let motion_percent = (changed_blocks as f32) * 100.0 / (total_blocks.max(1) as f32);
                let avg_motion_magnitude = if changed_blocks > 0 { total_motion_magnitude / changed_blocks as f32 } else { 0.0 };

                // Calcular coherencia del movimiento (qué tan alineados están los vectores)
                let mut coherence_score = 0.0f32;
                if block_vectors.len() > 1 {
                    let avg_vector = block_vectors.iter()
                        .fold((0.0f32, 0.0f32), |acc, (dx, dy, _)| (acc.0 + dx, acc.1 + dy));
                    let avg_mag = (avg_vector.0 * avg_vector.0 + avg_vector.1 * avg_vector.1).sqrt();
                    if avg_mag > 0.0 {
                        let normalized_avg = (avg_vector.0 / avg_mag, avg_vector.1 / avg_mag);
                        coherence_score = block_vectors.iter()
                            .map(|(dx, dy, mag)| {
                                if *mag > 0.0 {
                                    let dot_product = dx * normalized_avg.0 + dy * normalized_avg.1;
                                    dot_product.abs() // coherencia direccional
                                } else { 0.0 }
                            })
                            .sum::<f32>() / block_vectors.len() as f32;
                    }
                }

                // Clasificar tipo de movimiento
                let motion_type = if motion_percent < 1.0 {
                    "Sin movimiento"
                } else if brightness_change > 30.0 && motion_percent < 5.0 {
                    "Cambio de iluminación"
                } else if coherence_score > 0.8 && changed_blocks > (total_blocks * 3 / 4) && avg_motion_magnitude > 15.0 {
                    "Movimiento de cámara"
                } else if coherence_score > 0.6 && changed_blocks > (total_blocks / 2) && avg_motion_magnitude > 10.0 {
                    "Movimiento masivo"
                } else if changed_blocks <= 3 && avg_motion_magnitude > 8.0 {
                    "Movimiento localizado pequeño"
                } else if changed_blocks <= 6 && avg_motion_magnitude > 12.0 {
                    "Movimiento localizado mediano"
                } else {
                    "Movimiento general"
                };

                // Determinar dirección principal
                let direction = if avg_motion_magnitude > 10.0 && block_vectors.len() > 0 {
                    let avg_dx = block_vectors.iter().map(|(dx, _, _)| dx).sum::<f32>() / block_vectors.len() as f32;
                    let avg_dy = block_vectors.iter().map(|(_, dy, _)| dy).sum::<f32>() / block_vectors.len() as f32;

                    if avg_dx.abs() > avg_dy.abs() {
                        if avg_dx > 5.0 { "→ Este" } else if avg_dx < -5.0 { "← Oeste" } else { "○ Estacionario" }
                    } else {
                        if avg_dy > 5.0 { "↓ Sur" } else if avg_dy < -5.0 { "↑ Norte" } else { "○ Estacionario" }
                    }
                } else { "○ Sin dirección clara" };

                // Logging detallado si hay movimiento significativo
                if motion_percent >= percent_threshold {
                    let now = Instant::now();
                    let allow = match st.last_emit { Some(t) => now.duration_since(t) >= Duration::from_millis(debounce_ms), None => true };
                    if allow {
                        st.last_emit = Some(now);
                        let ts = Local::now().format("%H:%M:%S").to_string();

                        // Actualizar historial de movimiento
                        if block_vectors.len() > 0 {
                            let avg_vector = (block_vectors.iter().map(|(dx, _, _)| dx).sum::<f32>() / block_vectors.len() as f32,
                                            block_vectors.iter().map(|(_, dy, _)| dy).sum::<f32>() / block_vectors.len() as f32);
                            st.motion_history.push(avg_vector);
                            if st.motion_history.len() > 5 {
                                st.motion_history.remove(0);
                            }
                        }

                        log_to_file(&writer_cloned, "🚶", format!(
                            "{} ({}% bloques, {:.1} mag, {:.2} coh) {} | Ilum: {:.1} | Bloques: {}/{} | Frame: {} | {}",
                            motion_type, motion_percent as u32, avg_motion_magnitude,
                            coherence_score, direction, brightness_change,
                            changed_blocks, total_blocks, st.frame_count, ts
                        ));
                    }
                }

                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
}

fn setup_mjpeg_sinks(
    pipeline: &Pipeline,
    state: &Arc<AppState>,
    _writer: &Arc<StdMutex<Option<BufWriter<std::fs::File>>>>,
) {
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
async fn setup_audio_mp3_sink(
    pipeline: &Pipeline,
    state: &Arc<AppState>,
    _writer: &Arc<StdMutex<Option<BufWriter<std::fs::File>>>>,
) {
    let audio_sink_app = match pipeline.by_name("audio_mp3_sink") {
        Some(sink) => match sink.downcast::<gst_app::AppSink>() {
            Ok(s) => s,
            Err(_) => {
                eprintln!("❌ Error downcasting audio_mp3_sink to AppSink");
                return;
            }
        },
        None => {
            eprintln!("❌ No se encontró audio_mp3_sink en el pipeline (posiblemente no hay audio en el stream)");
            return;
        }
    };

    // Marcar que el audio está disponible
    *state.audio_available.lock().unwrap() = true;
    println!("🎵 Audio MP3 streaming configurado correctamente");

    let tx_audio = state.audio_mp3_tx.clone();
    let last_audio_ts = state.last_audio_timestamp.clone();
    let audio_available_flag = state.audio_available.clone();
    audio_sink_app.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |s| {
                // Actualizar timestamp del último audio recibido
                let now = std::time::Instant::now();
                *last_audio_ts.lock().unwrap() = Some(now);
                
                // Si el audio no estaba disponible, marcarlo como disponible ahora
                let was_available = *audio_available_flag.lock().unwrap();
                if !was_available {
                    *audio_available_flag.lock().unwrap() = true;
                    println!("🎵 Audio recuperado - datos llegando nuevamente");
                    
                    // Log estructurado para métricas
                    log::info!(
                        target: "metrics",
                        "audio_recuperado tiempo_sin_datos=0.0"
                    );
                }
                let sample = match s.pull_sample() {
                    Ok(samp) => samp,
                    Err(e) => {
                        eprintln!("❌ Error pulling sample from audio_mp3_sink: {:?}", e);
                        return Err(gst::FlowError::Eos);
                    }
                };
                let buffer = match sample.buffer() {
                    Some(buf) => buf,
                    None => {
                        eprintln!("❌ No buffer in audio sample");
                        return Err(gst::FlowError::Error);
                    }
                };

                // Optimización: usar map_readable solo si necesitamos acceder a los datos
                let data = if buffer.size() > 0 {
                    match buffer.map_readable() {
                        Ok(map) => {
                            let bytes = Bytes::copy_from_slice(map.as_ref());
                            // Log de debug cada 100 paquetes para monitoreo
                            use std::sync::atomic::{AtomicU64, Ordering};
                            static COUNTER: AtomicU64 = AtomicU64::new(0);
                            let count = COUNTER.fetch_add(1, Ordering::Relaxed);
                            if count % 1000 == 0 {
                                println!(
                                    "🎵 Audio MP3: {} bytes enviados (paquete #{})",
                                    bytes.len(),
                                    count
                                );
                            }

                            bytes
                        }
                        Err(e) => {
                            eprintln!("❌ Error mapping audio buffer: {:?}", e);
                            return Err(gst::FlowError::Error);
                        }
                    }
                } else {
                    eprintln!("⚠️  Buffer de audio vacío recibido");
                    return Ok(gst::FlowSuccess::Ok);
                };

                // Enviar datos de audio de forma no-bloqueante
                let _ = tx_audio.send_replace(data);
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
}
