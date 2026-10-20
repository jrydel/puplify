//! Outgoing webhooks. Each request is a JSON POST signed with HMAC-SHA256
//! over `"{timestamp}.{body}"` when a secret is configured:
//!
//! ```text
//! X-Verbatim-Event: recording.completed
//! X-Verbatim-Delivery: <uuid>
//! X-Verbatim-Timestamp: <unix seconds>
//! X-Verbatim-Signature: sha256=<hex hmac>
//! ```

use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

use crate::config::{WebhookConfig, WebhookEvent};
use crate::transcript::{Segment, Speaker, SpeakerLabels, Transcript};

pub fn display_name(hook: &WebhookConfig) -> &str {
    if hook.name.trim().is_empty() {
        &hook.url
    } else {
        &hook.name
    }
}

pub fn started_payload(t: &Transcript) -> Value {
    json!({
        "transcript": { "id": t.id, "title": t.title, "started_at": t.started_at, "engine": t.engine },
    })
}

pub fn segment_payload(t: &Transcript, segment: &Segment) -> Value {
    json!({
        "transcript_id": t.id,
        "title": t.title,
        "segment": segment,
        "speaker_label": t.labels.get(segment.speaker),
    })
}

/// Full transcript: structured segments plus the rendered plain text.
pub fn completed_payload(t: &Transcript) -> Value {
    json!({
        "transcript": t,
        "duration_ms": t.duration_ms(),
        "text": t.to_text(),
    })
}

/// HMAC-SHA256 signature header value for `body` at `timestamp`.
pub fn sign(secret: &str, timestamp: i64, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

/// POSTs `payload` wrapped in an envelope, retrying network errors and 5xx/429
/// responses with backoff up to `attempts` times. Returns a status summary.
pub async fn send(hook: &WebhookConfig, event: WebhookEvent, payload: &Value, attempts: u32) -> Result<String> {
    if hook.url.trim().is_empty() {
        bail!("webhook URL is empty");
    }
    let delivery = uuid::Uuid::new_v4().to_string();
    let mut envelope = json!({ "event": event.as_str(), "delivery_id": delivery, "sent_at": Utc::now() });
    if let (Value::Object(env), Value::Object(body)) = (&mut envelope, payload) {
        env.extend(body.clone());
    }
    let body = serde_json::to_vec(&envelope)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("verbatim/", env!("CARGO_PKG_VERSION")))
        .build()?;

    let mut last_err = anyhow!("no attempts made");
    for attempt in 0..attempts.max(1) {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(2u64.pow(attempt * 2 - 1))).await; // 2 s, 8 s, …
        }
        let timestamp = Utc::now().timestamp();
        let mut req = client
            .post(hook.url.trim())
            .header("Content-Type", "application/json")
            .header("X-Verbatim-Event", event.as_str())
            .header("X-Verbatim-Delivery", &delivery)
            .header("X-Verbatim-Timestamp", timestamp.to_string())
            .body(body.clone());
        if !hook.secret.is_empty() {
            req = req.header("X-Verbatim-Signature", sign(&hook.secret, timestamp, &body));
        }
        match req.send().await {
            Ok(resp) if resp.status().is_success() => return Ok(format!("HTTP {}", resp.status().as_u16())),
            Ok(resp) => {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                last_err = anyhow!("HTTP {}: {}", status.as_u16(), text.chars().take(200).collect::<String>());
                if !(status.is_server_error() || status.as_u16() == 429) {
                    break;
                }
            }
            Err(e) => last_err = anyhow!("{e}"),
        }
    }
    Err(last_err)
}

/// Sends a sample `recording.completed` event so users can check their endpoint.
pub async fn send_test(hook: &WebhookConfig) -> Result<String> {
    let mut t = Transcript::new(
        "Webhook test",
        "test",
        SpeakerLabels { me: "Me".into(), them: "Them".into() },
    );
    t.push(Segment { speaker: Speaker::Me, start_ms: 0, end_ms: 1_800, text: "This is a test transcript.".into() });
    t.push(Segment { speaker: Speaker::Them, start_ms: 2_000, end_ms: 3_500, text: "Received loud and clear.".into() });
    t.ended_at = Some(t.started_at + chrono::Duration::seconds(4));
    send(hook, WebhookEvent::RecordingCompleted, &completed_payload(&t), 1).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_matches_reference_hmac() {
        // printf '1700000000.{"a":1}' | openssl dgst -sha256 -hmac secret
        assert_eq!(
            sign("secret", 1_700_000_000, br#"{"a":1}"#),
            "sha256=49f24e537407743fa4a0242bb63b94b9a47ee99cbbe071ccd8a22550ae411686"
        );
    }
}
