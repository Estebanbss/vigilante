pub mod auth;
pub mod camera;
pub mod events;
pub mod logs;
pub mod ptz;
pub mod storage;
pub mod stream;

use bytes::Bytes;
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

// Dependencias de GStreamer
use gstreamer as gst;
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct AppState {
    pub camera_rtsp_url: String,
    pub camera_onvif_url: String,
    pub proxy_token: String,
    pub storage_path: PathBuf,
    pub pipeline: Arc<Mutex<Option<gst::Pipeline>>>,
    pub mjpeg_tx: broadcast::Sender<Bytes>,
    pub mjpeg_low_tx: broadcast::Sender<Bytes>,
    pub audio_mp3_tx: broadcast::Sender<Bytes>,
    pub enable_hls: bool,
    // Permite validar token por query (p.ej., ?token=...) en rutas de streaming
    pub allow_query_token_streams: bool,
    // Writer para logging de eventos de movimiento
    pub log_writer: Arc<Mutex<Option<std::io::BufWriter<std::fs::File>>>>,
}
