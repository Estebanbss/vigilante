//! Manejo de bypass de autenticación.
//!
//! Permite acceso sin token en ciertos casos.

use axum::http::HeaderMap;

pub struct BypassHandler;

impl BypassHandler {
    pub fn is_bypass_auth(_headers: &HeaderMap) -> bool {
        // Lógica para determinar si se permite bypass
        false
    }
}