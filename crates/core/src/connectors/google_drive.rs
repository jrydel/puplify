//! Google Drive upload. Sign-in is shared with Google Calendar and lives in
//! [`super::google`]; the `drive.file` scope means the app only ever sees files
//! and folders it created itself.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::google;
use super::oauth::{http, http_error};
use crate::config::{GoogleAccount, GoogleDriveConfig};
use crate::transcript::Transcript;

const API: &str = "https://www.googleapis.com/drive/v3";
const UPLOAD: &str = "https://www.googleapis.com/upload/drive/v3/files";
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";

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
pub async fn export(cfg: &GoogleDriveConfig, account: &GoogleAccount, transcript: &Transcript) -> Result<String> {
    if cfg.formats.is_empty() {
        bail!("no Google Drive formats selected");
    }
    let client = http()?;
    let token = google::access_token(account, &client).await?;
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
