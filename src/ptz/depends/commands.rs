//! Comandos PTZ para ONVIF.
//!
//! Implementa los comandos específicos de movimiento PTZ.

use super::client::OnvifClient;
use super::soap::xml_escape;
use crate::error::VigilanteError;

const NS_PTZ: &str = "http://www.onvif.org/ver20/ptz/wsdl";
const NS_MEDIA: &str = "http://www.onvif.org/ver10/media/wsdl";

/// Ejecuta un comando PTZ usando un cliente y token de perfil
pub async fn execute_ptz_command(
    client: &OnvifClient,
    _profile_token: &str,
    body: String,
) -> Result<(), VigilanteError> {
    client.soap_request(&body).await.map(|_| ())
}

/// Solicita un keyframe inmediato usando SetSynchronizationPoint del servicio Media
pub async fn request_keyframe(
    client: &OnvifClient,
    profile_token: &str,
) -> Result<(), VigilanteError> {
    println!("🎬 ONVIF SetSynchronizationPoint (keyframe request)");

    let body = format!(
        "<SetSynchronizationPoint xmlns=\"{ns}\">\
            <ProfileToken>{token}</ProfileToken>\
        </SetSynchronizationPoint>",
        ns = NS_MEDIA,
        token = xml_escape(profile_token)
    );

    client.soap_request(&body).await.map(|_| ())?;
    println!("✅ Keyframe solicitado vía ONVIF");
    Ok(())
}

/// Comando ContinuousMove para movimiento continuo
pub async fn continuous_move(
    client: &OnvifClient,
    profile_token: &str,
    pan: f32,
    tilt: f32,
    zoom: Option<f32>,
) -> Result<(), VigilanteError> {
    println!(
        "🎮 PTZ ContinuousMove: pan={}, tilt={}, zoom={:?}",
        pan, tilt, zoom
    );

    let vel_pt = if pan != 0.0 || tilt != 0.0 {
        format!("<PanTilt x=\"{:.3}\" y=\"{:.3}\"/>", pan, tilt)
    } else {
        String::new()
    };

    let vel_zoom = if let Some(z) = zoom {
        format!("<Zoom x=\"{:.3}\"/>", z)
    } else {
        String::new()
    };

    let body = format!(
        "<ContinuousMove xmlns=\"{ns}\">\
            <ProfileToken>{token}</ProfileToken>\
            <Velocity>{vel_pt}{vel_zoom}</Velocity>\
            <Timeout>PT1S</Timeout>\
        </ContinuousMove>",
        ns = NS_PTZ,
        token = xml_escape(profile_token),
        vel_pt = vel_pt,
        vel_zoom = vel_zoom
    );

    execute_ptz_command(client, profile_token, body).await?;
    println!("✅ ContinuousMove completado");
    Ok(())
}

/// Comando Stop para detener todos los movimientos
pub async fn stop_all(client: &OnvifClient, profile_token: &str) -> Result<(), VigilanteError> {
    println!("🛑 PTZ Stop");

    let body = format!(
        "<Stop xmlns=\"{ns}\">\
            <ProfileToken>{token}</ProfileToken>\
            <PanTilt>true</PanTilt>\
            <Zoom>true</Zoom>\
        </Stop>",
        ns = NS_PTZ,
        token = xml_escape(profile_token)
    );

    execute_ptz_command(client, profile_token, body).await?;
    println!("✅ Stop completado");
    Ok(())
}

/// Funciones de conveniencia para movimientos específicos
pub async fn pan_left(client: &OnvifClient, profile_token: &str) -> Result<(), VigilanteError> {
    continuous_move(client, profile_token, -0.5, 0.0, None).await
}

pub async fn pan_right(client: &OnvifClient, profile_token: &str) -> Result<(), VigilanteError> {
    continuous_move(client, profile_token, 0.5, 0.0, None).await
}

pub async fn tilt_up(client: &OnvifClient, profile_token: &str) -> Result<(), VigilanteError> {
    continuous_move(client, profile_token, 0.0, 0.5, None).await
}

pub async fn tilt_down(client: &OnvifClient, profile_token: &str) -> Result<(), VigilanteError> {
    continuous_move(client, profile_token, 0.0, -0.5, None).await
}

pub async fn zoom_in(client: &OnvifClient, profile_token: &str) -> Result<(), VigilanteError> {
    continuous_move(client, profile_token, 0.0, 0.0, Some(0.5)).await
}

pub async fn zoom_out(client: &OnvifClient, profile_token: &str) -> Result<(), VigilanteError> {
    continuous_move(client, profile_token, 0.0, 0.0, Some(-0.5)).await
}
