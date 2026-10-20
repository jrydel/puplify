//! The single active recording, shared by the UI commands and the
//! autopilot so both start and stop recordings the same way.

use std::time::Instant;

use anyhow::{bail, Result};
use puplify_core::{
    calendar::Meeting,
    config::Config,
    session::{Recording, SessionEvent},
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{mpsc, Mutex};

/// A call the autopilot noticed: another app took the microphone, during an
/// online meeting from the calendar (`meeting`) or outside any.
#[derive(Debug, Clone, Serialize)]
pub struct Call {
    pub title: String,
    pub meeting: Option<Meeting>,
    /// App holding the microphone, when the OS names it.
    pub app: Option<String>,
}

/// Why a recording was started.
pub enum Origin {
    Manual,
    /// Started by the autopilot (or from its Ask prompt) for this call.
    Call(Call),
}

impl Origin {
    fn call(&self) -> Option<Call> {
        match self {
            Origin::Call(call) => Some(call.clone()),
            Origin::Manual => None,
        }
    }
}

pub struct Active {
    pub recording: Recording,
    pub origin: Origin,
    /// Updated whenever a transcript segment arrives; the autopilot uses it
    /// to stop after a meeting has ended and nobody is talking.
    pub last_speech: Instant,
}

#[derive(Default)]
pub struct AppState {
    pub active: Mutex<Option<Active>>,
    /// Call the autopilot offered to record in Ask mode.
    pub offered: Mutex<Option<Call>>,
}

#[derive(Clone, Serialize)]
pub struct RecordingInfo {
    pub transcript_id: String,
    pub title: String,
    /// Call that triggered the recording automatically, if any.
    pub call: Option<Call>,
}

/// Starts recording unless one is already running. Emits `recording-started`
/// so the UI also follows recordings it did not start itself.
pub async fn start(app: &AppHandle, title: String, origin: Origin) -> Result<RecordingInfo> {
    let state = app.state::<AppState>();
    let mut guard = state.active.lock().await;
    if guard.is_some() {
        bail!("already recording");
    }
    let cfg = Config::load()?;
    let (tx, mut rx) = mpsc::unbounded_channel::<SessionEvent>();
    let forward = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(event) = rx.recv().await {
            if matches!(event, SessionEvent::Segment { .. }) {
                if let Some(active) = forward.state::<AppState>().active.lock().await.as_mut() {
                    active.last_speech = Instant::now();
                }
            }
            let _ = forward.emit("session", event);
        }
    });
    let recording = Recording::start(cfg, title, tx).await?;
    let info = RecordingInfo {
        transcript_id: recording.transcript_id.clone(),
        title: recording.title.clone(),
        call: origin.call(),
    };
    *guard = Some(Active { recording, origin, last_speech: Instant::now() });
    let _ = app.emit("recording-started", info.clone());
    Ok(info)
}

/// Stops the active recording, if any, and returns at once; completion and
/// delivery results arrive as `session` events.
pub async fn stop(app: &AppHandle) {
    let Some(active) = app.state::<AppState>().active.lock().await.take() else {
        return;
    };
    // Lets the UI switch to "finishing" right away, also when the autopilot stopped it.
    let _ = app.emit("recording-stopped", active.recording.transcript_id.clone());
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = active.recording.stop().await {
            let _ = app.emit("session", SessionEvent::Error { message: format!("{e:#}") });
        }
    });
}

pub async fn status(app: &AppHandle) -> Option<RecordingInfo> {
    let state = app.state::<AppState>();
    let guard = state.active.lock().await;
    guard.as_ref().map(|a| RecordingInfo {
        transcript_id: a.recording.transcript_id.clone(),
        title: a.recording.title.clone(),
        call: a.origin.call(),
    })
}
