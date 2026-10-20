//! Google account sign-in, shared by Google Drive export and Google Calendar.
//!
//! Google's limited-input device flow does not allow the calendar scope, so
//! this uses the installed-app loopback flow (OAuth 2.0 + PKCE): the system
//! browser opens Google's consent page and Google redirects back to a one-shot
//! HTTP listener on `127.0.0.1` (see `oauth::authorize`). Tokens are stored in
//! the config directory with mode 0600.

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::oauth::{self, authorize, http, oauth_error, pkce_challenge, pkce_verifier, Loopback, TokenResponse};
use crate::config::builtin;

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub(crate) const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const USERINFO_URL: &str = "https://openidconnect.googleapis.com/v1/userinfo";
const SCOPE_DRIVE: &str = "https://www.googleapis.com/auth/drive.file";
const SCOPE_CALENDAR: &str = "https://www.googleapis.com/auth/calendar.readonly";
const TOKEN_FILE: &str = "google-token.json";

#[derive(Serialize, Deserialize)]
struct StoredToken {
    client_id: String,
    account: String,
    access_token: String,
    refresh_token: String,
    expires_at: DateTime<Utc>,
}

/// Client ID + secret baked into this build.
fn credentials() -> Result<(&'static str, &'static str)> {
    builtin::GOOGLE_CREDENTIALS.context("Google sign-in isn't available in this build of Puplify")
}

fn load_token() -> Result<Option<StoredToken>> {
    oauth::load_token(TOKEN_FILE, "Google")
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

/// Interactive sign-in. Binds a loopback listener, hands the consent URL to
/// `open_url` (which should show it to the user / open a browser), waits up to
/// five minutes for Google's redirect and exchanges the code for tokens.
/// Returns the account email.
pub async fn login(open_url: impl FnOnce(&str) + Send) -> Result<String> {
    let (client_id, client_secret) = credentials()?;
    let verifier = pkce_verifier();
    let challenge = pkce_challenge(&verifier);
    let target = Loopback { label: "Google", host: "127.0.0.1", port: None, path: "" };
    let (code, redirect_uri) =
        authorize(target, |redirect_uri, state| auth_url(client_id, redirect_uri, state, &challenge), open_url).await?;

    let client = http()?;
    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("code_verifier", verifier.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("Google sign-in failed: {}", oauth_error(resp).await);
    }
    let token: TokenResponse = resp.json().await.context("unexpected Google sign-in response")?;
    let granted = token.scope.as_deref().unwrap_or_default();
    for (scope, what) in [(SCOPE_DRIVE, "Google Drive"), (SCOPE_CALENDAR, "Google Calendar")] {
        if !granted.split_whitespace().any(|s| s == scope) {
            bail!("{what} access was not granted; sign in again and keep every permission checkbox ticked");
        }
    }
    let account = match token.id_token.as_deref().and_then(email_from_id_token) {
        Some(email) => email,
        None => fetch_email(&client, &token.access_token).await?,
    };
    save_token(&StoredToken {
        client_id: client_id.to_string(),
        account: account.clone(),
        expires_at: Utc::now() + chrono::Duration::seconds(token.expires_in),
        refresh_token: token.refresh_token.context("Google returned no refresh token; sign in again")?,
        access_token: token.access_token,
    })?;
    Ok(account)
}

fn auth_url(client_id: &str, redirect_uri: &str, state: &str, challenge: &str) -> String {
    let scope = format!("openid email {SCOPE_DRIVE} {SCOPE_CALENDAR}");
    let params = [
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("response_type", "code"),
        ("scope", scope.as_str()),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("access_type", "offline"),
        ("prompt", "consent"),
        ("state", state),
    ];
    let query: Vec<String> = params.iter().map(|(k, v)| format!("{k}={}", urlencoding::encode(v))).collect();
    format!("{AUTH_URL}?{}", query.join("&"))
}

/// `email` claim of an OpenID Connect ID token. The token comes straight from
/// Google's token endpoint over TLS, so its signature is not re-verified.
fn email_from_id_token(id_token: &str) -> Option<String> {
    let payload = URL_SAFE_NO_PAD.decode(id_token.split('.').nth(1)?).ok()?;
    let claims: Value = serde_json::from_slice(&payload).ok()?;
    claims["email"].as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

async fn fetch_email(client: &reqwest::Client, access_token: &str) -> Result<String> {
    let info: Value = client
        .get(USERINFO_URL)
        .bearer_auth(access_token)
        .send()
        .await?
        .error_for_status()
        .context("reading Google account")?
        .json()
        .await?;
    Ok(info["email"].as_str().filter(|s| !s.is_empty()).unwrap_or("Google account").to_string())
}

/// Valid access token, refreshing (and persisting it) when needed.
pub(crate) async fn access_token(client: &reqwest::Client) -> Result<String> {
    let mut token = load_token()?.context("not signed in to Google (run `puplify google login` or use Settings → Accounts)")?;
    let (client_id, client_secret) = credentials()?;
    if token.client_id != client_id {
        bail!("Google client ID changed since sign-in; sign in again");
    }
    if token.expires_at > Utc::now() + chrono::Duration::seconds(120) {
        return Ok(token.access_token);
    }
    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", token.client_id.as_str()),
            ("client_secret", client_secret),
            ("refresh_token", token.refresh_token.as_str()),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("refreshing Google sign-in (sign in again): {}", oauth_error(resp).await);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_url_has_required_parameters() {
        let url = auth_url("id 1", "http://127.0.0.1:5555", "st", "chal");
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?client_id=id%201&"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A5555&"));
        assert!(url.contains("response_type=code&"));
        assert!(url.contains(
            "scope=openid%20email%20https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fdrive.file%20https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fcalendar.readonly&"
        ));
        assert!(url.contains("code_challenge=chal&code_challenge_method=S256&"));
        assert!(url.ends_with("access_type=offline&prompt=consent&state=st"));
    }

    #[test]
    fn email_is_read_from_id_token_claims() {
        let payload = URL_SAFE_NO_PAD.encode(r#"{"sub":"1","email":"me@example.com"}"#);
        assert_eq!(email_from_id_token(&format!("h.{payload}.sig")).as_deref(), Some("me@example.com"));
        assert_eq!(email_from_id_token("garbage"), None);
        let no_email = URL_SAFE_NO_PAD.encode(r#"{"sub":"1"}"#);
        assert_eq!(email_from_id_token(&format!("h.{no_email}.sig")), None);
    }

    #[tokio::test]
    async fn login_requires_builtin_credentials() {
        // Skipped when this build bakes in credentials: login would then really open a browser.
        if builtin::GOOGLE_CREDENTIALS.is_some() {
            return;
        }
        let err = login(|_| panic!("must not open a browser")).await.unwrap_err();
        assert!(err.to_string().contains("isn't available in this build of Puplify"), "{err}");
    }
}
