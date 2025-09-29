//! Utilidades auxiliares para la gestión de cámara.
//!
//! Funciones helper para configuración, validación y operaciones comunes.

use crate::error::VigilanteError;

pub struct CameraUtils;

impl CameraUtils {
    /// Valida una URL de cámara RTSP con reglas de seguridad
    pub fn validate_url(url: &str) -> Result<(), VigilanteError> {
        // Verificar que no esté vacío
        if url.trim().is_empty() {
            return Err(VigilanteError::Parse("URL cannot be empty".to_string()));
        }

        // Verificar que empiece con rtsp://
        if !url.starts_with("rtsp://") {
            return Err(VigilanteError::Parse(
                "URL must start with rtsp://".to_string(),
            ));
        }

        // Verificar que tenga un host válido (no localhost o IPs privadas en producción)
        let url_without_scheme = &url[7..]; // Remover "rtsp://"
        let host_end = url_without_scheme
            .find('/')
            .unwrap_or(url_without_scheme.len());
        let host = &url_without_scheme[..host_end];

        // Verificar que no sea localhost o IPs privadas
        if host == "localhost" || host == "127.0.0.1" || host == "::1" {
            return Err(VigilanteError::Auth(
                "Localhost connections not allowed".to_string(),
            ));
        }

        // Verificar IPs privadas (10.x.x.x, 172.16-31.x.x, 192.168.x.x)
        if let Some(ip) = host.split(':').next() {
            if Self::is_private_ip(ip) {
                return Err(VigilanteError::Auth(
                    "Private IP addresses not allowed".to_string(),
                ));
            }
        }

        // Verificar longitud máxima
        if url.len() > 2048 {
            return Err(VigilanteError::Parse(
                "URL too long (max 2048 characters)".to_string(),
            ));
        }

        // Verificar caracteres peligrosos
        if url.contains("..") || url.contains("<") || url.contains(">") {
            return Err(VigilanteError::Parse(
                "URL contains invalid characters".to_string(),
            ));
        }

        Ok(())
    }

    /// Verifica si una IP es privada
    fn is_private_ip(ip: &str) -> bool {
        let parts: Vec<&str> = ip.split('.').collect();
        if parts.len() != 4 {
            return false;
        }

        let first: u8 = parts[0].parse().unwrap_or(0);
        let second: u8 = parts[1].parse().unwrap_or(0);

        // 10.x.x.x
        if first == 10 {
            return true;
        }
        // 172.16.x.x - 172.31.x.x
        if first == 172 && (16..=31).contains(&second) {
            return true;
        }
        // 192.168.x.x
        if first == 192 && second == 168 {
            return true;
        }

        false
    }

    /// Extrae credenciales de una URL RTSP de forma segura
    /// ADVERTENCIA: Esta función extrae credenciales que NO deberían ser logueadas
    pub fn parse_credentials(url: &str) -> Option<(String, String)> {
        if let Some(at_pos) = url.find('@') {
            let before_at = &url[..at_pos];
            if let Some(colon_pos) = before_at.rfind(':') {
                // Verificar que no sea parte del esquema rtsp://
                if !before_at[..colon_pos].contains("://") {
                    let user = before_at[..colon_pos].to_string();
                    let pass = before_at[colon_pos + 1..].to_string();

                    // Validar que las credenciales no estén vacías y no contengan caracteres peligrosos
                    if !user.is_empty()
                        && !pass.is_empty()
                        && !user.contains('\n')
                        && !pass.contains('\n')
                        && !user.contains('\r')
                        && !pass.contains('\r')
                    {
                        return Some((user, pass));
                    }
                }
            }
        }
        None
    }

    /// Valida un token de autenticación
    pub fn validate_token(token: &str) -> Result<(), VigilanteError> {
        // Verificar que no esté vacío
        if token.trim().is_empty() {
            return Err(VigilanteError::Auth("Token cannot be empty".to_string()));
        }

        // Verificar longitud (mínimo 16 caracteres para seguridad)
        if token.len() < 16 {
            return Err(VigilanteError::Auth(
                "Token too short (minimum 16 characters)".to_string(),
            ));
        }

        // Verificar longitud máxima
        if token.len() > 512 {
            return Err(VigilanteError::Auth(
                "Token too long (maximum 512 characters)".to_string(),
            ));
        }

        // Verificar caracteres válidos (solo alfanuméricos, guiones, underscores, puntos)
        if !token
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            return Err(VigilanteError::Auth(
                "Token contains invalid characters (only alphanumeric, -, _, . allowed)"
                    .to_string(),
            ));
        }

        // Verificar que no contenga palabras comunes de diccionario
        let common_words = ["password", "admin", "root", "user", "test", "123456"];
        let token_lower = token.to_lowercase();
        for word in &common_words {
            if token_lower.contains(word) {
                return Err(VigilanteError::Auth(
                    "Token contains common dictionary words".to_string(),
                ));
            }
        }

        Ok(())
    }

    /// Sanitiza una URL para logging seguro (oculta credenciales)
    pub fn sanitize_url_for_logging(url: &str) -> String {
        if let Some(at_pos) = url.find('@') {
            if let Some(protocol_end) = url.find("://") {
                let protocol = &url[..protocol_end + 3]; // "rtsp://"
                let host_and_path = &url[at_pos..];
                return format!("{}[CREDENTIALS_HIDDEN]{}", protocol, host_and_path);
            }
        }
        url.to_string() // Si no hay credenciales, devolver tal cual
    }

    /// Verifica si una cadena contiene información sensible
    pub fn contains_sensitive_info(text: &str) -> bool {
        let sensitive_patterns = [
            "password",
            "passwd",
            "pwd",
            "secret",
            "token",
            "key",
            "auth",
            "credential",
            "bearer",
            "authorization",
        ];

        let text_lower = text.to_lowercase();
        sensitive_patterns
            .iter()
            .any(|pattern| text_lower.contains(pattern))
    }

    /// Genera un timestamp formateado para nombres de archivo
    pub fn format_timestamp() -> String {
        use chrono::{DateTime, Utc};
        let now: DateTime<Utc> = Utc::now();
        now.format("%Y-%m-%d_%H-%M-%S").to_string()
    }
}
