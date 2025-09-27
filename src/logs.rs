use crate::AppState;
use axum::body::Body;
use axum::{extract::State, http::StatusCode, response::Response};
use serde::Deserialize;
use std::sync::Arc;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

#[derive(Debug, Deserialize)]
pub struct LogsParams {
    pub unit: Option<String>,
    // Optional: last N lines before follow
    pub n: Option<u32>,
}

// GET /api/logs/stream?unit=vigilante.service&n=0
// Streams journald logs in real-time similar to `journalctl -u UNIT -f`
pub async fn stream_journal_logs(
    State(_state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<LogsParams>,
) -> Result<Response, StatusCode> {
    let unit = params
        .unit
        .unwrap_or_else(|| "vigilante.service".to_string());
    let n = params.n.unwrap_or(0).to_string();

    // Spawn journalctl -f
    let mut child = Command::new("journalctl")
        .args(["-u", &unit, "-f", "-n", &n, "-o", "cat"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let stdout = child
        .stdout
        .take()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut reader = BufReader::new(stdout);

    let stream = async_stream::stream! {
        let mut buf = String::new();
        // Send an initial SSE header comment to help some clients
        yield Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b":connected\n\n"));
        loop {
            buf.clear();
            let read = reader.read_line(&mut buf).await?;
            if read == 0 {
                // EOF (journalctl exited). End stream.
                break;
            }
            // Format as SSE
            let line = format!("data: {}\n\n", buf.trim_end_matches(['\n', '\r']));
            yield Ok(axum::body::Bytes::from(line));
        }
    };

    let mut resp = Response::new(Body::from_stream(stream));
    let headers = resp.headers_mut();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        "text/event-stream".parse().unwrap(),
    );
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        "no-cache".parse().unwrap(),
    );
    headers.insert(
        axum::http::header::CONNECTION,
        "keep-alive".parse().unwrap(),
    );
    headers.insert("X-Accel-Buffering", "no".parse().unwrap());
    // CORS headers for streaming
    headers.insert("Access-Control-Allow-Origin", "*".parse().unwrap());
    headers.insert(
        "Access-Control-Allow-Methods",
        "GET, POST, OPTIONS".parse().unwrap(),
    );
    headers.insert("Access-Control-Allow-Headers", "*".parse().unwrap());
    Ok(resp)
}
