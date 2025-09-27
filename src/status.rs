use crate::{auth::RequireAuth, AppState};
use axum::{extract::State, Json};
use std::sync::Arc;

#[derive(Clone, Debug, serde::Serialize)]
pub struct SimpleStatus {
    pub audio: bool,
    pub camera: bool,
    pub recordings: bool,
}

async fn compute_simple_status(state: &Arc<AppState>) -> (SimpleStatus, crate::RecordingSnapshot) {
    let camera_ok = state.pipeline.lock().await.is_some();
    let audio_ok = *state.audio_available.lock().await;

    let snapshot = { state.recording_snapshot.lock().await.clone() };
    let now = chrono::Utc::now();
    let recent_threshold = chrono::Duration::minutes(5);
    let recordings_ok = snapshot
        .latest_timestamp
        .map(|ts| now.signed_duration_since(ts) <= recent_threshold)
        .unwrap_or(false);

    let simple = SimpleStatus {
        audio: audio_ok,
        camera: camera_ok,
        recordings: recordings_ok,
    };

    (simple, snapshot)
}

pub async fn get_system_status(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Json<SimpleStatus> {
    let (simple, snapshot) = compute_simple_status(&state).await;

    {
        let mut status = state.system_status.lock().await;
        status.uptime_seconds = std::time::SystemTime::now()
            .duration_since(state.start_time)
            .unwrap_or_default()
            .as_secs();
        status.last_updated = chrono::Utc::now();
        status.pipeline_status.is_running = simple.camera;
        status.audio_status.available = simple.audio;
        status.storage_status.recording_count = snapshot.total_count;
        status.storage_status.last_recording = snapshot.last_recording.clone();
    }

    Json(simple)
}
