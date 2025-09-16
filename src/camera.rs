use crate::AppState;
use gstreamer::{ self as gst, prelude::*, Pipeline, MessageView };
use gstreamer_app as gst_app;
use std::sync::Arc;
use std::fs::{OpenOptions};
use std::io::Write;
use chrono::{Local};

// This function will handle the GStreamer pipeline for 24/7 recording and event detection
pub async fn start_camera_pipeline(camera_url: String, state: Arc<AppState>) {
    loop {
        let now = chrono::Local::now();
        let daily_filename = format!("{}.mp4", now.format("%Y-%m-%d"));
        let daily_path = state.storage_path.join(&daily_filename);
        
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
        
        // Pipeline para grabación continua en archivo diario
        let pipeline_str = format!(
            "rtspsrc location={} protocols=tcp latency=100 ! rtph264depay ! h264parse ! queue ! tee name=t \
            t. ! queue ! mp4mux name=mux ! filesink location={} sync=false append=false \
            t. ! queue ! h264parse ! appsink name=detector emit-signals=true",
            camera_url,
            shell_escape::escape(daily_path.to_string_lossy()).to_string()
        );

        println!("📷 Recording pipeline: {}", pipeline_str);
        let pipeline = match gst::parse::launch(&pipeline_str) {
            Ok(element) => element.downcast::<Pipeline>().unwrap(),
            Err(err) => {
                eprintln!("❌ Error al crear el pipeline: {}", err);
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
        appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |appsink| {
                    let sample = appsink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                    let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                    
                    // Simulate event detection
                    // In a real scenario, you would use a machine learning model to detect motion or people here
                    let has_movement = (map.len() % 1000) == 0; // A simple placeholder for now
                    let has_person = (map.len() % 5000) == 0; // A simple placeholder for now
                    
                    if has_movement || has_person {
                        let now = Local::now();
                        let log_filename = now.format("%Y-%m-%d-log.txt").to_string();
                        let log_path = state_for_cb.storage_path.join(&log_filename);
                        
                        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&log_path) {
                            let timestamp = now.format("%H:%M:%S.%3f").to_string();
                            if has_movement {
                                writeln!(file, "{} - se detectó movimiento", timestamp).ok();
                            }
                            if has_person {
                                writeln!(file, "{} - se detectó una persona", timestamp).ok();
                            }
                        }
                    }
                    
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        
        // Get the bus to receive messages from the pipeline
        let bus = pipeline.bus().unwrap();

        // Set the pipeline to "playing" state
        let _ = pipeline.set_state(gst::State::Playing);

        // Store the pipeline in the shared state
        *state.pipeline.lock().await = Some(pipeline.clone());

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
                        eprintln!("❌ Error del pipeline: {}, reiniciando...", err.error());
                        should_restart = true;
                        break;
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
        let _ = pipeline.set_state(gst::State::Null);
        
        if should_restart {
            println!("🔄 Reiniciando pipeline por error...");
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        } else {
            println!("🕛 Medianoche alcanzada, creando nuevo archivo diario...");
        }
    }
}
