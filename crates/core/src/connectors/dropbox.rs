//! Dropbox upload. Sign-in uses OAuth 2 with PKCE over a loopback redirect on a
//! fixed port (Dropbox requires the exact redirect URI to be registered in the
//! app). No app secret is needed. Tokens are stored in the config directory
//! with mode 0600.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::oauth::{self, authorize, http, http_error, oauth_error, pkce_challenge, pkce_verifier, Loopback, TokenResponse};
use crate::config::{builtin, DropboxConfig};
use crate::transcript::Transcript;

const AUTHORIZE_URL: &str = "https://www.dropbox.com/oauth2/authorize";
const TOKEN_URL: &str = "https://api.dropboxapi.com/oauth2/token";
const ACCOUNT_URL: &str = "https://api.dropboxapi.com/2/users/get_current_account";
const UPLOAD_URL: &str = "https://content.dropboxapi.com/2/files/upload";
const TOKEN_FILE: &str = "dropbox-token.json";
/// Dropbox simple uploads are limited to 150 MiB.
const UPLOAD_MAX: usize = 150 * 1024 * 1024;
/// Dropbox needs the redirect URI registered exactly, so the loopback port is fixed.
const REDIRECT_PORT: u16 = 53682;

#[derive(Serialize, Deserialize)]
struct StoredToken {
    app_key: String,
    account: String,
    access_token: String,
    refresh_token: String,
    expires_at: DateTime<Utc>,
}

/// The app key baked into this build.
fn app_key() -> Result<&'static str> {
    builtin::DROPBOX_APP_KEY.context("Dropbox sign-in isn't available in this build of Puplify")
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

/// Interactive sign-in. Binds the fixed loopback port, hands the authorize URL
/// to `open_url` (which should show it to the user / open a browser), waits up
/// to five minutes for Dropbox's redirect and exchanges the code for tokens.
/// Returns the account email.
pub async fn login(open_url: impl FnOnce(&str) + Send) -> Result<String> {
    let app_key = app_key()?;
    let verifier = pkce_verifier();
    let challenge = pkce_challenge(&verifier);
    let target = Loopback { label: "Dropbox", host: "localhost", port: Some(REDIRECT_PORT), path: "/" };
    let (code, redirect_uri) =
        authorize(target, |redirect_uri, state| authorize_url(app_key, redirect_uri, state, &challenge), open_url).await?;

    let client = http()?;
    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("code_verifier", verifier.as_str()),
            ("client_id", app_key),
            ("redirect_uri", redirect_uri.as_str()),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("Dropbox sign-in failed: {}", oauth_error(resp).await);
    }
    let token: TokenResponse = resp.json().await.context("unexpected Dropbox sign-in response")?;
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

fn authorize_url(app_key: &str, redirect_uri: &str, state: &str, challenge: &str) -> String {
    let params = [
        ("client_id", app_key),
        ("response_type", "code"),
        ("redirect_uri", redirect_uri),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("token_access_type", "offline"),
        ("state", state),
    ];
    let query: Vec<String> = params.iter().map(|(k, v)| format!("{k}={}", urlencoding::encode(v))).collect();
    format!("{AUTHORIZE_URL}?{}", query.join("&"))
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
async fn access_token(client: &reqwest::Client) -> Result<String> {
    let mut token = load_token()?.context("not signed in to Dropbox")?;
    if token.app_key != app_key()? {
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
pub async fn export(cfg: &DropboxConfig, transcript: &Transcript) -> Result<String> {
    if cfg.formats.is_empty() {
        bail!("no Dropbox formats selected");
    }
    let client = http()?;
    let token = access_token(&client).await?;
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
    fn authorize_url_has_required_parameters() {
        let verifier = pkce_verifier();
        let url = authorize_url("my key", "http://localhost:53682/", "st", &pkce_challenge(&verifier));
        assert!(url.starts_with("https://www.dropbox.com/oauth2/authorize?client_id=my%20key&response_type=code&"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A53682%2F&"));
        assert!(url.contains(&format!("code_challenge={}&code_challenge_method=S256&", pkce_challenge(&verifier))));
        assert!(url.ends_with("&token_access_type=offline&state=st"));
    }

    #[tokio::test]
    async fn login_requires_builtin_app_key() {
        // Skipped when this build bakes in an app key: login would then really open a browser.
        if builtin::DROPBOX_APP_KEY.is_some() {
            return;
        }
        let err = login(|_| panic!("must not open a browser")).await.unwrap_err();
        assert!(err.to_string().contains("isn't available in this build of Puplify"), "{err}");
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
