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
        let method = request.method().clone();
        let full_path = request.uri().to_string();
        log::debug!("🔐 Auth middleware received {} {}", method, full_path);

        // Verificar bypass de dominio
        if let Some(bypass_domain) = &context.auth.bypass_base_domain {
            if let Some(host) = request.headers().get("host").and_then(|h| h.to_str().ok()) {
                // Normalizar Host: bajar a minúsculas y quitar ":puerto" si viene presente
                let host_norm = host.to_lowercase();
                let host_base = host_norm.split(':').next().unwrap_or(&host_norm);
                if host_base == bypass_domain {
                    // Si hay secreto requerido, verificar
                    if let Some(required_secret) = &context.auth.bypass_domain_secret {
                        if let Some(provided_secret) = request
                            .headers()
                            .get("x-bypass-secret")
                            .and_then(|s| s.to_str().ok())
                        {
                            if provided_secret == required_secret {
                                log::debug!(
                                    "🔓 Bypass auth granted for host {} via header secret",
                                    host_base
                                );
                                return Ok(next.run(request).await);
                            }
                        }
                        // Secreto no coincide o no proporcionado, rechazar
                        log::warn!(
                            "🔐 Bypass secret mismatch for host {}, rejecting request",
                            host_base
                        );
                        return Err(StatusCode::UNAUTHORIZED);
                    } else {
                        // No hay secreto requerido, permitir bypass
                        log::debug!("🔓 Bypass auth granted for host {} without secret", host_base);
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
                    log::debug!("🔑 Token provided via Authorization header for {}", full_path);
                    Some(s[7..].trim().to_string())
                } else if s.is_empty() {
                    None
                } else {
                    log::debug!("🔑 Token provided via non-Bearer Authorization header for {}", full_path);
                    Some(s)
                }
            });

        // Si no hay token en header, y la petición es a /api/live/*, permitir token en query
        // si la configuración lo permite. Esto mantiene la política por defecto para
        // otros endpoints mientras permite reproductores simples usar ?token= para MJPEG.
        if provided.is_none() {
            let path = request.uri().path();
            let is_live_path = path.starts_with("/api/live/");

            // Para /api/live/* aceptar query token SIEMPRE.
            // Para otros endpoints, respetar la bandera global allow_query_token_streams.
            let allow_query_here = if is_live_path {
                true
            } else {
                context.auth.allow_query_token_streams
            };

            if allow_query_here {
                if let Some(query) = request.uri().query() {
                    if let Some(token_start) = query.find("token=") {
                        let token_part = &query[token_start + 6..];
                        let token = token_part.split('&').next().unwrap_or(token_part);
                        log::debug!("🔑 Token sourced from query string for {}", full_path);
                        provided = Some(token.to_string());
                    }
                }
            }
        }

        // Compare provided token (as &str) with manager expected token
        let provided_ref: Option<&str> = provided.as_deref();
        if manager.is_authorised(provided_ref) {
            log::debug!("✅ Auth success for {}", full_path);
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
