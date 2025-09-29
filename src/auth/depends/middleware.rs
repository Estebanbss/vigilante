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
                        if let Some(provided_secret) = request.headers().get("x-bypass-secret").and_then(|s| s.to_str().ok()) {
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

        // Validación normal de token: por header o por query si permitido
        let manager = &context.auth.manager;
        let mut provided = request
            .headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.trim_start_matches("Bearer "));

        // Si no hay token en header y se permite por query, buscar en query
        if provided.is_none() && context.auth.allow_query_token_streams {
            if let Some(query) = request.uri().query() {
                if let Some(token_start) = query.find("token=") {
                    let token_part = &query[token_start + 6..];
                    let token = token_part.split('&').next().unwrap_or(token_part);
                    provided = Some(token);
                }
            }
        }

        if manager.is_authorised(provided) {
            Ok(next.run(request).await)
        } else {
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}