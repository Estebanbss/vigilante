use crate::{auth::RequireAuth, AppState, SystemStatus};
use axum::{extract::State, Json};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use std::sync::Arc;
use std::sync::atomic::Ordering;

// Endpoint general de estado del sistema
pub async fn get_system_status(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Json<SystemStatus> {
    let mut status = state.system_status.lock().await.clone();

    // Actualizar uptime y timestamp
    status.uptime_seconds = std::time::SystemTime::now()
        .duration_since(state.start_time)
        .unwrap_or_default()
        .as_secs();
    status.last_updated = chrono::Utc::now();

        // Verificar estado del pipeline
    let pipeline_guard = state.pipeline.lock().await;
    status.pipeline_status.is_running = pipeline_guard.is_some();

    // Actualizar información de almacenamiento
    if let Ok(stats) = fs2::statvfs(&state.storage_path) {
        status.storage_status.total_space_bytes = stats.total_space();
        status.storage_status.free_space_bytes = stats.free_space();
        status.storage_status.used_space_bytes = stats.total_space() - stats.free_space();
    }

    // Contar grabaciones (esto es costoso, hacerlo cada cierto tiempo)
    let recordings = crate::storage::get_recordings_recursively(&state.storage_path);
    status.storage_status.recording_count = recordings.len();

    if let Some(last_recording) = recordings
        .iter()
        .max_by_key(|r| r.last_modified)
    {
        status.storage_status.last_recording = Some(last_recording.name.clone());
    }

    Json(status)
}

// Endpoint específico para estado del pipeline
pub async fn get_pipeline_status(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Json<crate::PipelineStatus> {
    let status = state.system_status.lock().await.clone();

    // Verificar estado actual del pipeline
    let mut pipeline_status = status.pipeline_status;
    let pipeline_guard = state.pipeline.lock().await;
    pipeline_status.is_running = pipeline_guard.is_some();

    Json(pipeline_status)
}

// Endpoint específico para estado del audio
pub async fn get_audio_status(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Json<crate::AudioStatus> {
    let status = state.system_status.lock().await;
    let audio_status = crate::AudioStatus {
        available: *state.audio_available.lock().await,
        bytes_sent: state.audio_bytes_sent.load(Ordering::Relaxed),
        packets_sent: state.audio_packets_sent.load(Ordering::Relaxed),
        last_activity: status.audio_status.last_activity,
    };
    Json(audio_status)
}

// Endpoint específico para estado del almacenamiento
pub async fn get_storage_status(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Json<crate::StorageStatus> {
    let mut storage_status = state.system_status.lock().await.storage_status.clone();

    // Actualizar información en tiempo real
    if let Ok(stats) = fs2::statvfs(&state.storage_path) {
        storage_status.total_space_bytes = stats.total_space();
        storage_status.free_space_bytes = stats.free_space();
        storage_status.used_space_bytes = stats.total_space() - stats.free_space();
    }

    // Contar grabaciones
    let recordings = crate::storage::get_recordings_recursively(&state.storage_path);
    storage_status.recording_count = recordings.len();

    if let Some(last_recording) = recordings
        .iter()
        .max_by_key(|r| r.last_modified)
    {
        storage_status.last_recording = Some(last_recording.name.clone());
    }

    Json(storage_status)
}

// Función helper para generar status simplificado (camara, audio, storage)
pub async fn generate_realtime_status(state: &Arc<AppState>) -> serde_json::Value {
    let mut status = state.system_status.lock().await.clone();

    // Actualizar uptime real
    status.uptime_seconds = std::time::SystemTime::now()
        .duration_since(state.start_time)
        .unwrap_or_default()
        .as_secs();

    // Verificar estado del pipeline
    let pipeline_guard = state.pipeline.try_lock();
    let is_running = pipeline_guard.as_ref().map(|g| g.is_some()).unwrap_or(false);

    // Actualizar audio disponible
    let audio_available = *state.audio_available.lock().await;

    // Contar grabaciones (simplificado, sin detalles)
    let recording_count = crate::storage::get_recordings_recursively(&state.storage_path).len();

    // Crear JSON simplificado
    serde_json::json!({
        "camara": {
            "is_running": is_running,
            "uptime_seconds": status.uptime_seconds,
            "last_updated": chrono::Utc::now().to_rfc3339()
        },
        "audio": {
            "available": audio_available,
            "bytes_sent": state.audio_bytes_sent.load(Ordering::Relaxed),
            "packets_sent": state.audio_packets_sent.load(Ordering::Relaxed),
            "last_activity": if audio_available { Some(chrono::Utc::now().to_rfc3339()) } else { None }
        },
        "storage": {
            "recording_count": recording_count,
            "last_recording": status.storage_status.last_recording
        }
    })
}

// WebSocket endpoint para status en tiempo real
pub async fn status_websocket(
    ws: WebSocketUpgrade,
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Response {
    ws.on_upgrade(move |socket| handle_status_websocket(socket, state))
}

async fn handle_status_websocket(mut socket: WebSocket, state: Arc<AppState>) {
    println!("📡 Cliente conectado al WebSocket de status");

    // Suscribirse al canal de status
    let mut status_rx = state.status_tx.subscribe();

    // Loop principal del WebSocket
    loop {
        tokio::select! {
            // Recibir actualizaciones de status
            Ok(status_update) = status_rx.recv() => {
                if socket.send(Message::Text(status_update.to_string())).await.is_err() {
                    println!("📡 Cliente desconectado del WebSocket de status");
                    break;
                }
            }

            // Manejar mensajes del cliente (ping/pong/close)
            message = socket.recv() => {
                match message {
                    Some(Ok(Message::Close(_))) | None => {
                        println!("📡 Cliente cerró la conexión WebSocket de status");
                        break;
                    }
                    Some(Ok(Message::Ping(data))) => {
                        if socket.send(Message::Pong(data)).await.is_err() {
                            break;
                        }
                    }
                    _ => {} // Ignorar otros mensajes
                }
            }
        }
    }
}