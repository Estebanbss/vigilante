//! Extractor de autenticación.
//!
//! Requiere autenticación en handlers que lo usan.

use axum::extract::{FromRef, FromRequestParts};
use axum::http::{request::Parts, StatusCode};
use std::sync::Arc;

use crate::AppState;

#[derive(Debug, Clone, Copy)]
pub struct RequireAuth;

#[axum::async_trait]
impl<S> FromRequestParts<S> for RequireAuth
where
    Arc<AppState>: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let context = Arc::<AppState>::from_ref(state);
        let manager = context.auth();
        // Prefer header Authorization but accept token in query string as fallback
        let mut provided: Option<String> = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.trim().to_string())
            .and_then(|s| {
                if s.to_lowercase().starts_with("bearer ") {
                    Some(s[7..].trim().to_string())
                } else if s.is_empty() {
                    None
                } else {
                    Some(s)
                }
            });

        // If header not present, look for token= in query string
        if provided.is_none() {
            if let Some(query) = parts.uri.query() {
                if let Some(pos) = query.find("token=") {
                    let tail = &query[pos + 6..];
                    let token = tail.split('&').next().unwrap_or(tail);
                    provided = Some(token.to_string());
                }
            }
        }

        if manager.is_authorised(provided.as_deref()) {
            Ok(RequireAuth)
        } else {
            Err((StatusCode::UNAUTHORIZED, "Token inválido o faltante"))
        }
    }
}
