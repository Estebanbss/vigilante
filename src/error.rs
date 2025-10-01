//! Tipos de error personalizados para Vigilante.
//!
//! Proporciona errores estructurados con contexto para mejor debugging
//! y manejo de errores en producción.

use std::fmt;

/// Error principal de la aplicación Vigilante
#[derive(Debug)]
pub enum VigilanteError {
    /// Errores de configuración
    Config(String),
    /// Errores de base de datos
    Database(String),
    /// Errores de streaming
    Streaming(String),
    /// Errores de autenticación
    Auth(String),
    /// Errores de PTZ
    Ptz(String),
    /// Errores de GStreamer
    GStreamer(String),
    /// Errores de WebRTC
    WebRTC(String),
    /// Errores de I/O
    Io(std::io::Error),
    /// Errores de parsing
    Parse(String),
    /// Errores HTTP
    Http(String),
    /// Errores genéricos
    Other(String),
}

impl fmt::Display for VigilanteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VigilanteError::Config(msg) => write!(f, "Config error: {}", msg),
            VigilanteError::Database(msg) => write!(f, "Database error: {}", msg),
            VigilanteError::Streaming(msg) => write!(f, "Streaming error: {}", msg),
            VigilanteError::Auth(msg) => write!(f, "Auth error: {}", msg),
            VigilanteError::Ptz(msg) => write!(f, "PTZ error: {}", msg),
            VigilanteError::GStreamer(msg) => write!(f, "GStreamer error: {}", msg),
            VigilanteError::WebRTC(msg) => write!(f, "WebRTC error: {}", msg),
            VigilanteError::Io(err) => write!(f, "IO error: {}", err),
            VigilanteError::Parse(msg) => write!(f, "Parse error: {}", msg),
            VigilanteError::Http(msg) => write!(f, "HTTP error: {}", msg),
            VigilanteError::Other(msg) => write!(f, "Error: {}", msg),
        }
    }
}

impl std::error::Error for VigilanteError {}

impl From<std::io::Error> for VigilanteError {
    fn from(err: std::io::Error) -> Self {
        VigilanteError::Io(err)
    }
}

impl From<rusqlite::Error> for VigilanteError {
    fn from(err: rusqlite::Error) -> Self {
        VigilanteError::Database(err.to_string())
    }
}

impl From<serde_json::Error> for VigilanteError {
    fn from(err: serde_json::Error) -> Self {
        VigilanteError::Parse(format!("JSON error: {}", err))
    }
}

impl From<&str> for VigilanteError {
    fn from(err: &str) -> Self {
        VigilanteError::Other(err.to_string())
    }
}

impl From<String> for VigilanteError {
    fn from(err: String) -> Self {
        VigilanteError::Other(err)
    }
}

impl From<gstreamer::glib::BoolError> for VigilanteError {
    fn from(err: gstreamer::glib::BoolError) -> Self {
        VigilanteError::GStreamer(format!("GStreamer BoolError: {}", err))
    }
}

impl From<webrtc::Error> for VigilanteError {
    fn from(err: webrtc::Error) -> Self {
        VigilanteError::WebRTC(format!("WebRTC error: {}", err))
    }
}

impl From<gstreamer::StateChangeError> for VigilanteError {
    fn from(err: gstreamer::StateChangeError) -> Self {
        VigilanteError::GStreamer(format!("GStreamer StateChangeError: {:?}", err))
    }
}

impl axum::response::IntoResponse for VigilanteError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match &self {
            VigilanteError::Auth(_) => (axum::http::StatusCode::UNAUTHORIZED, self.to_string()),
            VigilanteError::Config(_) | VigilanteError::Database(_) | VigilanteError::Io(_) => (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                self.to_string(),
            ),
            VigilanteError::Ptz(_)
            | VigilanteError::GStreamer(_)
            | VigilanteError::Streaming(_)
            | VigilanteError::WebRTC(_) => {
                (axum::http::StatusCode::BAD_REQUEST, self.to_string())
            }
            VigilanteError::Parse(_) | VigilanteError::Http(_) | VigilanteError::Other(_) => {
                (axum::http::StatusCode::BAD_REQUEST, self.to_string())
            }
        };

        axum::response::Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(format!(
                "{{\"error\": \"{}\"}}",
                message
            )))
            .unwrap()
    }
}

/// Result type alias para simplificar el código
pub type Result<T> = std::result::Result<T, VigilanteError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vigilante_error_display() {
        let err = VigilanteError::Config("test config error".to_string());
        assert_eq!(format!("{}", err), "Config error: test config error");

        let err = VigilanteError::Auth("invalid token".to_string());
        assert_eq!(format!("{}", err), "Auth error: invalid token");
    }

    #[test]
    fn test_error_from_conversions() {
        // Test From<String>
        let err: VigilanteError = "generic error".to_string().into();
        assert!(matches!(err, VigilanteError::Other(_)));

        // Test From<&str>
        let err: VigilanteError = "string error".into();
        assert!(matches!(err, VigilanteError::Other(_)));

        // Test From<std::io::Error>
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err: VigilanteError = io_err.into();
        assert!(matches!(err, VigilanteError::Io(_)));
    }

    #[test]
    fn test_error_is_error_trait() {
        let err = VigilanteError::Database("connection failed".to_string());
        // Verificar que implementa std::error::Error
        let _error: &dyn std::error::Error = &err;
    }
}
