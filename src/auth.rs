use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::body::Body;
use axum::middleware::Next;
use axum::response::Response;
use std::sync::Arc;
use crate::AppState;
use axum::extract::{FromRef, FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::Uri;
use url;

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
        Some(value) => value.to_str().unwrap_or("").trim(),
        None => {
            let ua = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("");
            let cfip = headers.get("cf-connecting-ip").and_then(|v| v.to_str().ok()).unwrap_or("");
            let xff = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()).unwrap_or("");
            let expected_bearer_masked = mask_auth_header(&format!("Bearer {}", token));
            let expected_raw_masked = mask_token(token);
            eprintln!(
                "❌ Falta encabezado Authorization | header_esperado_bearer='{}' header_esperado_raw='{}' | UA='{}' CF-IP='{}' XFF='{}'",
                expected_bearer_masked, expected_raw_masked, ua, cfip, xff
            );
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
        let expected_b = mask_auth_header(&expected_bearer);
        let expected_r = mask_token(token);
        let scheme = if auth_header.starts_with("Bearer ") { "Bearer" } else { "Raw/Other" };
        eprintln!(
            "❌ Authorization inválido: esquema={} | header_proporcionado='{}' header_esperado_bearer='{}' header_esperado_raw='{}' | UA='{}' CF-IP='{}' XFF='{}'",
            scheme, provided, expected_b, expected_r, ua, cfip, xff
        );
        Err(StatusCode::UNAUTHORIZED)
    } else {
        // Log detallado en éxito para validar que coincide con .env
        let ua = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("");
        let cfip = headers.get("cf-connecting-ip").and_then(|v| v.to_str().ok()).unwrap_or("");
        let xff = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()).unwrap_or("");
        let provided = mask_auth_header(auth_header);
        let expected_b = mask_auth_header(&expected_bearer);
        let expected_r = mask_token(token);
        let matched = if auth_header == expected_bearer { "Bearer" } else { "Raw" };
        println!(
            "🔑 Authorization OK: coincide={} | header_proporcionado='{}' header_esperado_bearer='{}' header_esperado_raw='{}' | UA='{}' CF-IP='{}' XFF='{}'",
            matched, provided, expected_b, expected_r, ua, cfip, xff
        );
        Ok(())
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

        // Igual que el middleware: aceptar token en query para rutas de stream si está habilitado
        let is_stream_route = path.starts_with("/api/live/") || path.starts_with("/hls") || path.starts_with("/webrtc/");
        if app_state.allow_query_token_streams && is_stream_route {
            // 1) Intentar con query propia
            if let Some(q) = parts.uri.query() {
                let tok_opt = url::form_urlencoded::parse(q.as_bytes())
                    .find(|(k, _)| k == "token")
                    .map(|(_, v)| v.into_owned());
                if let Some(tok) = tok_opt {
                    if tok == app_state.proxy_token {
                        println!("🔐 Auth OK (extractor query token): {} {}", method, path);
                        return Ok(RequireAuth);
                    } else {
                        println!("🚫 Token query inválido (extractor) en {} {}", method, path);
                    }
                }
            }
            // 2) Intentar con Referer
            if let Some(referer) = parts.headers.get(header::REFERER).and_then(|v| v.to_str().ok()) {
                if let Ok(url) = url::Url::parse(referer) {
                    if let Some(q) = url.query() {
                        let tok_opt = url::form_urlencoded::parse(q.as_bytes())
                            .find(|(k, _)| k == "token")
                            .map(|(_, v)| v.into_owned());
                        if let Some(tok) = tok_opt {
                            if tok == app_state.proxy_token {
                                println!("🔐 Auth OK (extractor referer token): {} {} <- {}", method, path, referer);
                                return Ok(RequireAuth);
                            }
                        }
                    }
                }
            }
        }

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

/// Middleware que valida token: si NO hay header Authorization, requiere token por query ?token=
/// Si hay header Authorization, lo valida normalmente
pub async fn flexible_auth_middleware(
    State(state): State<Arc<AppState>>,
    uri: Uri,
    headers: HeaderMap,
    request: Request<Body>,
    next: Next,
) -> Response {
    // Verificar si hay header Authorization
    let has_auth_header = headers.get(header::AUTHORIZATION).is_some();

    if has_auth_header {
        // Si hay header, validarlo normalmente
        if check_auth(&headers, &state.proxy_token).await.is_ok() {
            println!("🔐 Auth OK (header): {}", uri.path());
            return next.run(request).await;
        } else {
            println!("🚫 Header Authorization inválido: {}", uri.path());
        }
    } else {
        // Si NO hay header, verificar token en query
        if let Some(query) = uri.query() {
            let tok_opt = url::form_urlencoded::parse(query.as_bytes())
                .find(|(k, _)| k == "token")
                .map(|(_, v)| v.into_owned());
            if let Some(tok) = tok_opt {
                if tok == state.proxy_token {
                    println!("🔐 Auth OK (query token - no header): {}", uri.path());
                    return next.run(request).await;
                } else {
                    println!("🚫 Query token inválido: {}", uri.path());
                }
            } else {
                println!("🚫 No hay token en query: {}", uri.path());
            }
        } else {
            println!("🚫 No hay header ni query token: {}", uri.path());
        }
    }

    // Si llegamos aquí, falló la autenticación
    let ua = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("");
    let cfip = headers.get("cf-connecting-ip").and_then(|v| v.to_str().ok()).unwrap_or("");
    let xff = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()).unwrap_or("");
    println!("🚫 Auth FAIL (flexible): {} | UA='{}' CF-IP='{}' XFF='{}'", uri.path(), ua, cfip, xff);

    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .body(Body::from("Unauthorized"))
        .unwrap()
}
