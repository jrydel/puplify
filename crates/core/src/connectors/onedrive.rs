//! OneDrive upload through Microsoft Graph. Sign-in uses the OAuth 2.0 device
//! code flow against the `common` endpoint, which accepts personal Microsoft
//! accounts and work/school (Entra ID) accounts. Tokens are stored in the
//! config directory with mode 0600.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::OneDriveConfig;
use crate::paths;
use crate::transcript::Transcript;

const SCOPES: &str = "Files.ReadWrite offline_access User.Read";
const GRAPH: &str = "https://graph.microsoft.com/v1.0";
/// Graph simple upload limit; larger files use an upload session.
const SIMPLE_UPLOAD_MAX: usize = 4 * 1024 * 1024;
/// Upload-session chunks must be multiples of 320 KiB.
const CHUNK: usize = 320 * 1024 * 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCode {
    pub user_code: String,
    pub verification_uri: String,
    pub message: String,
    pub expires_in: u64,
    pub interval: u64,
    pub device_code: String,
}

#[derive(Serialize, Deserialize)]
struct StoredToken {
    client_id: String,
    tenant: String,
    account: String,
    access_token: String,
    refresh_token: String,
    expires_at: DateTime<Utc>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: i64,
}

#[derive(Deserialize)]
struct OAuthError {
    error: String,
    error_description: Option<String>,
}

fn token_path() -> Result<std::path::PathBuf> {
    Ok(paths::config_dir()?.join("onedrive-token.json"))
}

fn authority(cfg: &OneDriveConfig) -> String {
    let tenant = match cfg.tenant.trim() {
        "" => "common",
        t => t,
    };
    format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0")
}

fn client_id(cfg: &OneDriveConfig) -> Result<&str> {
    match cfg.client_id.trim() {
        "" => bail!("OneDrive client ID is not set (see README: registering the Microsoft app)"),
        id => Ok(id),
    }
}

fn http() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .user_agent(concat!("verbatim/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

fn load_token() -> Result<Option<StoredToken>> {
    match std::fs::read(token_path()?) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).context("parsing OneDrive token")?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn save_token(token: &StoredToken) -> Result<()> {
    paths::write_atomic(&token_path()?, &serde_json::to_vec_pretty(token)?, true)
}

/// Signed-in account, if any.
pub fn status() -> Result<Option<String>> {
    Ok(load_token()?.map(|t| t.account))
}

pub fn logout() -> Result<()> {
    match std::fs::remove_file(token_path()?) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Step 1: obtain a code the user enters at `verification_uri`.
pub async fn begin_login(cfg: &OneDriveConfig) -> Result<DeviceCode> {
    let resp = http()?
        .post(format!("{}/devicecode", authority(cfg)))
        .form(&[("client_id", client_id(cfg)?), ("scope", SCOPES)])
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("starting Microsoft sign-in: {}", oauth_error(resp).await);
    }
    Ok(resp.json().await?)
}

/// Step 2: poll until the user approves (or the code expires). Returns the account name.
pub async fn finish_login(cfg: &OneDriveConfig, code: &DeviceCode) -> Result<String> {
    let client = http()?;
    let client_id = client_id(cfg)?;
    let mut interval = code.interval.max(1);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(code.expires_in);
    loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        if tokio::time::Instant::now() > deadline {
            bail!("sign-in code expired; start again");
        }
        let resp = client
            .post(format!("{}/token", authority(cfg)))
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", client_id),
                ("device_code", &code.device_code),
            ])
            .send()
            .await?;
        if resp.status().is_success() {
            let token: TokenResponse = resp.json().await?;
            let account = fetch_account(&client, &token.access_token).await?;
            save_token(&StoredToken {
                client_id: client_id.to_string(),
                tenant: cfg.tenant.clone(),
                account: account.clone(),
                expires_at: Utc::now() + chrono::Duration::seconds(token.expires_in),
                refresh_token: token.refresh_token.context("no refresh token returned; is offline_access allowed?")?,
                access_token: token.access_token,
            })?;
            return Ok(account);
        }
        let err: OAuthError = resp.json().await.context("unexpected sign-in response")?;
        match err.error.as_str() {
            "authorization_pending" => {}
            "slow_down" => interval += 5,
            _ => bail!("sign-in failed: {}", err.error_description.unwrap_or(err.error)),
        }
    }
}

async fn fetch_account(client: &reqwest::Client, access_token: &str) -> Result<String> {
    let me: Value = client
        .get(format!("{GRAPH}/me?$select=displayName,mail,userPrincipalName"))
        .bearer_auth(access_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(["mail", "userPrincipalName", "displayName"]
        .iter()
        .find_map(|k| me[*k].as_str().filter(|s| !s.is_empty()))
        .unwrap_or("Microsoft account")
        .to_string())
}

async fn oauth_error(resp: reqwest::Response) -> String {
    let status = resp.status();
    match resp.json::<OAuthError>().await {
        Ok(e) => e.error_description.unwrap_or(e.error),
        Err(_) => format!("HTTP {status}"),
    }
}

/// Valid access token, refreshing (and persisting the rotated refresh token) when needed.
async fn access_token(cfg: &OneDriveConfig, client: &reqwest::Client) -> Result<String> {
    let mut token = load_token()?.context("not signed in to OneDrive")?;
    if token.client_id != client_id(cfg)? {
        bail!("OneDrive client ID changed since sign-in; sign in again");
    }
    if token.expires_at > Utc::now() + chrono::Duration::seconds(120) {
        return Ok(token.access_token);
    }
    let resp = client
        .post(format!("{}/token", authority(cfg)))
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", &token.client_id),
            ("refresh_token", &token.refresh_token),
            ("scope", SCOPES),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("refreshing OneDrive sign-in (sign in again): {}", oauth_error(resp).await);
    }
    let fresh: TokenResponse = resp.json().await?;
    token.access_token = fresh.access_token;
    if let Some(refresh) = fresh.refresh_token {
        token.refresh_token = refresh;
    }
    token.expires_at = Utc::now() + chrono::Duration::seconds(fresh.expires_in);
    save_token(&token)?;
    Ok(token.access_token)
}

/// Drive-relative path → URL path segment with each component percent-encoded.
fn encode_path(path: &str) -> String {
    path.split('/')
        .filter(|c| !c.is_empty())
        .map(|c| urlencoding::encode(c).into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Uploads (replacing) a file; returns its web URL.
async fn upload(client: &reqwest::Client, token: &str, path: &str, bytes: Vec<u8>, content_type: &str) -> Result<String> {
    let item = format!("{GRAPH}/me/drive/root:/{}:", encode_path(path));
    let resp = if bytes.len() <= SIMPLE_UPLOAD_MAX {
        client
            .put(format!("{item}/content?@microsoft.graph.conflictBehavior=replace"))
            .bearer_auth(token)
            .header("Content-Type", content_type)
            .body(bytes)
            .send()
            .await?
    } else {
        let session: Value = client
            .post(format!("{item}/createUploadSession"))
            .bearer_auth(token)
            .json(&serde_json::json!({ "item": { "@microsoft.graph.conflictBehavior": "replace" } }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let url = session["uploadUrl"].as_str().context("no uploadUrl in upload session")?.to_string();
        let total = bytes.len();
        let mut last = None;
        for (i, chunk) in bytes.chunks(CHUNK).enumerate() {
            let start = i * CHUNK;
            let end = start + chunk.len() - 1;
            // The upload URL is pre-authenticated; no bearer token.
            last = Some(
                client
                    .put(&url)
                    .header("Content-Range", format!("bytes {start}-{end}/{total}"))
                    .body(chunk.to_vec())
                    .send()
                    .await?,
            );
            if let Some(resp) = &last {
                if !resp.status().is_success() {
                    break;
                }
            }
        }
        last.context("empty upload")?
    };
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("uploading {path}: HTTP {}: {}", status.as_u16(), body.chars().take(300).collect::<String>()));
    }
    let item: Value = resp.json().await?;
    Ok(item["webUrl"].as_str().unwrap_or(path).to_string())
}

/// Uploads the transcript in each configured format; returns the web URLs.
pub async fn export(cfg: &OneDriveConfig, transcript: &Transcript) -> Result<String> {
    if cfg.formats.is_empty() {
        bail!("no OneDrive formats selected");
    }
    let client = http()?;
    let token = access_token(cfg, &client).await?;
    let folder = cfg.folder.trim().trim_matches('/');
    let stem = transcript.file_stem();
    let mut urls = Vec::new();
    for &format in &cfg.formats {
        let name = format!("{stem}.{}", format.extension());
        let path = if folder.is_empty() { name } else { format!("{folder}/{name}") };
        let body = transcript.render(format).into_bytes();
        urls.push(upload(&client, &token, &path, body, format.content_type()).await?);
    }
    Ok(urls.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_paths_are_encoded_per_component() {
        assert_eq!(
            encode_path("/Meetings/2026/2026-10-20 1405 Q3 plan & budget.txt"),
            "Meetings/2026/2026-10-20%201405%20Q3%20plan%20%26%20budget.txt"
        );
    }
}
