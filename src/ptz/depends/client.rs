//! Cliente ONVIF para control PTZ.
//!
//! Maneja las comunicaciones SOAP con cámaras ONVIF.

use super::soap::build_soap_envelope;
use crate::error::VigilanteError;
use quick_xml::events::Event;
use quick_xml::Reader;
use reqwest;
use url::Url;

const NS_MEDIA: &str = "http://www.onvif.org/ver10/media/wsdl";

/// Cliente ONVIF para control PTZ
pub struct OnvifClient {
    pub endpoint: String,
    pub username: String,
    pub password: String,
}

impl OnvifClient {
    pub fn new(endpoint: String, username: String, password: String) -> Self {
        Self {
            endpoint,
            username,
            password,
        }
    }

    /// Crea un cliente desde una URL ONVIF completa
    pub fn from_url(onvif_url: &str) -> Result<Self, VigilanteError> {
        let url = Url::parse(onvif_url)
            .map_err(|e| VigilanteError::Ptz(format!("Error parseando URL ONVIF: {}", e)))?;
        let username = url.username().to_string();
        let password = url.password().unwrap_or("").to_string();

        if username.is_empty() {
            return Err(VigilanteError::Ptz(
                "El URL ONVIF debe incluir usuario: http://user:pass@host/...".to_string(),
            ));
        }

        let mut endpoint_url = url.clone();
        endpoint_url.set_username("").ok();
        endpoint_url.set_password(None).ok();
        let endpoint = endpoint_url.as_str().to_string();

        Ok(Self::new(endpoint, username, password))
    }

    /// Envía una petición SOAP y retorna la respuesta
    pub async fn soap_request(&self, body_xml: &str) -> Result<String, VigilanteError> {
        let envelope = build_soap_envelope(&self.username, &self.password, body_xml);

        println!("🔄 PTZ ONVIF Request:");
        println!("   Endpoint: {}", self.endpoint);
        println!("   Username: {}", self.username);
        println!(
            "   Password: {}***",
            &self.password[..std::cmp::min(3, self.password.len())]
        );
        println!("   SOAP Body: {}", body_xml);

        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| VigilanteError::Ptz(format!("Error creando cliente HTTP: {}", e)))?;

        let resp = client
            .post(&self.endpoint)
            .header("Content-Type", "application/soap+xml; charset=utf-8")
            .body(envelope)
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .map_err(|e| VigilanteError::Ptz(format!("Error enviando petición ONVIF: {}", e)))?;

        let status = resp.status();
        let headers = resp.headers().clone();
        let text = resp
            .text()
            .await
            .map_err(|e| VigilanteError::Ptz(format!("Error leyendo respuesta: {}", e)))?;

        println!("📡 PTZ ONVIF Response:");
        println!("   Status: {}", status);
        println!("   Headers: {:?}", headers);
        println!("   Body: {}", text);

        if !status.is_success() {
            let error_msg = format!("ONVIF HTTP {}: {}", status, text);
            eprintln!("❌ {}", error_msg);
            return Err(VigilanteError::Ptz(error_msg));
        }

        println!("✅ PTZ Request successful");
        Ok(text)
    }

    /// Obtiene el ProfileToken del primer perfil disponible
    pub async fn get_profile_token(&self) -> Result<String, VigilanteError> {
        println!("🔍 Buscando ProfileToken...");
        let body = format!("<GetProfiles xmlns=\"{}\"/>", NS_MEDIA);
        let xml = self.soap_request(&body).await?;

        println!("🔍 Parseando XML para ProfileToken...");
        let mut reader = Reader::from_str(&xml);
        reader.trim_text(true);
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                    let local = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    if local.ends_with("Profiles") || local.ends_with("Profile") {
                        println!("   ✅ Encontrado elemento de perfil: {}", local);
                        for a in e.attributes().flatten() {
                            let _attr_name = String::from_utf8_lossy(a.key.as_ref());
                            if a.key.as_ref() == b"token" {
                                let v = a
                                    .unescape_value()
                                    .map_err(|e| VigilanteError::Ptz(e.to_string()))?
                                    .to_string();
                                if !v.is_empty() {
                                    println!("   🎯 ProfileToken encontrado: [TOKEN_HIDDEN_FOR_SECURITY]");
                                    return Ok(v);
                                }
                            }
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => {
                    let error_msg = format!("XML parse error: {}", e);
                    eprintln!("❌ {}", error_msg);
                    return Err(VigilanteError::Ptz(error_msg));
                }
                _ => {}
            }
            buf.clear();
        }

        let error_msg = "No se encontró ProfileToken en la respuesta ONVIF".to_string();
        eprintln!("❌ {}", error_msg);
        Err(VigilanteError::Ptz(error_msg))
    }

    /// Intenta obtener ProfileToken con fallback a endpoint alternativo
    pub async fn get_profile_token_with_fallback(
        &self,
    ) -> Result<(String, String), VigilanteError> {
        // Primero intenta con el endpoint actual
        match self.get_profile_token().await {
            Ok(token) => Ok((self.endpoint.clone(), token)),
            Err(e) => {
                println!("⚠️ Fallo en endpoint principal: {}", e);
                println!("🔄 Intentando fallback a endpoint /media...");

                // Fallback: reemplazar último segmento por 'media'
                let mut alt =
                    Url::parse(&self.endpoint).map_err(|e| VigilanteError::Ptz(e.to_string()))?;
                if let Ok(mut segs) = alt.path_segments_mut() {
                    segs.pop_if_empty();
                }
                let mut path = alt.path().trim_end_matches('/').to_string();
                if path.ends_with("/ptz") {
                    path = path.trim_end_matches("ptz").to_string() + "media";
                } else if !path.ends_with("/media") {
                    path += "/media";
                }
                alt.set_path(&path);
                let alt_endpoint = alt.as_str().to_string();

                let alt_client = Self::new(
                    alt_endpoint.clone(),
                    self.username.clone(),
                    self.password.clone(),
                );
                let token = alt_client.get_profile_token().await.map_err(|e| {
                    VigilanteError::Ptz(format!("GetProfiles fallback(media) error: {}", e))
                })?;

                println!(
                    "✅ ProfileToken obtenido desde endpoint /media: [TOKEN_HIDDEN_FOR_SECURITY]"
                );
                Ok((alt_endpoint, token))
            }
        }
    }
}
