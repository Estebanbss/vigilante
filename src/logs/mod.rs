//! Módulo de logs para Vigilante.
//!
//! Gestiona el registro y streaming de logs del sistema.

pub mod depends;

pub use depends::journal::LogReader;

use crate::AppState;
use axum::{extract::{Path, Query, State}, http::{StatusCode, header}, response::sse::Sse, response::Json, response::IntoResponse};
use tokio::time::{sleep, Duration as TokioDuration, Instant};
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
#[derive(serde::Deserialize)]
pub struct EntriesQuery {
    pub start: Option<usize>,
    pub page_size: Option<usize>,
    pub format: Option<String>,
}

pub async fn get_log_entries_handler(
    Path(date): Path<String>,
    Query(q): Query<EntriesQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let storage_path = env::var("STORAGE_PATH").map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "STORAGE_PATH not set".to_string(),
        )
    })?;

    let reader = LogReader::new(storage_path);

    let start = q.start.unwrap_or(0);
    let page_size = q.page_size.unwrap_or(1000).min(5000);

    // Also include total_count so clients can know if more pages exist
    let total_count = match reader.count_lines(&date).await {
        Ok(c) => c,
        Err(_) => 0usize,
    };

    match reader.get_log_entries_paginated(&date, start, page_size).await {
        Ok(entries) => {
            let returned = entries.len();
            let next_start = start.saturating_add(returned);
            let has_more = next_start < total_count;

            // If client requested NDJSON, stream newline-delimited JSON lines
            if let Some(fmt) = &q.format {
                if fmt.to_lowercase() == "ndjson" {
                    // Build ndjson body
                    let mut lines: Vec<String> = Vec::with_capacity(returned);
                    for (line_no, text) in entries.iter() {
                        let obj = json!({"line": line_no, "text": text});
                        lines.push(obj.to_string());
                    }
                    let body = lines.join("\n");
                    return Ok(Json(json!({
                        "date": date,
                        "start": start,
                        "page_size": page_size,
                        "total_count": total_count,
                        "returned": returned,
                        "next_start": next_start,
                        "has_more": has_more,
                        "ndjson": body
                    })));
                }
            }

            Ok(Json(json!({
                "date": date,
                "start": start,
                "page_size": page_size,
                "total_count": total_count,
                "returned": returned,
                "next_start": next_start,
                "has_more": has_more,
                "entries": entries
            })))
        }
        Err(e) => Err((
            StatusCode::NOT_FOUND,
            format!("No logs found for date {}: {}", date, e),
        )),
    }
}

/// Raw NDJSON handler: returns newline-delimited JSON objects with Content-Type application/x-ndjson
pub async fn get_log_entries_ndjson_handler(
    Path(date): Path<String>,
    Query(q): Query<EntriesQuery>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    let storage_path = env::var("STORAGE_PATH").map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "STORAGE_PATH not set".to_string(),
        )
    })?;

    let reader = LogReader::new(storage_path);

    let start = q.start.unwrap_or(0);
    let page_size = q.page_size.unwrap_or(1000).min(5000);

    let total_count = match reader.count_lines(&date).await {
        Ok(c) => c,
        Err(_) => 0usize,
    };

    match reader.get_log_entries_paginated(&date, start, page_size).await {
        Ok(entries) => {
            // Build ndjson lines
            let mut nd_lines: Vec<String> = Vec::with_capacity(entries.len());
            for (line_no, text) in entries.iter() {
                let obj = json!({"line": line_no, "text": text});
                nd_lines.push(obj.to_string());
            }
            let body = nd_lines.join("\n");
            let returned = entries.len();
            let next_start = start.saturating_add(returned);
            let has_more = next_start < total_count;

            let mut builder = axum::response::Response::builder();
            builder = builder
                .status(200)
                .header(header::CONTENT_TYPE, "application/x-ndjson")
                .header("X-Returned", returned.to_string())
                .header("X-Next-Start", next_start.to_string())
                .header("X-Has-More", if has_more { "1" } else { "0" });

            let response = builder
                .body(axum::body::Body::from(body))
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to build response: {}", e)))?;
            Ok(response)
        }
        Err(e) => Err((
            StatusCode::NOT_FOUND,
            format!("No logs found for date {}: {}", date, e),
        )),
    }
}

/// Long-polling endpoint: GET /api/logs/entries/:date/poll
/// Query params:
/// - since: optional usize (line number already received). If omitted, defaults to 0.
/// - timeout: optional seconds to wait (default 30)
/// - page_size: optional max number of lines to return in one response (default 1000)
pub async fn poll_log_entries_handler(
    Path(date): Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let storage_path = env::var("STORAGE_PATH").map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "STORAGE_PATH not set".to_string(),
        )
    })?;

    let reader = LogReader::new(storage_path);

    let since: usize = q
        .get("since")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let timeout_secs: u64 = q
        .get("timeout")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(30);
    let page_size: usize = q
        .get("page_size")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1000)
        .min(5000);

    let deadline = Instant::now() + TokioDuration::from_secs(timeout_secs);

    loop {
        // Get current total count
        let total = match reader.count_lines(&date).await {
            Ok(c) => c,
            Err(_) => 0usize,
        };

        if total > since {
            // There are new lines; return from `since` (0-based) up to page_size
            let entries = match reader
                .get_log_entries_paginated(&date, since, page_size)
                .await
            {
                Ok(e) => e,
                Err(e) => {
                    return Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Failed to read log entries: {}", e),
                    ))
                }
            };
            let returned = entries.len();
            let next_start = since.saturating_add(returned);
            let has_more = next_start < total;
            return Ok(Json(json!({
                "date": date,
                "since": since,
                "returned": returned,
                "total_count": total,
                "next_start": next_start,
                "has_more": has_more,
                "entries": entries
            })));
        }

        if Instant::now() >= deadline {
            // Timeout reached, return empty set with current total
            let next_start = since;
            let has_more = next_start < total;
            return Ok(Json(json!({
                "date": date,
                "since": since,
                "returned": 0,
                "total_count": total,
                "next_start": next_start,
                "has_more": has_more,
                "entries": []
            })));
        }

        // Sleep a short interval and retry
        sleep(TokioDuration::from_millis(700)).await;
    }
}

/// Handler para streaming de logs en tiempo real vía SSE.
/// Ruta: GET /api/logs/stream
/// SSE stream for real-time logs.
/// Accepts optional query param `date=YYYY-MM-DD` but will stream all new logs.
pub async fn stream_logs_sse(
    State(state): State<std::sync::Arc<AppState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl axum::response::IntoResponse {
    let mut rx = state.log_tx().subscribe();

    // We'll include incremental IDs based on a counter so clients can track
    // which messages they received. If the client reconnects, they can supply
    // the last_event_id in standard SSE reconnection semantics.
    let mut counter: u64 = 0;

    let stream = stream::poll_fn(move |cx| {
        match rx.try_recv() {
            Ok(msg) => {
                counter = counter.saturating_add(1);
                let mut event = axum::response::sse::Event::default().data(msg);
                event = event.id(counter.to_string());
                Poll::Ready(Some(Ok::<axum::response::sse::Event, Infallible>(event)))
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                // No message available, but keep polling
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => Poll::Ready(None),
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                // Lagged: notify client to refresh backlog via HTTP
                counter = counter.saturating_add(1);
                let mut event = axum::response::sse::Event::default().data("Log stream lagged: please refresh backlog via /api/logs/entries");
                event = event.id(counter.to_string());
                Poll::Ready(Some(Ok::<axum::response::sse::Event, Infallible>(event)))
            }
        }
    });

    let sse = Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    );

    let mut resp = sse.into_response();
    resp.headers_mut()
        .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, header::HeaderValue::from_static("*"));
    resp
}
