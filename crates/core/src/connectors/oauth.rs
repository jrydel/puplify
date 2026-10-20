//! Helpers shared by the OAuth-based connectors: HTTP client, token files
//! (config directory, mode 0600) and OAuth error parsing.

use std::time::Duration;

use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

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
