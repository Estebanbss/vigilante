//! Streamer MJPEG para transmisión de video en tiempo real.
//!
//! Gestiona la distribución de frames MJPEG a través de canales
//! para streaming continuo.

use crate::error::VigilanteError;
use crate::AppState;
use bytes::Bytes;
use gstreamer as gst;
use gstreamer::prelude::*;
use std::sync::Arc;

pub struct MjpegStreamer {
    pub mjpeg_tx: tokio::sync::broadcast::Sender<Bytes>,
    pub context: Arc<AppState>,
    pub appsink: Option<gst::Element>,
}

impl MjpegStreamer {
    pub fn new(context: Arc<AppState>) -> Self {
        let mjpeg_tx = context.streaming.mjpeg_tx.clone();
        Self {
            mjpeg_tx,
            context,
            appsink: None,
        }
    }

    pub fn set_appsink(&mut self, appsink: gst::Element) {
        self.appsink = Some(appsink);
    }

    pub async fn start_streaming(&self) -> Result<(), VigilanteError> {
        if let Some(ref appsink) = self.appsink {
            let tx = self.context.streaming.mjpeg_tx.clone();
            appsink.connect("new-sample", false, move |values| {
                let sample = values[1].get::<gst::Sample>().unwrap();
                let buffer = sample.buffer().unwrap();
                let map = buffer.map_readable().unwrap();
                let data = map.as_slice();
                let _ = tx.send(Bytes::copy_from_slice(data));
                None
            });
        }
        Ok(())
    }

    pub async fn get_frame(&self) -> Option<Vec<u8>> {
        // Not used, frames sent via signal
        None
    }
}
