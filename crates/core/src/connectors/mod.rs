//! Destinations for transcripts. Finished transcripts go to every enabled
//! connector; webhooks additionally receive live events during recording.

use serde::Serialize;

use crate::config::{Config, WebhookEvent};
use crate::transcript::{Segment, Transcript};

pub mod dropbox;
pub mod folder;
pub mod google_drive;
pub mod notion;
pub mod oauth;
pub mod onedrive;
pub mod webhook;

#[derive(Debug, Clone, Serialize)]
pub struct DeliveryResult {
    pub connector: String,
    pub ok: bool,
    /// Location written (path/URL) on success, error text on failure.
    pub detail: String,
}

impl DeliveryResult {
    fn from(connector: String, result: anyhow::Result<String>) -> Self {
        match result {
            Ok(detail) => Self { connector, ok: true, detail },
            Err(e) => Self { connector, ok: false, detail: format!("{e:#}") },
        }
    }
}

/// Sends a finished transcript to every enabled connector concurrently.
pub async fn deliver(cfg: &Config, transcript: &Transcript) -> Vec<DeliveryResult> {
    let c = &cfg.connectors;
    let mut tasks: Vec<futures_util::future::BoxFuture<'_, DeliveryResult>> = Vec::new();
    if c.folder.enabled {
        tasks.push(Box::pin(async {
            DeliveryResult::from("folder".into(), folder::export(&c.folder, transcript).await)
        }));
    }
    if c.onedrive.enabled {
        tasks.push(Box::pin(async {
            DeliveryResult::from("onedrive".into(), onedrive::export(&c.onedrive, transcript).await)
        }));
    }
    if c.google_drive.enabled {
        tasks.push(Box::pin(async {
            DeliveryResult::from("google-drive".into(), google_drive::export(&c.google_drive, transcript).await)
        }));
    }
    if c.dropbox.enabled {
        tasks.push(Box::pin(async {
            DeliveryResult::from("dropbox".into(), dropbox::export(&c.dropbox, transcript).await)
        }));
    }
    if c.notion.enabled {
        tasks.push(Box::pin(async {
            DeliveryResult::from("notion".into(), notion::export(&c.notion, transcript).await)
        }));
    }
    for hook in c.webhooks.iter().filter(|h| h.enabled && h.events.contains(&WebhookEvent::RecordingCompleted)) {
        tasks.push(Box::pin(async move {
            let payload = webhook::completed_payload(transcript);
            let result = webhook::send(hook, WebhookEvent::RecordingCompleted, &payload, 3).await;
            DeliveryResult::from(format!("webhook:{}", webhook::display_name(hook)), result)
        }));
    }
    futures_util::future::join_all(tasks).await
}

/// Fire-and-forget `recording.started` notification.
pub fn notify_started(cfg: &Config, transcript: &Transcript) {
    let payload = webhook::started_payload(transcript);
    spawn_live(cfg, WebhookEvent::RecordingStarted, payload);
}

/// Fire-and-forget `transcript.segment` notification.
pub fn notify_segment(cfg: &Config, transcript: &Transcript, segment: &Segment) {
    if !cfg.connectors.webhooks.iter().any(|h| h.enabled && h.events.contains(&WebhookEvent::TranscriptSegment)) {
        return;
    }
    let payload = webhook::segment_payload(transcript, segment);
    spawn_live(cfg, WebhookEvent::TranscriptSegment, payload);
}

fn spawn_live(cfg: &Config, event: WebhookEvent, payload: serde_json::Value) {
    for hook in cfg.connectors.webhooks.iter().filter(|h| h.enabled && h.events.contains(&event)) {
        let hook = hook.clone();
        let payload = payload.clone();
        tokio::spawn(async move {
            if let Err(e) = webhook::send(&hook, event, &payload, 1).await {
                tracing::warn!("webhook {} ({}): {e:#}", webhook::display_name(&hook), event.as_str());
            }
        });
    }
}
