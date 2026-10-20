// Prevents an extra console window on Windows release builds; harmless elsewhere.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use puplify_core::{
    audio::{self, DeviceInfo},
    config::Config,
    connectors::{self, dropbox, google_drive, onedrive, webhook, DeliveryResult},
    session::{Recording, SessionEvent},
    store::{self, TranscriptSummary},
    stt::models::{self, ModelStatus},
    Transcript,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::{mpsc, Mutex};

/// Errors cross the IPC boundary as display strings.
type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

fn anyhow_err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

#[derive(Default)]
struct AppState {
    recording: Mutex<Option<Recording>>,
}

#[derive(Clone, Serialize)]
struct RecordingInfo {
    transcript_id: String,
    title: String,
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

/// Outcome of a background sign-in, emitted as the `account-login` event.
#[derive(Clone, Serialize)]
struct LoginResult {
    provider: &'static str,
    ok: bool,
    account: Option<String>,
    error: Option<String>,
}

impl LoginResult {
    fn from(provider: &'static str, result: anyhow::Result<String>) -> Self {
        match result {
            Ok(account) => Self { provider, ok: true, account: Some(account), error: None },
            Err(e) => Self { provider, ok: false, account: None, error: Some(format!("{e:#}")) },
        }
    }
}

/// Code the user enters on the provider's sign-in page.
#[derive(Serialize)]
struct DeviceLogin {
    user_code: String,
    verification_uri: String,
    expires_in: u64,
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
async fn recording_status(state: State<'_, AppState>) -> CmdResult<Option<RecordingInfo>> {
    let guard = state.recording.lock().await;
    Ok(guard.as_ref().map(|r| RecordingInfo { transcript_id: r.transcript_id.clone(), title: r.title.clone() }))
}

#[tauri::command]
async fn start_recording(app: AppHandle, state: State<'_, AppState>, title: String) -> CmdResult<RecordingInfo> {
    let mut guard = state.recording.lock().await;
    if guard.is_some() {
        return Err("already recording".into());
    }
    let cfg = Config::load().map_err(anyhow_err)?;
    let (tx, mut rx) = mpsc::unbounded_channel::<SessionEvent>();
    tauri::async_runtime::spawn(async move {
        while let Some(event) = rx.recv().await {
            let _ = app.emit("session", event);
        }
    });
    let recording = Recording::start(cfg, title, tx).await.map_err(anyhow_err)?;
    let info = RecordingInfo { transcript_id: recording.transcript_id.clone(), title: recording.title.clone() };
    *guard = Some(recording);
    Ok(info)
}

/// Stops capture and returns immediately; completion and connector results
/// arrive as `session` events.
#[tauri::command]
async fn stop_recording(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    let Some(recording) = state.recording.lock().await.take() else {
        return Ok(());
    };
    tauri::async_runtime::spawn(async move {
        if let Err(e) = recording.stop().await {
            let _ = app.emit("session", SessionEvent::Error { message: format!("{e:#}") });
        }
    });
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
    if state.recording.lock().await.as_ref().is_some_and(|r| r.transcript_id == id) {
        return Err("cannot rename while recording".into());
    }
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
    if state.recording.lock().await.as_ref().is_some_and(|r| r.transcript_id == id) {
        return Err("cannot delete while recording".into());
    }
    store::delete(&id).map_err(anyhow_err)
}

#[tauri::command]
async fn deliver_transcript(id: String) -> CmdResult<Vec<DeliveryResult>> {
    let cfg = Config::load().map_err(anyhow_err)?;
    let t = store::load(&id).map_err(anyhow_err)?;
    let results = connectors::deliver(&cfg, &t).await;
    if results.is_empty() {
        return Err("no connectors are enabled".into());
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
fn onedrive_status() -> CmdResult<Option<String>> {
    onedrive::status().map_err(anyhow_err)
}

/// Starts device-code sign-in; the outcome arrives as an `account-login` event.
#[tauri::command]
async fn onedrive_login(app: AppHandle) -> CmdResult<DeviceLogin> {
    let cfg = Config::load().map_err(anyhow_err)?.connectors.onedrive;
    let code = onedrive::begin_login(&cfg).await.map_err(anyhow_err)?;
    let view = DeviceLogin {
        user_code: code.user_code.clone(),
        verification_uri: code.verification_uri.clone(),
        expires_in: code.expires_in,
    };
    tauri::async_runtime::spawn(async move {
        let result = onedrive::finish_login(&cfg, &code).await;
        let _ = app.emit("account-login", LoginResult::from("onedrive", result));
    });
    Ok(view)
}

#[tauri::command]
fn onedrive_logout() -> CmdResult<()> {
    onedrive::logout().map_err(anyhow_err)
}

#[tauri::command]
fn gdrive_status() -> CmdResult<Option<String>> {
    google_drive::status().map_err(anyhow_err)
}

/// Starts device-code sign-in; the outcome arrives as an `account-login` event.
#[tauri::command]
async fn gdrive_login(app: AppHandle) -> CmdResult<DeviceLogin> {
    let cfg = Config::load().map_err(anyhow_err)?.connectors.google_drive;
    let code = google_drive::begin_login(&cfg).await.map_err(anyhow_err)?;
    let view = DeviceLogin {
        user_code: code.user_code.clone(),
        verification_uri: code.verification_uri.clone(),
        expires_in: code.expires_in,
    };
    tauri::async_runtime::spawn(async move {
        let result = google_drive::finish_login(&cfg, &code).await;
        let _ = app.emit("account-login", LoginResult::from("google_drive", result));
    });
    Ok(view)
}

#[tauri::command]
fn gdrive_logout() -> CmdResult<()> {
    google_drive::logout().map_err(anyhow_err)
}

#[tauri::command]
fn dropbox_status() -> CmdResult<Option<String>> {
    dropbox::status().map_err(anyhow_err)
}

/// Returns the authorization URL and PKCE verifier; the user pastes back the code.
#[tauri::command]
fn dropbox_begin_login() -> CmdResult<dropbox::PendingLogin> {
    let cfg = Config::load().map_err(anyhow_err)?.connectors.dropbox;
    dropbox::begin_login(&cfg).map_err(anyhow_err)
}

#[tauri::command]
async fn dropbox_finish_login(verifier: String, code: String) -> CmdResult<String> {
    let cfg = Config::load().map_err(anyhow_err)?.connectors.dropbox;
    dropbox::finish_login(&cfg, &verifier, code.trim()).await.map_err(anyhow_err)
}

#[tauri::command]
fn dropbox_logout() -> CmdResult<()> {
    dropbox::logout().map_err(anyhow_err)
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
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            list_devices,
            recording_status,
            start_recording,
            stop_recording,
            list_transcripts,
            get_transcript,
            rename_transcript,
            delete_transcript,
            deliver_transcript,
            list_models,
            download_model,
            onedrive_status,
            onedrive_login,
            onedrive_logout,
            gdrive_status,
            gdrive_login,
            gdrive_logout,
            dropbox_status,
            dropbox_begin_login,
            dropbox_finish_login,
            dropbox_logout,
            test_webhook,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Puplify");
}
