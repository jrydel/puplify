//! OneDrive upload through Microsoft Graph. Sign-in is shared with the
//! Outlook calendar and lives in [`super::microsoft`].

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

use super::microsoft::{self, GRAPH};
use super::oauth::http;
use crate::config::{MicrosoftAccount, OneDriveConfig};
use crate::transcript::Transcript;

/// Graph simple upload limit; larger files use an upload session.
const SIMPLE_UPLOAD_MAX: usize = 4 * 1024 * 1024;
/// Upload-session chunks must be multiples of 320 KiB.
const CHUNK: usize = 320 * 1024 * 32;

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
pub async fn export(cfg: &OneDriveConfig, account: &MicrosoftAccount, transcript: &Transcript) -> Result<String> {
    if cfg.formats.is_empty() {
        bail!("no OneDrive formats selected");
    }
    let client = http()?;
    let token = microsoft::access_token(account, &client).await?;
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
