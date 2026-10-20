//! Helpers shared by the OAuth-based connectors: HTTP client, token files
//! (config directory, mode 0600) and OAuth error parsing.

use std::time::Duration;

use anyhow::{Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::paths;

pub fn http() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .user_agent(concat!("puplify/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

/// Reads `file` from the config directory; `label` names the service in errors.
pub fn load_token<T: DeserializeOwned>(file: &str, label: &str) -> Result<Option<T>> {
    match std::fs::read(paths::config_dir()?.join(file)) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).with_context(|| format!("parsing {label} token"))?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn save_token<T: Serialize>(file: &str, token: &T) -> Result<()> {
    paths::write_atomic(&paths::config_dir()?.join(file), &serde_json::to_vec_pretty(token)?, true)
}

pub fn remove_token(file: &str) -> Result<()> {
    match std::fs::remove_file(paths::config_dir()?.join(file)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

#[derive(Deserialize)]
pub struct OAuthError {
    pub error: String,
    pub error_description: Option<String>,
}

/// Human-readable reason from a failed OAuth response.
pub async fn oauth_error(resp: reqwest::Response) -> String {
    let status = resp.status();
    match resp.json::<OAuthError>().await {
        Ok(e) => e.error_description.unwrap_or(e.error),
        Err(_) => format!("HTTP {status}"),
    }
}

/// `HTTP <status>: <first 300 chars of body>` for a non-success response.
pub async fn http_error(resp: reqwest::Response) -> String {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    format!("HTTP {}: {}", status.as_u16(), body.chars().take(300).collect::<String>())
}

/// Successful response of a token endpoint (authorization-code, device-code and refresh grants).
#[derive(Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: i64,
    /// OpenID Connect identity token, when the `openid` scope was granted.
    pub id_token: Option<String>,
    /// Space-separated scopes actually granted (users can untick some on Google's consent screen).
    pub scope: Option<String>,
}

/// Fresh PKCE code verifier: 64 hex characters from two random UUIDs (244 bits
/// of entropy, within RFC 7636's 43..=128 unreserved characters).
pub fn pkce_verifier() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// S256 code challenge: base64url (no padding) of the SHA-256 of the verifier.
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc7636_appendix_b() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generated_verifier_is_within_rfc_length() {
        let v = pkce_verifier();
        assert!((43..=128).contains(&v.len()));
        assert_ne!(v, pkce_verifier());
    }
}
