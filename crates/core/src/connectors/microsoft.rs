//! Microsoft account sign-in, shared by OneDrive export and the Outlook
//! calendar. Uses the OAuth 2.0 authorization code flow with PKCE over a
//! loopback redirect (public client, no secret) against the `common` tenant,
//! which accepts personal Microsoft accounts and work/school Entra ID accounts.
//! The app registration is built into the binary (`config::builtin`). Tokens are
//! stored in the config directory with mode 0600.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::oauth::{self, authorize, http, oauth_error, pkce_challenge, pkce_verifier, Loopback, TokenResponse};
use crate::config::builtin;

const SCOPES: &str = "Files.ReadWrite Calendars.Read User.Read offline_access";
pub(crate) const GRAPH: &str = "https://graph.microsoft.com/v1.0";
const TOKEN_FILE: &str = "microsoft-token.json";

#[derive(Serialize, Deserialize)]
struct StoredToken {
    client_id: String,
    account: String,
    access_token: String,
    refresh_token: String,
    expires_at: DateTime<Utc>,
}

const AUTHORITY: &str = "https://login.microsoftonline.com/common/oauth2/v2.0";

/// The client ID baked into this build.
fn client_id() -> Result<&'static str> {
    builtin::MICROSOFT_CLIENT_ID.context("Microsoft sign-in isn't available in this build of Puplify")
}

fn load_token() -> Result<Option<StoredToken>> {
    oauth::load_token(TOKEN_FILE, "Microsoft")
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
/// five minutes for Microsoft's redirect and exchanges the code for tokens.
/// Returns the account name.
pub async fn login(open_url: impl FnOnce(&str) + Send) -> Result<String> {
    let client_id = client_id()?;
    let verifier = pkce_verifier();
    let challenge = pkce_challenge(&verifier);
    // Entra accepts any port for a `http://localhost` redirect registered as a desktop app.
    let target = Loopback { label: "Microsoft", host: "localhost", port: None, path: "" };
    let (code, redirect_uri) = authorize(
        target,
        |redirect_uri, state| authorize_url(client_id, redirect_uri, state, &challenge),
        open_url,
    )
    .await?;

    let client = http()?;
    let resp = client
        .post(format!("{AUTHORITY}/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("code_verifier", verifier.as_str()),
            ("scope", SCOPES),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("Microsoft sign-in failed: {}", oauth_error(resp).await);
    }
    let token: TokenResponse = resp.json().await.context("unexpected Microsoft sign-in response")?;
    let account = fetch_account(&client, &token.access_token).await?;
    save_token(&StoredToken {
        client_id: client_id.to_string(),
        account: account.clone(),
        expires_at: Utc::now() + chrono::Duration::seconds(token.expires_in),
        refresh_token: token.refresh_token.context("no refresh token returned; is offline_access allowed?")?,
        access_token: token.access_token,
    })?;
    Ok(account)
}

fn authorize_url(client_id: &str, redirect_uri: &str, state: &str, challenge: &str) -> String {
    let params = [
        ("client_id", client_id),
        ("response_type", "code"),
        ("redirect_uri", redirect_uri),
        ("response_mode", "query"),
        ("scope", SCOPES),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
        ("prompt", "select_account"),
    ];
    let query: Vec<String> = params.iter().map(|(k, v)| format!("{k}={}", urlencoding::encode(v))).collect();
    format!("{AUTHORITY}/authorize?{}", query.join("&"))
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

/// Valid access token, refreshing (and persisting the rotated refresh token) when needed.
pub(crate) async fn access_token(client: &reqwest::Client) -> Result<String> {
    let mut token = load_token()?.context("not signed in to Microsoft (run `puplify microsoft login` or use Settings → Accounts)")?;
    if token.client_id != client_id()? {
        bail!("Microsoft client ID changed since sign-in; sign in again");
    }
    if token.expires_at > Utc::now() + chrono::Duration::seconds(120) {
        return Ok(token.access_token);
    }
    let resp = client
        .post(format!("{AUTHORITY}/token"))
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", &token.client_id),
            ("refresh_token", &token.refresh_token),
            ("scope", SCOPES),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("refreshing Microsoft sign-in (sign in again): {}", oauth_error(resp).await);
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
    fn authorize_url_has_required_parameters() {
        let verifier = pkce_verifier();
        let url = authorize_url("id 1", "http://localhost:5555", "st", &pkce_challenge(&verifier));
        assert!(url.starts_with(
            "https://login.microsoftonline.com/common/oauth2/v2.0/authorize?client_id=id%201&response_type=code&"
        ));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A5555&response_mode=query&"));
        assert!(url.contains("scope=Files.ReadWrite%20Calendars.Read%20User.Read%20offline_access&"));
        assert!(url.contains(&format!("code_challenge={}&code_challenge_method=S256&", pkce_challenge(&verifier))));
        assert!(url.ends_with("state=st&prompt=select_account"));
    }

    #[tokio::test]
    async fn login_requires_builtin_client_id() {
        // Skipped when this build bakes in a client ID: login would then really open a browser.
        if builtin::MICROSOFT_CLIENT_ID.is_some() {
            return;
        }
        let err = login(|_| panic!("must not open a browser")).await.unwrap_err();
        assert!(err.to_string().contains("isn't available in this build of Puplify"), "{err}");
    }
}
