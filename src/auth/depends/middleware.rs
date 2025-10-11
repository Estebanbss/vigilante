//! Middleware de autenticación.
//!
//! Intercepta requests y valida tokens.

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
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
        // Verificar bypass de dominio
        if let Some(bypass_domain) = &context.auth.bypass_base_domain {
            if let Some(host) = request.headers().get("host").and_then(|h| h.to_str().ok()) {
                if host == bypass_domain {
                    // Si hay secreto requerido, verificar
                    if let Some(required_secret) = &context.auth.bypass_domain_secret {
                        if let Some(provided_secret) = request
                            .headers()
                            .get("x-bypass-secret")
                            .and_then(|s| s.to_str().ok())
                        {
                            if provided_secret == required_secret {
                                return Ok(next.run(request).await);
                            }
                        }
                        // Secreto no coincide o no proporcionado, rechazar
                        return Err(StatusCode::UNAUTHORIZED);
                    } else {
                        // No hay secreto requerido, permitir bypass
                        return Ok(next.run(request).await);
                    }
                }
            }
        }

        // Validación normal de token: prefer header, pero permitir token en query
        // para endpoints de streaming cuando esté habilitado (o para /api/live/*
        // específicamente si se desea dar compatibilidad puntual).
        let manager = &context.auth.manager;

        // Extraer Authorization de forma robusta y normalizar "Bearer " prefix.
        let mut provided: Option<String> = request
            .headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.trim().to_string())
            .and_then(|s| {
                // soportar tanto "Bearer TOKEN" como solo el token
                if s.to_lowercase().starts_with("bearer ") {
                    Some(s[7..].trim().to_string())
                } else if s.is_empty() {
                    None
                } else {
                    Some(s)
                }
            });

        // Si no hay token en header, y la petición es a /api/live/*, permitir token en query
        // si la configuración lo permite. Esto mantiene la política por defecto para
        // otros endpoints mientras permite reproductores simples usar ?token= para MJPEG.
        if provided.is_none() {
            let path = request.uri().path();
            let is_live_path = path.starts_with("/api/live/");

            if is_live_path && context.auth.allow_query_token_streams {
                if let Some(query) = request.uri().query() {
                    if let Some(token_start) = query.find("token=") {
                        let token_part = &query[token_start + 6..];
                        let token = token_part.split('&').next().unwrap_or(token_part);
                        provided = Some(token.to_string());
                    }
                }
            }
        }

        // Compare provided token (as &str) with manager expected token
        let provided_ref: Option<&str> = provided.as_deref();
        if manager.is_authorised(provided_ref) {
            Ok(next.run(request).await)
        } else {
            // Log a masked version of the provided token for debugging (don't leak full token)
            if let Some(p) = provided_ref {
                let masked = if p.len() > 8 {
                    format!("{}...{}", &p[..4], &p[p.len() - 4..])
                } else {
                    "<short>".to_string()
                };
                log::warn!("Auth failed for request {} - provided token: {}", request.uri().path(), masked);
            } else {
                log::warn!("Auth failed for request {} - no token provided", request.uri().path());
            }
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}
