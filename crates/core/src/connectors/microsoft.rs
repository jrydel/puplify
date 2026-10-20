//! Microsoft account sign-in, shared by OneDrive export and the Outlook
//! calendar. Uses the OAuth 2.0 device code flow against the configured tenant
//! (`common` accepts personal Microsoft accounts and work/school Entra ID
//! accounts). Tokens are stored in the config directory with mode 0600.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::oauth::{self, http, oauth_error, OAuthError, TokenResponse};
use crate::config::MicrosoftAccount;

const SCOPES: &str = "Files.ReadWrite Calendars.Read User.Read offline_access";
pub(crate) const GRAPH: &str = "https://graph.microsoft.com/v1.0";
const TOKEN_FILE: &str = "microsoft-token.json";

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

fn authority(cfg: &MicrosoftAccount) -> String {
    let tenant = match cfg.tenant.trim() {
        "" => "common",
        t => t,
    };
    format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0")
}

fn client_id(cfg: &MicrosoftAccount) -> Result<&str> {
    match cfg.client_id.trim() {
        "" => bail!("Microsoft client ID is not set (see README: Microsoft setup)"),
        id => Ok(id),
    }
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

/// Step 1: obtain a code the user enters at `verification_uri`.
pub async fn begin_login(cfg: &MicrosoftAccount) -> Result<DeviceCode> {
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
pub async fn finish_login(cfg: &MicrosoftAccount, code: &DeviceCode) -> Result<String> {
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

/// Valid access token, refreshing (and persisting the rotated refresh token) when needed.
pub(crate) async fn access_token(cfg: &MicrosoftAccount, client: &reqwest::Client) -> Result<String> {
    let mut token = load_token()?.context("not signed in to Microsoft (run `puplify microsoft login` or use Settings → Accounts)")?;
    if token.client_id != client_id(cfg)? {
        bail!("Microsoft client ID changed since sign-in; sign in again");
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
    fn authority_defaults_to_common() {
        let mut cfg = MicrosoftAccount::default();
        assert_eq!(authority(&cfg), "https://login.microsoftonline.com/common/oauth2/v2.0");
        cfg.tenant = " ".into();
        assert_eq!(authority(&cfg), "https://login.microsoftonline.com/common/oauth2/v2.0");
        cfg.tenant = "contoso.onmicrosoft.com".into();
        assert!(authority(&cfg).contains("/contoso.onmicrosoft.com/"));
    }

    #[test]
    fn begin_login_requires_client_id() {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let err = rt.block_on(begin_login(&MicrosoftAccount::default())).unwrap_err().to_string();
        assert!(err.contains("client ID"), "{err}");
    }
}
