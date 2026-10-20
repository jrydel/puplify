use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths;

/// Application settings, persisted as JSON in the config directory.
/// The file is written with mode 0600 because it may contain API keys and
/// webhook secrets.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub capture: CaptureConfig,
    pub stt: SttConfig,
    pub connectors: ConnectorsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CaptureConfig {
    /// Record the microphone (labelled as `me_label`).
    pub mic: bool,
    /// Record system/meeting audio (labelled as `them_label`).
    pub system: bool,
    /// Input device id; `None` = system default.
    pub mic_device: Option<String>,
    /// Linux: PulseAudio/PipeWire monitor source name; `None` = monitor of the default output.
    /// macOS: ignored (ScreenCaptureKit captures all system audio).
    pub system_device: Option<String>,
    pub me_label: String,
    pub them_label: String,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            mic: true,
            system: true,
            mic_device: None,
            system_device: None,
            me_label: "Me".into(),
            them_label: "Them".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SttProvider {
    #[default]
    Whisper,
    Deepgram,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SttConfig {
    pub provider: SttProvider,
    pub whisper: WhisperConfig,
    pub deepgram: DeepgramConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WhisperConfig {
    /// Model name from the catalog (e.g. `base.en`) or an absolute path to a ggml `.bin` file.
    pub model: String,
    /// ISO language code, or `auto` to detect.
    pub language: String,
    /// CPU threads; 0 = automatic.
    pub threads: u32,
}

impl Default for WhisperConfig {
    fn default() -> Self {
        Self {
            model: "base.en".into(),
            language: "en".into(),
            threads: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DeepgramConfig {
    pub api_key: String,
    pub model: String,
    /// Language code, or `multi` for Nova-3 multilingual.
    pub language: String,
}

impl Default for DeepgramConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: "nova-3".into(),
            language: "en".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    Txt,
    Md,
    Json,
}

impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Txt => "txt",
            Self::Md => "md",
            Self::Json => "json",
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Self::Txt => "text/plain; charset=utf-8",
            Self::Md => "text/markdown; charset=utf-8",
            Self::Json => "application/json",
        }
    }
}

fn default_formats() -> Vec<ExportFormat> {
    vec![ExportFormat::Txt]
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ConnectorsConfig {
    pub folder: FolderConfig,
    pub onedrive: OneDriveConfig,
    pub google_drive: GoogleDriveConfig,
    pub dropbox: DropboxConfig,
    pub notion: NotionConfig,
    pub webhooks: Vec<WebhookConfig>,
}

/// Copies finished transcripts into a local directory (e.g. a synced Obsidian vault).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FolderConfig {
    pub enabled: bool,
    pub path: PathBuf,
    pub formats: Vec<ExportFormat>,
}

impl Default for FolderConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            path: PathBuf::new(),
            formats: default_formats(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct OneDriveConfig {
    pub enabled: bool,
    /// Application (client) ID of your Microsoft Entra app registration.
    pub client_id: String,
    /// `common` accepts both personal and work/school accounts.
    pub tenant: String,
    /// Destination folder path relative to the drive root.
    pub folder: String,
    pub formats: Vec<ExportFormat>,
}

impl Default for OneDriveConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            client_id: String::new(),
            tenant: "common".into(),
            folder: "Transcripts".into(),
            formats: default_formats(),
        }
    }
}

/// Uploads transcripts to Google Drive (`drive.file` scope: only files this app creates).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GoogleDriveConfig {
    pub enabled: bool,
    /// OAuth client ID of type "TVs and Limited Input devices".
    pub client_id: String,
    /// Google requires the client secret in token requests, even for this client type.
    pub client_secret: String,
    /// Folder name in My Drive; created on first use.
    pub folder: String,
    pub formats: Vec<ExportFormat>,
}

impl Default for GoogleDriveConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            client_id: String::new(),
            client_secret: String::new(),
            folder: "Transcripts".into(),
            formats: default_formats(),
        }
    }
}

/// Uploads transcripts to Dropbox. Sign-in uses OAuth 2 PKCE, so no app secret is needed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DropboxConfig {
    pub enabled: bool,
    /// App key from the Dropbox App Console.
    pub app_key: String,
    /// Destination folder (relative to the app folder for app-folder apps).
    pub folder: String,
    pub formats: Vec<ExportFormat>,
}

impl Default for DropboxConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            app_key: String::new(),
            folder: "/Transcripts".into(),
            formats: default_formats(),
        }
    }
}

/// Creates a Notion page per transcript under an existing page.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct NotionConfig {
    pub enabled: bool,
    /// Internal integration token.
    pub token: String,
    /// Page ID or URL of the parent page (shared with the integration).
    pub parent_page_id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum WebhookEvent {
    #[serde(rename = "recording.started")]
    RecordingStarted,
    #[serde(rename = "transcript.segment")]
    TranscriptSegment,
    #[serde(rename = "recording.completed")]
    RecordingCompleted,
}

impl WebhookEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RecordingStarted => "recording.started",
            Self::TranscriptSegment => "transcript.segment",
            Self::RecordingCompleted => "recording.completed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WebhookConfig {
    pub name: String,
    pub url: String,
    /// When non-empty, requests carry `X-Puplify-Signature: sha256=<hmac>`.
    pub secret: String,
    pub enabled: bool,
    pub events: Vec<WebhookEvent>,
}

impl Default for WebhookConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            url: String::new(),
            secret: String::new(),
            enabled: true,
            events: vec![WebhookEvent::RecordingStarted, WebhookEvent::RecordingCompleted],
        }
    }
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        Ok(paths::config_dir()?.join("config.json"))
    }

    /// Loads the config, returning defaults when the file does not exist yet.
    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self) -> Result<()> {
        let json = serde_json::to_vec_pretty(self)?;
        paths::write_atomic(&Self::path()?, &json, true)
    }
}
