//! Módulo de logs para Vigilante.
//!
//! Gestiona el registro y streaming de logs del sistema.

pub mod depends;

pub use depends::journal::LogReader;

use crate::AppState;
use axum::{extract::Path, http::{StatusCode, header}, response::sse::Sse, response::Json, response::IntoResponse};
use futures::stream::{self};
use serde_json::json;
use std::convert::Infallible;
use std::env;
use std::task::Poll;
use std::time::Duration;

/// Manager principal para logs.
#[derive(Clone)]
pub struct LogsManager {
    // Aquí iría la lógica del manager
}

impl LogsManager {
    pub fn new() -> Self {
        Self {}
    }

    pub async fn stream_logs(&self) -> Result<(), String> {
        // Lógica para streaming de logs
        Ok(())
    }
}

impl Default for LogsManager {
    fn default() -> Self {
        Self::new()
    }
}

pub async fn stream_journal_logs() -> impl axum::response::IntoResponse {
    "OK"
}

/// Handler para obtener entradas de log por fecha.
/// Ruta: GET /api/logs/entries/:date
/// Ejemplo: GET /api/logs/entries/2023-10-01
pub async fn get_log_entries_handler(
    Path(date): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let storage_path = env::var("STORAGE_PATH").map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "STORAGE_PATH not set".to_string(),
        )
    })?;

    let reader = LogReader::new(storage_path);
    match reader.get_log_entries(&date, None).await {
        Ok(entries) => Ok(Json(json!({
            "date": date,
            "entries": entries
        }))),
        Err(e) => Err((
            StatusCode::NOT_FOUND,
            format!("No logs found for date {}: {}", date, e),
        )),
    }
}

/// Handler para streaming de logs en tiempo real vía SSE.
/// Ruta: GET /api/logs/stream
pub async fn stream_logs_sse(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    let mut rx = state.log_tx().subscribe();
    let stream = stream::poll_fn(move |cx| {
        match rx.try_recv() {
            Ok(msg) => Poll::Ready(Some(Ok::<axum::response::sse::Event, Infallible>(
                axum::response::sse::Event::default().data(msg),
            ))),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                // No message available, but keep polling
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => Poll::Ready(None),
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                // Lagged, send a message
                Poll::Ready(Some(Ok::<axum::response::sse::Event, Infallible>(
                    axum::response::sse::Event::default().data("Log stream lagged"),
                )))
            }
        }
    });

    let sse = Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    );

    // As a safety fallback, ensure SSE responses include CORS header so
    // browsers receive Access-Control-Allow-Origin even if a proxy or
    // middleware misconfiguration prevents the global CORS layer from running.
    let mut resp = sse.into_response();
    resp.headers_mut()
        .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, header::HeaderValue::from_static("*"));
    resp
}
