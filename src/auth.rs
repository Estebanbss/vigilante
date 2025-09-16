use axum::http::{HeaderMap, StatusCode};

/// Comprueba la validez del token de autenticación en los encabezados de la petición.
///
/// La función espera un encabezado `Authorization: Bearer <token>`.
pub async fn check_auth(headers: &HeaderMap, token: &str) -> Result<(), StatusCode> {
    // Busca el encabezado "Authorization".
    let auth_header_value = headers.get("authorization");

    // Si el encabezado no existe, rechaza la solicitud.
    let auth_header = match auth_header_value {
        Some(value) => value.to_str().unwrap_or(""),
        None => {
            eprintln!("❌ Solicitud rechazada: Encabezado de autenticación ausente.");
            return Err(StatusCode::UNAUTHORIZED);
        }
    };

    // Construye el token esperado en el formato "Bearer <token>".
    let expected_token = format!("Bearer {}", token);

    // Compara el token del encabezado con el token esperado.
    if auth_header != expected_token {
        eprintln!("❌ Solicitud rechazada: Token de autenticación inválido.");
        Err(StatusCode::UNAUTHORIZED)
    } else {
        Ok(())
    }
}
