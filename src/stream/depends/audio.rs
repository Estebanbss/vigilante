//! Streaming de audio.
//!
//! Gestiona la transmisión de audio en tiempo real.

use crate::error::VigilanteError;
use crate::AppState;
use std::sync::Arc;
use tokio::sync::broadcast;

pub struct AudioStreamer {
    context: Arc<AppState>,
    audio_rx: broadcast::Receiver<bytes::Bytes>,
}

impl AudioStreamer {
    pub fn new(context: Arc<AppState>) -> Self {
        let audio_rx = context.streaming.audio_mp3_tx.subscribe();
        Self { context, audio_rx }
    }

    pub async fn start_stream(&self) -> Result<(), VigilanteError> {
        // Audio streaming is handled by the handlers, this just initializes
        Ok(())
    }

    pub async fn get_audio_chunk(&mut self) -> Option<Vec<u8>> {
        match self.audio_rx.recv().await {
            Ok(chunk) => {
                if chunk.is_empty() {
                    None
                } else {
                    Some(chunk.to_vec())
                }
            }
            Err(_) => None, // Channel closed
        }
    }

    pub fn is_audio_available(&self) -> bool {
        *self.context.streaming.audio_available.lock().unwrap()
    }
}
