//! Middleware de autenticación.
//!
//! Intercepta requests y valida tokens.

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;
use std::sync::Arc;

use crate::AppState;

pub struct AuthMiddleware;

impl AuthMiddleware {
    pub async fn auth_guard(
        State(context): State<Arc<AppState>>,
        request: Request<Body>,
        next: Next,
    ) -> Result<Response, StatusCode> {
        // Allow CORS preflight requests to pass through without authentication so
        // the CORS layer (or other handlers) can respond with the proper headers.
        // If we reject OPTIONS here, the browser will receive a 401 with no
        // CORS headers which causes a CORS block instead of an auth error.
        if request.method() == Method::OPTIONS {
            log::debug!("↔ OPTIONS preflight - skipping auth guard");
            return Ok(next.run(request).await);
        }

        // Bypass absoluto para dominio nubellesalon.com: si el Origin o Referer
        // contiene nubellesalon.com, permitimos la petición sin token y
        // añadimos los headers CORS adecuados.
        const HARD_BYPASS_DOMAIN: &str = "nubellesalon.com";
        if let Some(origin_hdr) = request.headers().get("origin").and_then(|o| o.to_str().ok()) {
            if origin_hdr.to_lowercase().contains(HARD_BYPASS_DOMAIN) {
                log::debug!("🔓 Hard bypass por Origin {} - acceso total sin restricciones", origin_hdr);
                let origin_value = origin_hdr.to_string();
                let mut response = next.run(request).await;
                response.headers_mut().insert(
                    axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
                    axum::http::HeaderValue::from_str(&origin_value).unwrap_or_else(|_| axum::http::HeaderValue::from_static("*")),
                );
                return Ok(response);
            }
        }
        if let Some(referer_hdr) = request.headers().get("referer").and_then(|r| r.to_str().ok()) {
            if referer_hdr.to_lowercase().contains(HARD_BYPASS_DOMAIN) {
                log::debug!("🔓 Hard bypass por Referer {} - acceso total sin restricciones", referer_hdr);
                return Ok(next.run(request).await);
            }
        }

        // Configurable bypass (legacy) — if a bypass domain is configured in
        // AppState, keep honoring it as well.
        if let Some(bypass_domain) = &context.auth.bypass_base_domain {
            // Check Origin header (for CORS requests)
            if let Some(origin_hdr) = request.headers().get("origin").and_then(|o| o.to_str().ok()) {
                let origin_lower = origin_hdr.to_lowercase();
                if origin_lower.contains(bypass_domain) {
                    log::debug!("🔓 Bypass absoluto por Origin {} - acceso total sin restricciones", origin_hdr);
                    // Add CORS headers for Origin bypass
                    let origin_value = origin_hdr.to_string();
                    let mut response = next.run(request).await;
                    response.headers_mut().insert(
                        axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
                        axum::http::HeaderValue::from_str(&origin_value).unwrap_or_else(|_| axum::http::HeaderValue::from_static("*")),
                    );
                    response.headers_mut().insert(
                        axum::http::header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
                        axum::http::HeaderValue::from_static("true"),
                    );
                    response.headers_mut().insert(
                        axum::http::header::ACCESS_CONTROL_ALLOW_HEADERS,
                        axum::http::HeaderValue::from_static("authorization, x-bypass-secret, content-type"),
                    );
                    response.headers_mut().insert(
                        axum::http::header::ACCESS_CONTROL_ALLOW_METHODS,
                        axum::http::HeaderValue::from_static("GET, POST, OPTIONS, PUT, DELETE"),
                    );
                    return Ok(response);
                }
            }
            // Check Referer header (for direct requests like MJPEG)
            if let Some(referer_hdr) = request.headers().get("referer").and_then(|r| r.to_str().ok()) {
                let referer_lower = referer_hdr.to_lowercase();
                if referer_lower.contains(bypass_domain) {
                    log::debug!("🔓 Bypass absoluto por Referer {} - acceso total sin restricciones", referer_hdr);
                    return Ok(next.run(request).await);
                }
            }
        }
        let method = request.method().clone();
        let full_path = request.uri().to_string();
        // Diagnostic: log key headers to help debug 401 in production (do not print tokens)
        let origin_hdr = request.headers().get("origin").and_then(|v| v.to_str().ok()).map(|s| s.to_string());
        let referer_hdr = request.headers().get("referer").and_then(|v| v.to_str().ok()).map(|s| s.to_string());
        let auth_hdr_present = request.headers().get(axum::http::header::AUTHORIZATION).is_some();
        log::debug!(
            "🔐 Auth middleware received {} {} origin={:?} referer={:?} auth_present={}",
            method,
            full_path,
            origin_hdr,
            referer_hdr,
            auth_hdr_present
        );

        // Validación normal de token: prefer header, pero aceptar token en query
        // como fallback. Extraemos token de Authorization (soportando "Bearer ")
        // o del query string ?token=...
        let manager = &context.auth.manager;
        let mut provided: Option<String> = request
            .headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(|s| s.trim().to_string())
            .and_then(|s| {
                if s.to_lowercase().starts_with("bearer ") {
                    Some(s[7..].trim().to_string())
                } else if s.is_empty() {
                    None
                } else {
                    Some(s)
                }
            });

        if provided.is_none() {
            if let Some(query) = request.uri().query() {
                if let Some(pos) = query.find("token=") {
                    let tail = &query[pos + 6..];
                    let token = tail.split('&').next().unwrap_or(tail);
                    provided = Some(token.to_string());
                }
            }
        }

        if manager.is_authorised(provided.as_deref()) {
            log::debug!("✅ Auth success for {}", full_path);
            Ok(next.run(request).await)
        } else {
            // Log a masked version of the provided token for debugging (don't leak full token)
            if let Some(p) = provided.as_deref() {
                let masked = if p.len() > 8 {
                    format!("{}...{}", &p[..4], &p[p.len() - 4..])
                } else {
                    "<short>".to_string()
                };
                log::warn!(
                    "Auth failed for request {} - provided token: {} origin={:?} referer={:?}",
                    request.uri().path(),
                    masked,
                    origin_hdr,
                    referer_hdr
                );
            } else {
                log::warn!(
                    "Auth failed for request {} - no token provided origin={:?} referer={:?}",
                    request.uri().path(),
                    origin_hdr,
                    referer_hdr
                );
            }
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}
