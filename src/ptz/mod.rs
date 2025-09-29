//! Módulo PTZ para Vigilante.
//!
//! Proporciona una API ligera para control de cámaras PTZ,
//! delegando la lógica pesada a los submódulos en `depends/`.

pub mod depends;

pub use depends::OnvifClient;

use crate::error::VigilanteError;
use crate::{auth::RequireAuth, AppState};
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use std::sync::Arc;

#[derive(serde::Serialize)]
pub struct ApiResponse {
    pub status: String,
    pub message: String,
}

/// Manager principal para PTZ.
#[derive(Clone)]
pub struct PtzManager {
    onvif_url: String,
}

impl PtzManager {
    pub fn new(onvif_url: String) -> Self {
        Self { onvif_url }
    }

    /// Ejecuta un comando PTZ con manejo automático de cliente y perfil
    async fn execute_command<F>(&self, command: F) -> Result<(), VigilanteError>
    where
        F: for<'a> FnOnce(
            &'a OnvifClient,
            &'a str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), VigilanteError>> + Send + 'a>,
        >,
    {
        let client = OnvifClient::from_url(&self.onvif_url)?;
        let (endpoint, token) = client.get_profile_token_with_fallback().await?;

        // Crear cliente con el endpoint correcto
        let final_client =
            OnvifClient::new(endpoint, client.username.clone(), client.password.clone());

        println!("🎯 Ejecutando comando PTZ con ProfileToken: {}", token);
        command(&final_client, &token).await
    }

    pub async fn move_camera(&self, direction: &str) -> Result<(), VigilanteError> {
        match direction {
            "left" => {
                self.execute_command(|client, token| {
                    Box::pin(depends::commands::pan_left(client, token))
                })
                .await
            }
            "right" => {
                self.execute_command(|client, token| {
                    Box::pin(depends::commands::pan_right(client, token))
                })
                .await
            }
            "up" => {
                self.execute_command(|client, token| {
                    Box::pin(depends::commands::tilt_up(client, token))
                })
                .await
            }
            "down" => {
                self.execute_command(|client, token| {
                    Box::pin(depends::commands::tilt_down(client, token))
                })
                .await
            }
            "zoom_in" => {
                self.execute_command(|client, token| {
                    Box::pin(depends::commands::zoom_in(client, token))
                })
                .await
            }
            "zoom_out" => {
                self.execute_command(|client, token| {
                    Box::pin(depends::commands::zoom_out(client, token))
                })
                .await
            }
            "stop" => {
                self.execute_command(|client, token| {
                    Box::pin(depends::commands::stop_all(client, token))
                })
                .await
            }
            _ => Err(VigilanteError::Ptz(format!(
                "Dirección PTZ desconocida: {}",
                direction
            ))),
        }
    }
}

use axum::response::Response;

fn ok() -> Response {
    (
        StatusCode::OK,
        Json(ApiResponse {
            status: "success".into(),
            message: "ok".into(),
        }),
    )
        .into_response()
}

#[axum::debug_handler]
pub async fn pan_left(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, VigilanteError> {
    println!("🎮 API Call: pan_left");
    let manager = PtzManager::new(state.camera_onvif_url().to_string());
    manager.move_camera("left").await?;
    println!("✅ pan_left exitoso");
    Ok(ok())
}

#[axum::debug_handler]
pub async fn pan_right(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, VigilanteError> {
    println!("🎮 API Call: pan_right");
    let manager = PtzManager::new(state.camera_onvif_url().to_string());
    manager.move_camera("right").await?;
    println!("✅ pan_right exitoso");
    Ok(ok())
}

#[axum::debug_handler]
pub async fn tilt_up(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, VigilanteError> {
    println!("🎮 API Call: tilt_up");
    let manager = PtzManager::new(state.camera_onvif_url().to_string());
    manager.move_camera("up").await?;
    println!("✅ tilt_up exitoso");
    Ok(ok())
}

#[axum::debug_handler]
pub async fn tilt_down(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, VigilanteError> {
    println!("🎮 API Call: tilt_down");
    let manager = PtzManager::new(state.camera_onvif_url().to_string());
    manager.move_camera("down").await?;
    println!("✅ tilt_down exitoso");
    Ok(ok())
}

#[axum::debug_handler]
pub async fn zoom_in(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, VigilanteError> {
    println!("🎮 API Call: zoom_in");
    let manager = PtzManager::new(state.camera_onvif_url().to_string());
    manager.move_camera("zoom_in").await?;
    println!("✅ zoom_in exitoso");
    Ok(ok())
}

#[axum::debug_handler]
pub async fn zoom_out(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, VigilanteError> {
    println!("🎮 API Call: zoom_out");
    let manager = PtzManager::new(state.camera_onvif_url().to_string());
    manager.move_camera("zoom_out").await?;
    println!("✅ zoom_out exitoso");
    Ok(ok())
}

#[axum::debug_handler]
pub async fn ptz_stop(
    RequireAuth: RequireAuth,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, VigilanteError> {
    println!("🎮 API Call: ptz_stop");
    let manager = PtzManager::new(state.camera_onvif_url().to_string());
    manager.move_camera("stop").await?;
    println!("✅ ptz_stop exitoso");
    Ok(ok())
}
