//! Google account sign-in, shared by Google Drive export and Google Calendar.
//!
//! Google's limited-input device flow does not allow the calendar scope, so
//! this uses the installed-app loopback flow (OAuth 2.0 + PKCE): the system
//! browser opens Google's consent page and Google redirects back to a one-shot
//! HTTP listener on `127.0.0.1`. Tokens are stored in the config directory with
//! mode 0600.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::oauth::{self, http, oauth_error, pkce_challenge, pkce_verifier, TokenResponse};
use crate::config::GoogleAccount;

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub(crate) const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const USERINFO_URL: &str = "https://openidconnect.googleapis.com/v1/userinfo";
const SCOPE_DRIVE: &str = "https://www.googleapis.com/auth/drive.file";
const SCOPE_CALENDAR: &str = "https://www.googleapis.com/auth/calendar.readonly";
const TOKEN_FILE: &str = "google-token.json";
/// How long the user has to finish the consent screen.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
/// A browser connection that sends nothing (speculative preconnect) is dropped after this.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Serialize, Deserialize)]
struct StoredToken {
    client_id: String,
    account: String,
    access_token: String,
    refresh_token: String,
    expires_at: DateTime<Utc>,
}

fn client_id(cfg: &GoogleAccount) -> Result<&str> {
    match cfg.client_id.trim() {
        "" => bail!("Google client ID is not set (see README: Google setup)"),
        id => Ok(id),
    }
}

fn client_secret(cfg: &GoogleAccount) -> Result<&str> {
    match cfg.client_secret.trim() {
        "" => bail!("Google client secret is not set (see README: Google setup)"),
        secret => Ok(secret),
    }
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
pub async fn login(cfg: &GoogleAccount, open_url: impl FnOnce(&str) + Send) -> Result<String> {
    let client_id = client_id(cfg)?;
    let client_secret = client_secret(cfg)?;
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .context("opening a local port for the Google sign-in redirect")?;
    let redirect_uri = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
    let verifier = pkce_verifier();
    let state = uuid::Uuid::new_v4().simple().to_string();
    open_url(&auth_url(client_id, &redirect_uri, &state, &pkce_challenge(&verifier)));

    let code = tokio::time::timeout(LOGIN_TIMEOUT, wait_for_code(&listener, &state))
        .await
        .map_err(|_| anyhow!("Google sign-in timed out after 5 minutes; start again"))??;
    drop(listener);

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

/// Serves the loopback listener until Google's redirect arrives and returns
/// the authorization code. Requests without `code`/`error` (favicon, stray
/// probes) are answered with 404 and ignored; a redirect with the wrong `state`
/// or an `error` ends the sign-in.
async fn wait_for_code(listener: &TcpListener, state: &str) -> Result<String> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let Some(line) = read_request_line(&mut stream).await else { continue };
        let Some(params) = parse_callback(&line) else {
            respond(&mut stream, 404, "Not found").await;
            continue;
        };
        let outcome = callback_outcome(&params, state);
        match &outcome {
            Ok(_) => respond(&mut stream, 200, "You can close this tab and return to Puplify.").await,
            Err(e) => respond(&mut stream, 400, &format!("Google sign-in failed: {e}")).await,
        }
        return outcome;
    }
}

fn callback_outcome(params: &HashMap<String, String>, state: &str) -> Result<String> {
    if params.get("state").map(String::as_str) != Some(state) {
        bail!("the redirect did not match this sign-in attempt (state mismatch)");
    }
    if let Some(error) = params.get("error") {
        match error.as_str() {
            "access_denied" => bail!("access was denied"),
            other => bail!("{other}"),
        }
    }
    params.get("code").cloned().filter(|c| !c.is_empty()).context("the redirect carried no authorization code")
}

/// First line of the HTTP request, or `None` when nothing usable arrived.
async fn read_request_line(stream: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(end) = buf.windows(2).position(|w| w == b"\r\n") {
            return Some(String::from_utf8_lossy(&buf[..end]).into_owned());
        }
        if buf.len() > 8192 {
            return None;
        }
        match tokio::time::timeout(READ_TIMEOUT, stream.read(&mut chunk)).await {
            Ok(Ok(n)) if n > 0 => buf.extend_from_slice(&chunk[..n]),
            _ => return None,
        }
    }
}

/// Query parameters of `GET /?… HTTP/1.1` when it carries `code` or `error`.
fn parse_callback(request_line: &str) -> Option<HashMap<String, String>> {
    let mut parts = request_line.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let (_, query) = parts.next()?.split_once('?')?;
    let params: HashMap<String, String> = query
        .split('&')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            Some((decode(k), decode(v)))
        })
        .collect();
    (params.contains_key("code") || params.contains_key("error")).then_some(params)
}

fn decode(s: &str) -> String {
    let s = s.replace('+', " ");
    urlencoding::decode(&s).map(|c| c.into_owned()).unwrap_or(s)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Best-effort reply: the outcome is already decided, so write errors are ignored.
async fn respond(stream: &mut TcpStream, status: u16, message: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Puplify</title></head>\
         <body style=\"font-family:system-ui,sans-serif;max-width:32rem;margin:4rem auto;padding:0 1rem\">\
         <h1>Puplify</h1><p>{}</p></body></html>",
        html_escape(message)
    );
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Valid access token, refreshing (and persisting it) when needed.
pub(crate) async fn access_token(cfg: &GoogleAccount, client: &reqwest::Client) -> Result<String> {
    let mut token = load_token()?.context("not signed in to Google (run `puplify google login` or use Settings → Accounts)")?;
    if token.client_id != client_id(cfg)? {
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
            ("client_secret", client_secret(cfg)?),
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

    /// Sends a raw request to the listener and returns the full response text.
    async fn request(port: u16, target: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream
            .write_all(format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut out = String::new();
        stream.read_to_string(&mut out).await.unwrap();
        out
    }

    async fn bind_local() -> (TcpListener, u16) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    }

    #[tokio::test]
    async fn loopback_captures_code_for_matching_state() {
        let (listener, port) = bind_local().await;
        let server = tokio::spawn(async move { wait_for_code(&listener, "s3cret").await });
        // A favicon probe is ignored and does not end the sign-in.
        assert!(request(port, "/favicon.ico").await.starts_with("HTTP/1.1 404"));
        let page = request(port, "/?state=s3cret&code=4%2F0Abc-d&scope=email").await;
        assert!(page.starts_with("HTTP/1.1 200"), "{page}");
        assert!(page.contains("You can close this tab and return to Puplify."));
        assert_eq!(server.await.unwrap().unwrap(), "4/0Abc-d");
    }

    #[tokio::test]
    async fn loopback_rejects_wrong_state() {
        let (listener, port) = bind_local().await;
        let server = tokio::spawn(async move { wait_for_code(&listener, "right").await });
        let page = request(port, "/?state=wrong&code=abc").await;
        assert!(page.starts_with("HTTP/1.1 400"), "{page}");
        assert!(page.contains("state mismatch"));
        let err = server.await.unwrap().unwrap_err().to_string();
        assert!(err.contains("state mismatch"), "{err}");
    }

    #[tokio::test]
    async fn loopback_reports_denied_consent_and_escapes_html() {
        let (listener, port) = bind_local().await;
        let server = tokio::spawn(async move { wait_for_code(&listener, "st").await });
        let page = request(port, "/?state=st&error=access_denied").await;
        assert!(page.starts_with("HTTP/1.1 400"), "{page}");
        assert!(page.contains("access was denied"));
        assert!(server.await.unwrap().unwrap_err().to_string().contains("denied"));

        let (listener, port) = bind_local().await;
        let server = tokio::spawn(async move { wait_for_code(&listener, "st").await });
        let page = request(port, "/?state=st&error=%3Cscript%3E").await;
        assert!(page.contains("&lt;script&gt;") && !page.contains("<script>"), "{page}");
        assert!(server.await.unwrap().is_err());
    }

    #[test]
    fn callback_parsing() {
        let p = parse_callback("GET /?code=a%2Fb&state=x+y HTTP/1.1").unwrap();
        assert_eq!(p["code"], "a/b");
        assert_eq!(p["state"], "x y");
        assert!(parse_callback("GET /favicon.ico HTTP/1.1").is_none());
        assert!(parse_callback("GET /?foo=bar HTTP/1.1").is_none());
        assert!(parse_callback("POST /?code=a HTTP/1.1").is_none());
    }

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
    async fn login_requires_client_credentials() {
        let err = login(&GoogleAccount::default(), |_| panic!("must not open a browser")).await.unwrap_err();
        assert!(err.to_string().contains("client ID"), "{err}");
        let cfg = GoogleAccount { client_id: "id".into(), ..Default::default() };
        let err = login(&cfg, |_| panic!("must not open a browser")).await.unwrap_err();
        assert!(err.to_string().contains("client secret"), "{err}");
    }
}
