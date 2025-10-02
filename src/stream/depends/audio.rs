//! Streaming de audio.
//!
//! Gestiona la transmisión de audio en tiempo real.

use crate::error::VigilanteError;
use std::sync::Arc;
use tokio::sync::broadcast;

#[derive(Debug)]
pub struct AudioStreamer {
    streaming_state: Arc<crate::state::StreamingState>,
    audio_rx: broadcast::Receiver<bytes::Bytes>,
}

impl AudioStreamer {
    pub fn new(streaming_state: Arc<crate::state::StreamingState>) -> Self {
        let audio_rx = streaming_state.audio_mp3_tx.subscribe();
        Self {
            streaming_state,
            audio_rx,
        }
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
        *self.streaming_state.audio_available.lock().unwrap()
    }
}
