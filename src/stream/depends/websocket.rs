//! Manejo de conexiones WebSocket.
//!
//! Gestiona conexiones bidireccionales para streaming.

use axum::extract::ws::{Message, WebSocket};
use serde_json;
use std::sync::Arc;
use crate::AppState;
use crate::error::VigilanteError;

#[derive(Clone)]
pub struct WebSocketHandler {
    context: Arc<AppState>,
}

impl WebSocketHandler {
    pub fn new(context: Arc<AppState>) -> Self {
        Self { context }
    }

    pub async fn handle_connection(&self, mut socket: WebSocket) {
        // Subscribe to MJPEG stream for sending frames
        let mut mjpeg_rx = self.context.streaming.mjpeg_tx.subscribe();

        loop {
            tokio::select! {
                // Handle incoming messages from client
                msg = socket.recv() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            // Handle text messages (commands, etc.)
                            if (self.handle_text_message(&mut socket, &text).await).is_err() {
                                break;
                            }
                        }
                        Some(Ok(Message::Close(_))) => break,
                        Some(Err(_)) => break,
                        _ => {} // Ignore other message types
                    }
                }

                // Send MJPEG frames to client
                frame = mjpeg_rx.recv() => {
                    match frame {
                        Ok(bytes) => {
                            if (socket.send(Message::Binary(bytes.to_vec())).await).is_err() {
                                break;
                            }
                        }
                        Err(_) => break, // Channel closed
                    }
                }
            }
        }
    }

    async fn handle_text_message(&self, socket: &mut WebSocket, message: &str) -> Result<(), Box<dyn std::error::Error>> {
        // Handle different message types
        match message {
            "ping" => {
                socket.send(Message::Text("pong".to_string())).await?;
            }
            "status" => {
                let status = serde_json::json!({
                    "audio_available": self.is_audio_available(),
                    "camera_connected": true // TODO: Add real camera status
                });
                socket.send(Message::Text(status.to_string())).await?;
            }
            _ => {
                // Unknown message, echo back
                socket.send(Message::Text(format!("echo: {}", message))).await?;
            }
        }
        Ok(())
    }

    pub async fn send_message(&self, message: &str) -> Result<(), VigilanteError> {
        // This method could be used to broadcast messages to all connected clients
        // For now, it's a placeholder
        log::info!("WebSocket broadcast: {}", message);
        Ok(())
    }

    fn is_audio_available(&self) -> bool {
        *self.context.streaming.audio_available.lock().unwrap()
    }
}