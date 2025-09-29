//! Módulo de autenticación para Vigilante.
//!
//! Gestiona validación de tokens y middleware de seguridad.

use crate::error::VigilanteError;

pub mod depends;

pub use depends::middleware::AuthMiddleware;
pub use depends::check::TokenChecker;
pub use depends::extractor::RequireAuth;
pub use depends::bypass::BypassHandler;

#[derive(Clone, Debug, Default)]
pub struct AuthConfig {
    pub bearer_token: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct AuthManager {
    settings: AuthConfig,
}

impl AuthManager {
    pub fn new(settings: AuthConfig) -> Result<Self, VigilanteError> {
        // Validar el token si está presente
        if let Some(ref token) = settings.bearer_token {
            crate::camera::depends::utils::CameraUtils::validate_token(token)?;
        }

        Ok(Self { settings })
    }

    pub fn is_authorised(&self, provided: Option<&str>) -> bool {
        match (&self.settings.bearer_token, provided) {
            (Some(expected), Some(value)) => expected == value,
            (None, _) => true,
            _ => false,
        }
    }

    pub fn settings(&self) -> AuthConfig {
        self.settings.clone()
    }
}

pub async fn flexible_auth_middleware(
    axum::extract::State(context): axum::extract::State<std::sync::Arc<crate::AppState>>,
    req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    // Usar AuthMiddleware para validación completa con bypass
    match crate::auth::depends::middleware::AuthMiddleware::auth_guard(axum::extract::State(context), req, next).await {
        Ok(response) => response,
        Err(status) => axum::response::Response::builder()
            .status(status)
            .body(axum::body::Body::empty())
            .unwrap(),
    }
}