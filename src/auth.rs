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
    // Helper: mask token to avoid leaking secrets in logs
    fn mask_token(tok: &str) -> String {
        if tok.is_empty() { return "".into(); }
        let n = tok.chars().count();
        if n <= 4 { return "*".repeat(n); }
        let prefix: String = tok.chars().take(3).collect();
        let suffix: String = tok.chars().rev().take(2).collect::<String>().chars().rev().collect();
        format!("{}{}{}", prefix, "*".repeat(n.saturating_sub(5)), suffix)
    }
    fn mask_auth_header(val: &str) -> String {
        let bearer = "Bearer ";
        if let Some(rest) = val.strip_prefix(bearer) {
            format!("{}{}", bearer, mask_token(rest))
        } else {
            mask_token(val)
        }
    }

    // Busca el encabezado "Authorization".
    let auth_header_value = headers.get(header::AUTHORIZATION);

    // Si el encabezado no existe, rechaza la solicitud.
    let auth_header = match auth_header_value {
        Some(value) => value.to_str().unwrap_or(""),
        None => {
            let ua = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("");
            let cfip = headers.get("cf-connecting-ip").and_then(|v| v.to_str().ok()).unwrap_or("");
            let xff = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()).unwrap_or("");
            eprintln!("❌ Auth missing: UA='{}' CF-Connecting-IP='{}' X-Forwarded-For='{}'", ua, cfip, xff);
            return Err(StatusCode::UNAUTHORIZED);
        }
    };

    // Construye tokens válidos: "Bearer <token>" o el token crudo
    let expected_bearer = format!("Bearer {}", token);
    let expected_raw = token;

    // Compara el token del encabezado con los formatos aceptados.
    if auth_header != expected_bearer && auth_header != expected_raw {
        let ua = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("");
        let cfip = headers.get("cf-connecting-ip").and_then(|v| v.to_str().ok()).unwrap_or("");
        let xff = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()).unwrap_or("");
        let provided = mask_auth_header(auth_header);
        let expected = mask_auth_header(&expected_bearer);
        eprintln!("❌ Auth invalid: provided='{}' expected~='{}' UA='{}' CF-Connecting-IP='{}' X-Forwarded-For='{}'", provided, expected, ua, cfip, xff);
        Err(StatusCode::UNAUTHORIZED)
    } else {
        // Log mínimo en éxito para trazabilidad
        let ua = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("");
        let cfip = headers.get("cf-connecting-ip").and_then(|v| v.to_str().ok()).unwrap_or("");
        let xff = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()).unwrap_or("");
        println!("🔑 Auth header accepted: UA='{}' CF-Connecting-IP='{}' X-Forwarded-For='{}'", ua, cfip, xff);
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
            let ua = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("");
            let cfip = headers.get("cf-connecting-ip").and_then(|v| v.to_str().ok()).unwrap_or("");
            let xff = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()).unwrap_or("");
            println!("❌ Auth FAIL (middleware): {} {} -> {} | UA='{}' CF-IP='{}' XFF='{}'", method, path, status.as_u16(), ua, cfip, xff);
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
        let ua = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("");
        let cfip = headers.get("cf-connecting-ip").and_then(|v| v.to_str().ok()).unwrap_or("");
        let xff = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()).unwrap_or("");
        println!("🚫 Auth FAIL (extractor): {} {} | UA='{}' CF-IP='{}' XFF='{}'", method, path, ua, cfip, xff);
                Err((StatusCode::UNAUTHORIZED, "Unauthorized"))
            }
        }
    }
}
