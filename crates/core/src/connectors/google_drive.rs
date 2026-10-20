//! Google Drive upload. Sign-in uses the OAuth 2.0 device flow with the
//! `drive.file` scope, so the app only ever sees files and folders it created
//! itself. Tokens are stored in the config directory with mode 0600.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::oauth::{self, http, http_error, oauth_error, OAuthError};
use crate::config::GoogleDriveConfig;
use crate::transcript::Transcript;

const DEVICE_CODE_URL: &str = "https://oauth2.googleapis.com/device/code";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const SCOPE: &str = "https://www.googleapis.com/auth/drive.file";
const API: &str = "https://www.googleapis.com/drive/v3";
const UPLOAD: &str = "https://www.googleapis.com/upload/drive/v3/files";
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
const TOKEN_FILE: &str = "google-drive-token.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCode {
    pub user_code: String,
    pub verification_uri: String,
    pub message: String,
    pub expires_in: u64,
    pub interval: u64,
    pub device_code: String,
}

/// Google's device-code response; it names the URL `verification_url`.
#[derive(Deserialize)]
struct RawDeviceCode {
    device_code: String,
    user_code: String,
    #[serde(alias = "verification_uri")]
    verification_url: String,
    expires_in: u64,
    #[serde(default)]
    interval: u64,
}

impl From<RawDeviceCode> for DeviceCode {
    fn from(raw: RawDeviceCode) -> Self {
        Self {
            message: format!(
                "To sign in to Google Drive, open {} and enter the code {}",
                raw.verification_url, raw.user_code
            ),
            user_code: raw.user_code,
            verification_uri: raw.verification_url,
            expires_in: raw.expires_in,
            interval: if raw.interval == 0 { 5 } else { raw.interval },
            device_code: raw.device_code,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct StoredToken {
    client_id: String,
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

fn client_id(cfg: &GoogleDriveConfig) -> Result<&str> {
    match cfg.client_id.trim() {
        "" => bail!("Google Drive client ID is not set (see README: Google Drive setup)"),
        id => Ok(id),
    }
}

fn client_secret(cfg: &GoogleDriveConfig) -> Result<&str> {
    match cfg.client_secret.trim() {
        "" => bail!("Google Drive client secret is not set (see README: Google Drive setup)"),
        secret => Ok(secret),
    }
}

fn load_token() -> Result<Option<StoredToken>> {
    oauth::load_token(TOKEN_FILE, "Google Drive")
}

fn save_token(token: &StoredToken) -> Result<()> {
    oauth::save_token(TOKEN_FILE, token)
}

/// Signed-in account, if any.
pub fn status() -> Result<Option<String>> {
    Ok(load_token()?.map(|t| t.account))
}

pub fn logout() -> Result<()> {
    oauth::remove_token(TOKEN_FILE)
}

/// Step 1: obtain a code the user enters at `verification_uri`.
pub async fn begin_login(cfg: &GoogleDriveConfig) -> Result<DeviceCode> {
    let resp = http()?
        .post(DEVICE_CODE_URL)
        .form(&[("client_id", client_id(cfg)?), ("scope", SCOPE)])
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("starting Google sign-in: {}", oauth_error(resp).await);
    }
    let raw: RawDeviceCode = resp.json().await.context("unexpected Google sign-in response")?;
    Ok(raw.into())
}

/// Step 2: poll until the user approves (or the code expires). Returns the account email.
pub async fn finish_login(cfg: &GoogleDriveConfig, code: &DeviceCode) -> Result<String> {
    let client = http()?;
    let client_id = client_id(cfg)?;
    let client_secret = client_secret(cfg)?;
    let mut interval = code.interval.max(1);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(code.expires_in);
    loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        if tokio::time::Instant::now() > deadline {
            bail!("sign-in code expired; start again");
        }
        let resp = client
            .post(TOKEN_URL)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", client_id),
                ("client_secret", client_secret),
                ("device_code", &code.device_code),
            ])
            .send()
            .await?;
        if resp.status().is_success() {
            let token: TokenResponse = resp.json().await?;
            let account = fetch_account(&client, &token.access_token).await?;
            save_token(&StoredToken {
                client_id: client_id.to_string(),
                account: account.clone(),
                expires_at: Utc::now() + chrono::Duration::seconds(token.expires_in),
                refresh_token: token.refresh_token.context("Google returned no refresh token; sign in again")?,
                access_token: token.access_token,
            })?;
            return Ok(account);
        }
        let err: OAuthError = resp.json().await.context("unexpected sign-in response")?;
        match err.error.as_str() {
            "authorization_pending" => {}
            "slow_down" => interval += 5,
            "access_denied" => bail!("sign-in was denied"),
            "expired_token" => bail!("sign-in code expired; start again"),
            _ => bail!("sign-in failed: {}", err.error_description.unwrap_or(err.error)),
        }
    }
}

async fn fetch_account(client: &reqwest::Client, access_token: &str) -> Result<String> {
    let about: Value = client
        .get(format!("{API}/about"))
        .query(&[("fields", "user(emailAddress)")])
        .bearer_auth(access_token)
        .send()
        .await?
        .error_for_status()
        .context("reading Google account (is the Drive API enabled for this project?)")?
        .json()
        .await?;
    Ok(about["user"]["emailAddress"].as_str().filter(|s| !s.is_empty()).unwrap_or("Google account").to_string())
}

/// Valid access token, refreshing (and persisting it) when needed.
async fn access_token(cfg: &GoogleDriveConfig, client: &reqwest::Client) -> Result<String> {
    let mut token = load_token()?.context("not signed in to Google Drive")?;
    if token.client_id != client_id(cfg)? {
        bail!("Google Drive client ID changed since sign-in; sign in again");
    }
    if token.expires_at > Utc::now() + chrono::Duration::seconds(120) {
        return Ok(token.access_token);
    }
    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", &token.client_id),
            ("client_secret", client_secret(cfg)?),
            ("refresh_token", &token.refresh_token),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("refreshing Google Drive sign-in (sign in again): {}", oauth_error(resp).await);
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

/// Escapes a value for a Drive `q` string literal.
fn q_literal(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// First file id matching the query, if any.
async fn find_id(client: &reqwest::Client, token: &str, q: &str) -> Result<Option<String>> {
    let resp = client
        .get(format!("{API}/files"))
        .query(&[("q", q), ("fields", "files(id)"), ("pageSize", "1"), ("spaces", "drive")])
        .bearer_auth(token)
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("searching Google Drive: {}", http_error(resp).await);
    }
    let list: Value = resp.json().await?;
    Ok(list["files"][0]["id"].as_str().map(str::to_string))
}

/// Id of the configured folder, created in My Drive when missing. `None` means the My Drive root.
async fn ensure_folder(client: &reqwest::Client, token: &str, name: &str) -> Result<Option<String>> {
    let name = name.trim().trim_matches('/');
    if name.is_empty() {
        return Ok(None);
    }
    let q = format!(
        "name = '{}' and mimeType = '{FOLDER_MIME}' and trashed = false and 'root' in parents",
        q_literal(name)
    );
    if let Some(id) = find_id(client, token, &q).await? {
        return Ok(Some(id));
    }
    let resp = client
        .post(format!("{API}/files"))
        .query(&[("fields", "id")])
        .bearer_auth(token)
        .json(&json!({ "name": name, "mimeType": FOLDER_MIME }))
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("creating Google Drive folder {name}: {}", http_error(resp).await);
    }
    let created: Value = resp.json().await?;
    Ok(Some(created["id"].as_str().context("no folder id in Google Drive response")?.to_string()))
}

/// `multipart/related` body: JSON metadata followed by the file content.
fn multipart_related(boundary: &str, metadata: &Value, content_type: &str, content: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(content.len() + 512);
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{metadata}\r\n--{boundary}\r\nContent-Type: {content_type}\r\n\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(content);
    body.extend_from_slice(format!("\r\n--{boundary}--").as_bytes());
    body
}

/// Creates the file in `folder`, or updates it when one with the same name already exists there.
/// Returns its web link.
async fn upload(
    client: &reqwest::Client,
    token: &str,
    folder: Option<&str>,
    name: &str,
    content: Vec<u8>,
    content_type: &str,
) -> Result<String> {
    let parent = folder.unwrap_or("root");
    let q = format!("name = '{}' and '{}' in parents and trashed = false", q_literal(name), q_literal(parent));
    let existing = find_id(client, token, &q).await?;

    let boundary = format!("puplify-{}", uuid::Uuid::new_v4().simple());
    let (request, metadata) = match &existing {
        Some(id) => (client.patch(format!("{UPLOAD}/{id}")), json!({ "name": name })),
        None => {
            let mut metadata = json!({ "name": name });
            if let Some(folder) = folder {
                metadata["parents"] = json!([folder]);
            }
            (client.post(UPLOAD), metadata)
        }
    };
    let resp = request
        .query(&[("uploadType", "multipart"), ("fields", "id,webViewLink")])
        .bearer_auth(token)
        .header("Content-Type", format!("multipart/related; boundary={boundary}"))
        .body(multipart_related(&boundary, &metadata, content_type, &content))
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("uploading {name}: {}", http_error(resp).await);
    }
    let file: Value = resp.json().await?;
    Ok(file["webViewLink"].as_str().unwrap_or(name).to_string())
}

/// Uploads the transcript in each configured format; returns the web links.
pub async fn export(cfg: &GoogleDriveConfig, transcript: &Transcript) -> Result<String> {
    if cfg.formats.is_empty() {
        bail!("no Google Drive formats selected");
    }
    let client = http()?;
    let token = access_token(cfg, &client).await?;
    let folder = ensure_folder(&client, &token, &cfg.folder).await?;
    let stem = transcript.file_stem();
    let mut links = Vec::new();
    for &format in &cfg.formats {
        let name = format!("{stem}.{}", format.extension());
        let body = transcript.render(format).into_bytes();
        links.push(upload(&client, &token, folder.as_deref(), &name, body, format.content_type()).await?);
    }
    Ok(links.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_code_normalizes_verification_url() {
        let raw: RawDeviceCode = serde_json::from_str(
            r#"{"device_code":"dc","user_code":"ABCD-EFGH","verification_url":"https://www.google.com/device","expires_in":1800,"interval":5}"#,
        )
        .unwrap();
        let code = DeviceCode::from(raw);
        assert_eq!(code.verification_uri, "https://www.google.com/device");
        assert_eq!(code.interval, 5);
        assert!(code.message.contains("https://www.google.com/device"));
        assert!(code.message.contains("ABCD-EFGH"));
        let json = serde_json::to_value(&code).unwrap();
        assert_eq!(json["verification_uri"], "https://www.google.com/device");
    }

    #[test]
    fn device_code_accepts_standard_field_and_defaults_interval() {
        let raw: RawDeviceCode = serde_json::from_str(
            r#"{"device_code":"dc","user_code":"X","verification_uri":"https://example.com/d","expires_in":60}"#,
        )
        .unwrap();
        let code = DeviceCode::from(raw);
        assert_eq!(code.verification_uri, "https://example.com/d");
        assert_eq!(code.interval, 5);
    }

    #[test]
    fn drive_query_literals_are_escaped() {
        assert_eq!(q_literal(r"Bob's \ notes"), r"Bob\'s \\ notes");
    }

    #[test]
    fn multipart_body_wraps_metadata_and_content() {
        let body = multipart_related("b", &json!({ "name": "a.txt" }), "text/plain", b"hi");
        assert_eq!(
            String::from_utf8(body).unwrap(),
            "--b\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{\"name\":\"a.txt\"}\r\n--b\r\nContent-Type: text/plain\r\n\r\nhi\r\n--b--"
        );
    }
}
