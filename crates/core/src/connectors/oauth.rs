//! Helpers shared by the OAuth-based connectors: HTTP client, token files
//! (config directory, mode 0600), OAuth error parsing, PKCE and the loopback
//! redirect server used by the browser sign-in flows.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::paths;

/// How long the user has to finish the consent screen.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
/// A browser connection that sends nothing (speculative preconnect) is dropped after this.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

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

/// Where the local redirect listener lives. The listener always binds
/// `127.0.0.1`; `host` is only the name used in the redirect URI the provider sees.
#[derive(Debug, Clone, Copy)]
pub struct Loopback<'a> {
    /// Service name for error messages, e.g. `"Google"`.
    pub label: &'a str,
    /// Host in the redirect URI: `127.0.0.1` or `localhost`.
    pub host: &'a str,
    /// `None` picks an ephemeral port; `Some` is a port the provider has registered.
    pub port: Option<u16>,
    /// Path of the redirect URI after the port: `""` or `"/"`, as the provider registered it.
    pub path: &'a str,
}

/// Browser authorization over a loopback redirect. Binds the listener,
/// calls `build_url(redirect_uri, state)` for the authorize URL, hands it to
/// `open_url` (which should show it to the user / open a browser), then serves
/// requests until the redirect carrying `state` arrives. Returns the
/// authorization code and the redirect URI to repeat in the token request.
pub async fn authorize(
    target: Loopback<'_>,
    build_url: impl FnOnce(&str, &str) -> String,
    open_url: impl FnOnce(&str) + Send,
) -> Result<(String, String)> {
    let label = target.label;
    let listener = match target.port {
        Some(port) => TcpListener::bind(("127.0.0.1", port)).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::AddrInUse {
                anyhow!("port {port} is busy; close the other program using it and try again")
            } else {
                anyhow::Error::new(e).context(format!("opening local port {port} for the {label} sign-in redirect"))
            }
        })?,
        None => TcpListener::bind(("127.0.0.1", 0))
            .await
            .with_context(|| format!("opening a local port for the {label} sign-in redirect"))?,
    };
    let redirect_uri = format!("http://{}:{}{}", target.host, listener.local_addr()?.port(), target.path);
    let state = uuid::Uuid::new_v4().simple().to_string();
    open_url(&build_url(&redirect_uri, &state));
    let code = tokio::time::timeout(LOGIN_TIMEOUT, wait_for_code(&listener, label, &state))
        .await
        .map_err(|_| anyhow!("{label} sign-in timed out after 5 minutes; start again"))??;
    Ok((code, redirect_uri))
}

/// Serves the loopback listener until the provider's redirect arrives and
/// returns the authorization code. Requests without `code`/`error` (favicon,
/// stray probes) are answered with 404 and ignored; a redirect with the wrong
/// `state` or an `error` ends the sign-in.
async fn wait_for_code(listener: &TcpListener, label: &str, state: &str) -> Result<String> {
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
            Err(e) => respond(&mut stream, 400, &format!("{label} sign-in failed: {e}")).await,
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
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(k), decode(v))
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
        let server = tokio::spawn(async move { wait_for_code(&listener, "Google", "s3cret").await });
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
        let server = tokio::spawn(async move { wait_for_code(&listener, "Google", "right").await });
        let page = request(port, "/?state=wrong&code=abc").await;
        assert!(page.starts_with("HTTP/1.1 400"), "{page}");
        assert!(page.contains("state mismatch"));
        let err = server.await.unwrap().unwrap_err().to_string();
        assert!(err.contains("state mismatch"), "{err}");
    }

    #[tokio::test]
    async fn loopback_reports_denied_consent_and_escapes_html() {
        let (listener, port) = bind_local().await;
        let server = tokio::spawn(async move { wait_for_code(&listener, "Dropbox", "st").await });
        let page = request(port, "/?state=st&error=access_denied").await;
        assert!(page.starts_with("HTTP/1.1 400"), "{page}");
        assert!(page.contains("Dropbox sign-in failed: access was denied"));
        assert!(server.await.unwrap().unwrap_err().to_string().contains("denied"));

        let (listener, port) = bind_local().await;
        let server = tokio::spawn(async move { wait_for_code(&listener, "Google", "st").await });
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

    /// Plays the browser: reads `redirect_uri` and `state` from the fake authorize URL and
    /// calls back with `code` (or a wrong state when `state` is overridden).
    fn browser(url: &str, code: &str, state: Option<&str>) -> tokio::task::JoinHandle<String> {
        let query = url.split_once('?').unwrap().1;
        let param = |name: &str| {
            query.split('&').find_map(|p| p.strip_prefix(&format!("{name}=")).map(decode)).unwrap()
        };
        let redirect = param("redirect_uri");
        let state = state.map(str::to_string).unwrap_or_else(|| param("state"));
        let port: u16 = redirect.rsplit(':').next().unwrap().trim_end_matches('/').parse().unwrap();
        let target = format!("/?code={code}&state={state}");
        tokio::spawn(async move {
            assert!(request(port, "/favicon.ico").await.starts_with("HTTP/1.1 404"));
            request(port, &target).await
        })
    }

    fn fake_url(redirect_uri: &str, state: &str) -> String {
        format!("https://auth.test/?redirect_uri={}&state={state}", urlencoding::encode(redirect_uri))
    }

    #[tokio::test]
    async fn authorize_on_ephemeral_port() {
        let target = Loopback { label: "Microsoft", host: "localhost", port: None, path: "" };
        let mut page = None;
        let (code, redirect) =
            authorize(target, fake_url, |url| page = Some(browser(url, "c0de", None))).await.unwrap();
        assert_eq!(code, "c0de");
        assert!(redirect.starts_with("http://localhost:") && !redirect.ends_with('/'), "{redirect}");
        assert!(page.unwrap().await.unwrap().contains("You can close this tab and return to Puplify."));
    }

    #[tokio::test]
    async fn authorize_on_fixed_port_keeps_redirect_path() {
        let (probe, port) = bind_local().await;
        drop(probe);
        let target = Loopback { label: "Dropbox", host: "localhost", port: Some(port), path: "/" };
        let mut page = None;
        let (code, redirect) =
            authorize(target, fake_url, |url| page = Some(browser(url, "abc", None))).await.unwrap();
        assert_eq!(code, "abc");
        assert_eq!(redirect, format!("http://localhost:{port}/"));
        page.unwrap().await.unwrap();
    }

    #[tokio::test]
    async fn authorize_reports_busy_fixed_port_without_opening_browser() {
        let (_held, port) = bind_local().await;
        let target = Loopback { label: "Dropbox", host: "localhost", port: Some(port), path: "/" };
        let err = authorize(target, fake_url, |_| panic!("must not open a browser")).await.unwrap_err();
        assert_eq!(err.to_string(), format!("port {port} is busy; close the other program using it and try again"));
    }

    #[tokio::test]
    async fn authorize_fails_on_state_mismatch() {
        let target = Loopback { label: "Google", host: "127.0.0.1", port: None, path: "" };
        let mut page = None;
        let err = authorize(target, fake_url, |url| page = Some(browser(url, "x", Some("other")))).await.unwrap_err();
        assert!(err.to_string().contains("state mismatch"), "{err}");
        assert!(page.unwrap().await.unwrap().starts_with("HTTP/1.1 400"));
    }
}
