//! Construcción de envelopes SOAP para ONVIF.
//!
//! Maneja la creación de mensajes SOAP con autenticación WS-Security.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use chrono::Utc;
use rand::RngCore;
use sha1::{Digest, Sha1};

const SOAP_ENV: &str = "http://www.w3.org/2003/05/soap-envelope"; // SOAP 1.2
const NS_WSSE: &str =
    "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd";
const NS_WSU: &str =
    "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd";
const NS_WSU_PWD_DIGEST: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordDigest";
const NS_WSSE_NONCE_ENCODING: &str = "http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-soap-message-security-1.0#Base64Binary";

pub fn build_soap_envelope(username: &str, password: &str, inner_body: &str) -> String {
    // Build WS-Security UsernameToken with PasswordDigest as per ONVIF requirements
    let mut nonce_bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce_b64 = BASE64.encode(nonce_bytes);
    let created = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut sha = Sha1::new();
    sha.update(nonce_bytes);
    sha.update(created.as_bytes());
    sha.update(password.as_bytes());
    let digest = sha.finalize();
    let digest_b64 = BASE64.encode(digest);

    format!(
        r#"<s:Envelope xmlns:s="{soap}" xmlns:wsse="{wsse}" xmlns:wsu="{wsu}">
    <s:Header>
        <wsse:Security s:mustUnderstand="1">
            <wsse:UsernameToken>
                <wsse:Username>{user}</wsse:Username>
                <wsse:Password Type="{pwd_type}">{pwd}</wsse:Password>
                <wsse:Nonce EncodingType="{nonce_enc}">{nonce}</wsse:Nonce>
                <wsu:Created>{created}</wsu:Created>
            </wsse:UsernameToken>
        </wsse:Security>
    </s:Header>
    <s:Body>
        {inner}
    </s:Body>
</s:Envelope>"#,
        soap = SOAP_ENV,
        wsse = NS_WSSE,
        wsu = NS_WSU,
        pwd_type = NS_WSU_PWD_DIGEST,
        pwd = digest_b64,
        nonce_enc = NS_WSSE_NONCE_ENCODING,
        nonce = nonce_b64,
        user = xml_escape(username),
        created = created,
        inner = inner_body
    )
}

pub fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}