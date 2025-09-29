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
        let provided = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.trim_start_matches("Bearer "));

        if manager.is_authorised(provided) {
            Ok(RequireAuth)
        } else {
            Err((StatusCode::UNAUTHORIZED, "Token inválido o faltante"))
        }
    }
}
