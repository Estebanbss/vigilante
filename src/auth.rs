use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::body::Body;
use axum::middleware::Next;
use axum::response::Response;
use std::sync::Arc;
use crate::AppState;

/// Comprueba la validez del token de autenticación en los encabezados de la petición.
///
/// La función espera un encabezado `Authorization` con uno de estos formatos:
/// - "Bearer <token>"
/// - "<token>" (solo el token crudo)
pub async fn check_auth(headers: &HeaderMap, token: &str) -> Result<(), StatusCode> {
    // Busca el encabezado "Authorization".
    let auth_header_value = headers.get(header::AUTHORIZATION);

    // Si el encabezado no existe, rechaza la solicitud.
    let auth_header = match auth_header_value {
        Some(value) => value.to_str().unwrap_or(""),
        None => {
            eprintln!("❌ Solicitud rechazada: Encabezado de autenticación ausente.");
            return Err(StatusCode::UNAUTHORIZED);
        }
    };

    // Construye tokens válidos: "Bearer <token>" o el token crudo
    let expected_bearer = format!("Bearer {}", token);
    let expected_raw = token;

    // Compara el token del encabezado con los formatos aceptados.
    if auth_header != expected_bearer && auth_header != expected_raw {
        eprintln!("❌ Solicitud rechazada: Token de autenticación inválido.");
        Err(StatusCode::UNAUTHORIZED)
    } else {
        Ok(())
    }
}

/// Middleware global para exigir Authorization en todas las rutas.
/// Permite OPTIONS (preflight CORS) sin autenticación.
pub async fn require_auth_middleware(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    req: Request<Body>,
    next: Next,
)
-> Response {
    // Permite preflight CORS sin auth (deja que CorsLayer maneje y añada headers)
    if req.method() == Method::OPTIONS {
        return next.run(req).await;
    }

    let headers = req.headers();
    match check_auth(headers, &state.proxy_token).await {
        Ok(()) => next.run(req).await,
        Err(status) => Response::builder()
            .status(status)
            .body(Body::from("Unauthorized"))
            .unwrap(),
    }
}
