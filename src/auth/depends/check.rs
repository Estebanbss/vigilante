//! Verificación de tokens.
//!
//! Funciones para validar tokens de autenticación.

use axum::http::HeaderMap;

pub struct TokenChecker;

impl TokenChecker {
    pub fn check_auth(headers: &HeaderMap, token: &str) -> bool {
        let provided = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.trim());

        match provided {
            Some(header) if header == token => true,
            Some(header) if header.strip_prefix("Bearer ") == Some(token) => true,
            _ => false,
        }
    }
}