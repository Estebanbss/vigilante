//! Módulo de autenticación para Vigilante.
//!
//! Gestiona validación de tokens y middleware de seguridad.

use crate::error::VigilanteError;

pub mod depends;

pub use depends::bypass::BypassHandler;
pub use depends::check::TokenChecker;
pub use depends::extractor::RequireAuth;
pub use depends::middleware::AuthMiddleware;

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
    // Capture Origin header early because `req` will be moved into auth_guard.
    use axum::http::{header, HeaderValue};
    let origin_header: Option<String> = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // Usar AuthMiddleware para validación completa con bypass. Clone the state
    // so we don't move the original Arc out of this function.
    match crate::auth::depends::middleware::AuthMiddleware::auth_guard(
        axum::extract::State(context.clone()),
        req,
        next,
    )
    .await
    {
        Ok(response) => response,
        Err(status) => {
            // When returning an error here, ensure we include basic CORS headers so
            // browsers receive Access-Control-Allow-Origin and can show the real
            // 401 instead of a generic CORS blocking error. We attempt to echo the
            // Origin header if present.

            let mut builder = axum::response::Response::builder().status(status);

            if let Some(orig) = origin_header.as_deref() {
                if let Ok(hv) = HeaderValue::from_str(orig) {
                    builder = builder.header(header::ACCESS_CONTROL_ALLOW_ORIGIN, hv);
                }
            }

            // Always advertise these common CORS headers so the browser can complete
            // the preflight and read the error response.
            builder = builder
                .header(header::ACCESS_CONTROL_ALLOW_METHODS, "GET,POST,OPTIONS")
                .header(
                    header::ACCESS_CONTROL_ALLOW_HEADERS,
                    "Authorization,Content-Type,Accept,Origin",
                )
                .header(header::ACCESS_CONTROL_ALLOW_CREDENTIALS, "true");

            builder
                .body(axum::body::Body::empty())
                .unwrap()
        }
    }
}
