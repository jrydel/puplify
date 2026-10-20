// Prevents an extra console window on Windows release builds; harmless elsewhere.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autopilot;
mod recorder;

use std::future::Future;

use puplify_core::{
    audio::{self, DeviceInfo},
    calendar,
    config::{self, BuiltinAccounts, Config},
    connectors::{self, dropbox, google, microsoft, webhook, DeliveryResult},
    store::{self, TranscriptSummary},
    stt::models::{self, ModelStatus},
    Transcript,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;
use tokio::sync::Mutex;
use tokio::task::AbortHandle;

use autopilot::CalendarSnapshot;
use recorder::{AppState, Origin, RecordingInfo};

/// Errors cross the IPC boundary as display strings.
type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

fn anyhow_err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

#[derive(Serialize)]
struct TranscriptView {
    transcript: Transcript,
    text: String,
}

#[derive(Clone, Serialize)]
struct ModelProgress {
    name: String,
    done: u64,
    total: Option<u64>,
}

/// The browser sign-in currently waiting for its redirect, so a newer
/// attempt or "Cancel" can abort it (which also frees its local port).
#[derive(Default)]
struct PendingLogin(Mutex<Option<AbortHandle>>);

/// Runs a browser sign-in to completion unless it is cancelled.
async fn browser_login(
    pending: &PendingLogin,
    login: impl Future<Output = anyhow::Result<String>> + Send + 'static,
) -> CmdResult<String> {
    let task = tokio::spawn(login);
    if let Some(previous) = pending.0.lock().await.replace(task.abort_handle()) {
        previous.abort();
    }
    match task.await {
        Ok(result) => result.map_err(anyhow_err),
        Err(e) if e.is_cancelled() => Err("sign-in cancelled".into()),
        Err(e) => Err(err(e)),
    }
}

/// Opens `url` in the default browser.
fn browser_opener(app: AppHandle) -> impl FnOnce(&str) + Send + 'static {
    move |url| {
        if let Err(e) = app.opener().open_url(url, None::<&str>) {
            tracing::warn!("opening the browser: {e}");
        }
    }
}

#[tauri::command]
fn get_config() -> CmdResult<Config> {
    Config::load().map_err(anyhow_err)
}

#[tauri::command]
fn save_config(config: Config) -> CmdResult<()> {
    config.save().map_err(anyhow_err)
}

#[tauri::command]
async fn list_devices() -> CmdResult<Vec<DeviceInfo>> {
    tokio::task::spawn_blocking(audio::list_devices).await.map_err(err)?.map_err(anyhow_err)
}

#[tauri::command]
async fn recording_status(app: AppHandle) -> CmdResult<Option<RecordingInfo>> {
    Ok(recorder::status(&app).await)
}

#[tauri::command]
async fn start_recording(app: AppHandle, title: String) -> CmdResult<RecordingInfo> {
    recorder::start(&app, title, Origin::Manual).await.map_err(anyhow_err)
}

/// Starts recording the meeting the autopilot offered in Ask mode, so it
/// stops automatically like an automatic recording.
#[tauri::command]
async fn record_offered_meeting(app: AppHandle, state: State<'_, AppState>) -> CmdResult<RecordingInfo> {
    let meeting = state.offered.lock().await.take().ok_or("that meeting is no longer on offer")?;
    recorder::start(&app, meeting.title.clone(), Origin::Calendar(meeting)).await.map_err(anyhow_err)
}

#[tauri::command]
async fn dismiss_offered_meeting(state: State<'_, AppState>) -> CmdResult<()> {
    state.offered.lock().await.take();
    Ok(())
}

/// Stops capture and returns immediately; completion and connector results
/// arrive as `session` events.
#[tauri::command]
async fn stop_recording(app: AppHandle) -> CmdResult<()> {
    recorder::stop(&app).await;
    Ok(())
}

async fn ensure_not_recording(state: &AppState, id: &str, action: &str) -> CmdResult<()> {
    if state.active.lock().await.as_ref().is_some_and(|a| a.recording.transcript_id == id) {
        return Err(format!("cannot {action} while recording"));
    }
    Ok(())
}

#[tauri::command]
fn list_transcripts() -> CmdResult<Vec<TranscriptSummary>> {
    store::list().map_err(anyhow_err)
}

#[tauri::command]
fn get_transcript(id: String) -> CmdResult<TranscriptView> {
    let transcript = store::load(&id).map_err(anyhow_err)?;
    let text = transcript.to_text();
    Ok(TranscriptView { transcript, text })
}

#[tauri::command]
async fn rename_transcript(state: State<'_, AppState>, id: String, title: String) -> CmdResult<()> {
    ensure_not_recording(&state, &id, "rename").await?;
    let title = title.trim();
    if title.is_empty() {
        return Err("title cannot be empty".into());
    }
    let mut t = store::load(&id).map_err(anyhow_err)?;
    t.title = title.to_string();
    store::save(&t).map_err(anyhow_err)
}

#[tauri::command]
async fn delete_transcript(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    ensure_not_recording(&state, &id, "delete").await?;
    store::delete(&id).map_err(anyhow_err)
}

#[tauri::command]
async fn deliver_transcript(id: String) -> CmdResult<Vec<DeliveryResult>> {
    let cfg = Config::load().map_err(anyhow_err)?;
    let t = store::load(&id).map_err(anyhow_err)?;
    let results = connectors::deliver(&cfg, &t).await;
    if results.is_empty() {
        return Err("no destinations are turned on".into());
    }
    Ok(results)
}

#[tauri::command]
fn list_models() -> CmdResult<Vec<ModelStatus>> {
    models::list().map_err(anyhow_err)
}

#[tauri::command]
async fn download_model(app: AppHandle, name: String) -> CmdResult<()> {
    let mut last_mb = u64::MAX;
    let progress_name = name.clone();
    models::download(&name, |done, total| {
        let mb = done / 1_000_000;
        if mb != last_mb {
            last_mb = mb;
            let _ = app.emit("model-progress", ModelProgress { name: progress_name.clone(), done, total });
        }
    })
    .await
    .map(|_| ())
    .map_err(anyhow_err)
}

#[tauri::command]
fn builtin_accounts() -> BuiltinAccounts {
    config::builtin_accounts()
}

#[tauri::command]
async fn cancel_login(pending: State<'_, PendingLogin>) -> CmdResult<()> {
    if let Some(task) = pending.0.lock().await.take() {
        task.abort();
    }
    Ok(())
}

#[tauri::command]
fn microsoft_status() -> CmdResult<Option<String>> {
    microsoft::status().map_err(anyhow_err)
}

/// Opens Microsoft's sign-in page and waits for the redirect back to
/// Puplify (up to five minutes). Returns the account name.
#[tauri::command]
async fn microsoft_login(app: AppHandle, pending: State<'_, PendingLogin>) -> CmdResult<String> {
    browser_login(&pending, microsoft::login(browser_opener(app))).await
}

#[tauri::command]
fn microsoft_logout() -> CmdResult<()> {
    microsoft::logout().map_err(anyhow_err)
}

#[tauri::command]
fn google_status() -> CmdResult<Option<String>> {
    google::status().map_err(anyhow_err)
}

/// Opens Google's sign-in page and waits for the redirect back to Puplify.
#[tauri::command]
async fn google_login(app: AppHandle, pending: State<'_, PendingLogin>) -> CmdResult<String> {
    browser_login(&pending, google::login(browser_opener(app))).await
}

#[tauri::command]
fn google_logout() -> CmdResult<()> {
    google::logout().map_err(anyhow_err)
}

#[tauri::command]
fn dropbox_status() -> CmdResult<Option<String>> {
    dropbox::status().map_err(anyhow_err)
}

/// Opens Dropbox's sign-in page and waits for the redirect back to Puplify.
#[tauri::command]
async fn dropbox_login(app: AppHandle, pending: State<'_, PendingLogin>) -> CmdResult<String> {
    browser_login(&pending, dropbox::login(browser_opener(app))).await
}

#[tauri::command]
fn dropbox_logout() -> CmdResult<()> {
    dropbox::logout().map_err(anyhow_err)
}

/// Online meetings from the enabled calendars, read now (the autopilot
/// refreshes its own copy every minute and emits `calendar`).
#[tauri::command]
async fn upcoming_meetings() -> CmdResult<CalendarSnapshot> {
    let cfg = Config::load().map_err(anyhow_err)?;
    let (meetings, warnings) =
        calendar::upcoming(&cfg, chrono::Duration::hours(autopilot::AHEAD_HOURS)).await;
    Ok(CalendarSnapshot { meetings, warnings })
}

#[tauri::command]
async fn test_webhook(index: usize) -> CmdResult<String> {
    let cfg = Config::load().map_err(anyhow_err)?;
    let hook = cfg.connectors.webhooks.get(index).ok_or("webhook not found; save settings first")?;
    webhook::send_test(hook).await.map_err(anyhow_err)
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn,whisper_rs=off".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .manage(AppState::default())
        .manage(PendingLogin::default())
        .setup(|app| {
            // Whisper + VAD models shipped inside the bundle (see bundle.resources).
            match app.path().resource_dir() {
                Ok(dir) => models::set_bundled_dir(dir.join("models")),
                Err(e) => tracing::warn!("no resource dir; bundled models unavailable: {e}"),
            }
            autopilot::spawn(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            list_devices,
            recording_status,
            start_recording,
            record_offered_meeting,
            dismiss_offered_meeting,
            stop_recording,
            list_transcripts,
            get_transcript,
            rename_transcript,
            delete_transcript,
            deliver_transcript,
            list_models,
            download_model,
            microsoft_status,
            builtin_accounts,
            cancel_login,
            microsoft_login,
            microsoft_logout,
            google_status,
            google_login,
            google_logout,
            dropbox_status,
            dropbox_login,
            dropbox_logout,
            upcoming_meetings,
            test_webhook,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Puplify");
}
