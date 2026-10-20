//! Dropbox upload. Sign-in uses OAuth 2 with PKCE and no redirect: the user
//! opens the authorize URL, approves, and pastes the code Dropbox shows. No app
//! secret is needed. Tokens are stored in the config directory with mode 0600.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::oauth::{self, http, http_error, oauth_error, pkce_challenge, pkce_verifier, TokenResponse};
use crate::config::{DropboxAccount, DropboxConfig};
use crate::transcript::Transcript;

const AUTHORIZE_URL: &str = "https://www.dropbox.com/oauth2/authorize";
const TOKEN_URL: &str = "https://api.dropboxapi.com/oauth2/token";
const ACCOUNT_URL: &str = "https://api.dropboxapi.com/2/users/get_current_account";
const UPLOAD_URL: &str = "https://content.dropboxapi.com/2/files/upload";
const TOKEN_FILE: &str = "dropbox-token.json";
/// Dropbox simple uploads are limited to 150 MiB.
const UPLOAD_MAX: usize = 150 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingLogin {
    /// Open this in a browser, approve, and copy the code Dropbox shows.
    pub url: String,
    /// PKCE code verifier; pass it back to `finish_login`.
    pub verifier: String,
}

#[derive(Serialize, Deserialize)]
struct StoredToken {
    app_key: String,
    account: String,
    access_token: String,
    refresh_token: String,
    expires_at: DateTime<Utc>,
}

fn app_key(cfg: &DropboxAccount) -> Result<&str> {
    match cfg.app_key.trim() {
        "" => bail!("Dropbox app key is not set (see README: Dropbox setup)"),
        key => Ok(key),
    }
}

fn load_token() -> Result<Option<StoredToken>> {
    oauth::load_token(TOKEN_FILE, "Dropbox")
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

/// Step 1: build the authorize URL and a fresh PKCE verifier.
pub fn begin_login(cfg: &DropboxAccount) -> Result<PendingLogin> {
    let app_key = app_key(cfg)?;
    let verifier = pkce_verifier();
    let url = format!(
        "{AUTHORIZE_URL}?client_id={}&response_type=code&code_challenge={}&code_challenge_method=S256&token_access_type=offline",
        urlencoding::encode(app_key),
        pkce_challenge(&verifier),
    );
    Ok(PendingLogin { url, verifier })
}

/// Step 2: exchange the pasted code for tokens. Returns the account email.
pub async fn finish_login(cfg: &DropboxAccount, verifier: &str, code: &str) -> Result<String> {
    let client = http()?;
    let app_key = app_key(cfg)?;
    let code = code.trim();
    if code.is_empty() {
        bail!("no authorization code entered");
    }
    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("code_verifier", verifier),
            ("client_id", app_key),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("Dropbox sign-in failed (the code may be wrong or expired): {}", oauth_error(resp).await);
    }
    let token: TokenResponse = resp.json().await?;
    let account = fetch_account(&client, &token.access_token).await?;
    save_token(&StoredToken {
        app_key: app_key.to_string(),
        account: account.clone(),
        expires_at: Utc::now() + chrono::Duration::seconds(token.expires_in),
        refresh_token: token.refresh_token.context("Dropbox returned no refresh token; sign in again")?,
        access_token: token.access_token,
    })?;
    Ok(account)
}

async fn fetch_account(client: &reqwest::Client, access_token: &str) -> Result<String> {
    let resp = client.post(ACCOUNT_URL).bearer_auth(access_token).send().await?;
    if !resp.status().is_success() {
        bail!(
            "reading Dropbox account (does the app have the account_info.read permission?): {}",
            http_error(resp).await
        );
    }
    let account: Value = resp.json().await?;
    Ok(account["email"].as_str().filter(|s| !s.is_empty()).unwrap_or("Dropbox account").to_string())
}

/// Valid access token, refreshing (and persisting it) when needed.
async fn access_token(cfg: &DropboxAccount, client: &reqwest::Client) -> Result<String> {
    let mut token = load_token()?.context("not signed in to Dropbox")?;
    if token.app_key != app_key(cfg)? {
        bail!("Dropbox app key changed since sign-in; sign in again");
    }
    if token.expires_at > Utc::now() + chrono::Duration::seconds(120) {
        return Ok(token.access_token);
    }
    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", &token.app_key),
            ("refresh_token", &token.refresh_token),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("refreshing Dropbox sign-in (sign in again): {}", oauth_error(resp).await);
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

/// Compact JSON with every non-ASCII character as `\uXXXX`, as required for the
/// `Dropbox-API-Arg` header.
fn ascii_json(value: &Value) -> String {
    let mut out = String::new();
    for c in value.to_string().chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

/// `/folder/name`, with exactly one leading slash and no trailing slash on the folder.
fn dropbox_path(folder: &str, name: &str) -> String {
    let folder = folder.trim().trim_matches('/');
    if folder.is_empty() {
        format!("/{name}")
    } else {
        format!("/{folder}/{name}")
    }
}

/// Uploads (overwriting) a file; returns its path in Dropbox.
async fn upload(client: &reqwest::Client, token: &str, path: &str, bytes: Vec<u8>) -> Result<String> {
    if bytes.len() > UPLOAD_MAX {
        bail!("{path} is too large for a Dropbox upload (150 MiB limit)");
    }
    let arg = json!({ "path": path, "mode": "overwrite", "autorename": false, "mute": true });
    let resp = client
        .post(UPLOAD_URL)
        .bearer_auth(token)
        .header("Dropbox-API-Arg", ascii_json(&arg))
        .header("Content-Type", "application/octet-stream")
        .body(bytes)
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("uploading {path}: {}", http_error(resp).await);
    }
    let file: Value = resp.json().await?;
    Ok(file["path_display"].as_str().unwrap_or(path).to_string())
}

/// Uploads the transcript in each configured format; returns the Dropbox paths.
pub async fn export(cfg: &DropboxConfig, account: &DropboxAccount, transcript: &Transcript) -> Result<String> {
    if cfg.formats.is_empty() {
        bail!("no Dropbox formats selected");
    }
    let client = http()?;
    let token = access_token(account, &client).await?;
    let stem = transcript.file_stem();
    let mut paths = Vec::new();
    for &format in &cfg.formats {
        let path = dropbox_path(&cfg.folder, &format!("{stem}.{}", format.extension()));
        paths.push(upload(&client, &token, &path, transcript.render(format).into_bytes()).await?);
    }
    Ok(paths.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_url_carries_challenge_for_generated_verifier() {
        let account = DropboxAccount { app_key: "my key".into() };
        let pending = begin_login(&account).unwrap();
        assert!((43..=128).contains(&pending.verifier.len()));
        assert!(pending.url.starts_with("https://www.dropbox.com/oauth2/authorize?client_id=my%20key&"));
        assert!(pending.url.contains(&format!("code_challenge={}&", pkce_challenge(&pending.verifier))));
        assert!(pending.url.ends_with("&token_access_type=offline"));
    }

    #[test]
    fn begin_login_requires_app_key() {
        let err = begin_login(&DropboxAccount::default()).unwrap_err().to_string();
        assert!(err.contains("app key"), "{err}");
    }

    #[test]
    fn api_arg_header_is_ascii_even_for_astral_characters() {
        let json = ascii_json(&json!({ "path": "/Schůzka 😀.txt" }));
        assert!(json.is_ascii());
        assert_eq!(json, r#"{"path":"/Sch\u016fzka \ud83d\ude00.txt"}"#);
        let back: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(back["path"], "/Schůzka 😀.txt");
    }

    #[test]
    fn folder_paths_are_normalized() {
        assert_eq!(dropbox_path("/Transcripts", "a.txt"), "/Transcripts/a.txt");
        assert_eq!(dropbox_path("Transcripts/", "a.txt"), "/Transcripts/a.txt");
        assert_eq!(dropbox_path("", "a.txt"), "/a.txt");
        assert_eq!(dropbox_path("/", "a.txt"), "/a.txt");
    }
}
