use crate::{auth::RequireAuth, AppState};
use axum::{extract::State, Json};
use std::sync::Arc;

#[derive(Clone, Debug, serde::Serialize)]
pub struct SimpleStatus {
    pub audio: bool,
    pub camera: bool,
    pub recordings: bool,
}

async fn compute_simple_status(state: &Arc<AppState>) -> SimpleStatus {
    let camera_ok = state.pipeline.lock().await.is_some();
    let audio_ok = *state.audio_available.lock().unwrap();

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

    simple
}

pub async fn get_system_status(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Json<SimpleStatus> {
    let simple = compute_simple_status(&state).await;

    Json(simple)
}
