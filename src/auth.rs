use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::body::Body;
use axum::middleware::Next;
use axum::response::Response;
use std::sync::Arc;
use crate::AppState;
use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;

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
        println!("🛫 CORS preflight bypass: {} {}", req.method(), req.uri().path());
        return next.run(req).await;
    }

    let headers = req.headers();
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    match check_auth(headers, &state.proxy_token).await {
        Ok(()) => {
            println!("✅ Auth OK (middleware): {} {}", method, path);
            next.run(req).await
        }
        Err(status) => {
            println!("❌ Auth FAIL (middleware): {} {} -> {}", method, path, status.as_u16());
            Response::builder()
                .status(status)
                .body(Body::from("Unauthorized"))
                .unwrap()
        }
    }
}

/// Extractor que exige Authorization en cada handler que lo use.
/// Esto añade una segunda capa de protección además del middleware global.
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
        let app_state = Arc::<AppState>::from_ref(state);
        let headers = &parts.headers;
        let method = parts.method.clone();
        let path = parts.uri.path().to_string();
        match check_auth(headers, &app_state.proxy_token).await {
            Ok(()) => {
                println!("🔐 Auth OK (extractor): {} {}", method, path);
                Ok(RequireAuth)
            }
            Err(_) => {
                println!("🚫 Auth FAIL (extractor): {} {}", method, path);
                Err((StatusCode::UNAUTHORIZED, "Unauthorized"))
            }
        }
    }
}
