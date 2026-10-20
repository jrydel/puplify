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
    pub calendar: CalendarConfig,
    pub connectors: ConnectorsConfig,
}

/// OAuth app registrations baked into official builds at compile time: the
/// release workflow (or a developer's shell) exports these env vars before
/// `cargo build`. Unset or empty (unset GitHub variables expand to empty
/// strings) means the build has no registration for that service.
pub mod builtin {
    const fn non_empty(v: Option<&'static str>) -> Option<&'static str> {
        match v {
            Some(s) if !s.is_empty() => Some(s),
            _ => None,
        }
    }

    /// A Google registration needs both halves; one without the other is unusable.
    const fn pair(id: Option<&'static str>, secret: Option<&'static str>) -> Option<(&'static str, &'static str)> {
        match (non_empty(id), non_empty(secret)) {
            (Some(id), Some(secret)) => Some((id, secret)),
            _ => None,
        }
    }

    pub const MICROSOFT_CLIENT_ID: Option<&str> = non_empty(option_env!("PUPLIFY_MICROSOFT_CLIENT_ID"));
    pub const GOOGLE_CLIENT_ID: Option<&str> = non_empty(option_env!("PUPLIFY_GOOGLE_CLIENT_ID"));
    pub const GOOGLE_CLIENT_SECRET: Option<&str> = non_empty(option_env!("PUPLIFY_GOOGLE_CLIENT_SECRET"));
    /// Google client ID and secret, present only when both are set.
    pub const GOOGLE_CREDENTIALS: Option<(&str, &str)> = pair(GOOGLE_CLIENT_ID, GOOGLE_CLIENT_SECRET);
    pub const DROPBOX_APP_KEY: Option<&str> = non_empty(option_env!("PUPLIFY_DROPBOX_APP_KEY"));

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn empty_values_count_as_absent() {
            assert_eq!(non_empty(None), None);
            assert_eq!(non_empty(Some("")), None);
            assert_eq!(non_empty(Some("id")), Some("id"));
        }

        #[test]
        fn google_needs_both_halves() {
            assert_eq!(pair(Some("id"), Some("secret")), Some(("id", "secret")));
            assert_eq!(pair(Some("id"), None), None);
            assert_eq!(pair(Some("id"), Some("")), None);
            assert_eq!(pair(None, Some("secret")), None);
        }
    }
}

/// Which services this build ships a registration for, i.e. which Connect buttons work.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct BuiltinAccounts {
    pub microsoft: bool,
    pub google: bool,
    pub dropbox: bool,
}

pub fn builtin_accounts() -> BuiltinAccounts {
    BuiltinAccounts {
        microsoft: builtin::MICROSOFT_CLIENT_ID.is_some(),
        google: builtin::GOOGLE_CREDENTIALS.is_some(),
        dropbox: builtin::DROPBOX_APP_KEY.is_some(),
    }
}

/// What to do when a call starts: record it, ask first, or nothing.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AutoRecord {
    Off,
    Ask,
    #[default]
    Auto,
}

/// Automatic recording: which calendars to watch and how to react when
/// another app starts using the microphone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CalendarConfig {
    /// For an online meeting from a watched calendar that is under way.
    pub mode: AutoRecord,
    /// For any other call (no matching calendar event).
    pub calls: AutoRecord,
    /// Watch the connected Google calendar.
    pub google: bool,
    /// Watch the connected Microsoft calendar.
    pub microsoft: bool,
    /// Private iCal addresses (`https://` or `webcal://`) of calendars to watch, no sign-in needed.
    pub links: Vec<String>,
}

impl Default for CalendarConfig {
    fn default() -> Self {
        Self { mode: AutoRecord::Auto, calls: AutoRecord::Ask, google: false, microsoft: false, links: Vec::new() }
    }
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
            model: crate::stt::models::BUNDLED_MODEL.into(),
            language: "auto".into(),
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
    /// Destination folder path relative to the drive root.
    pub folder: String,
    pub formats: Vec<ExportFormat>,
}

impl Default for OneDriveConfig {
    fn default() -> Self {
        Self {
            enabled: false,
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
    /// Folder name in My Drive; created on first use.
    pub folder: String,
    pub formats: Vec<ExportFormat>,
}

impl Default for GoogleDriveConfig {
    fn default() -> Self {
        Self {
            enabled: false,
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
    /// Destination folder (relative to the app folder for app-folder apps).
    pub folder: String,
    pub formats: Vec<ExportFormat>,
}

impl Default for DropboxConfig {
    fn default() -> Self {
        Self {
            enabled: false,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_and_connector_defaults() {
        let cfg: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.calendar.mode, AutoRecord::Auto);
        assert!(!cfg.calendar.google && !cfg.calendar.microsoft);
        assert_eq!(cfg.connectors.onedrive.folder, "Transcripts");
    }

    #[test]
    fn partial_sections_keep_other_defaults() {
        let cfg: Config = serde_json::from_str(
            r#"{ "calendar": { "mode": "ask", "microsoft": true },
                 "connectors": { "dropbox": { "enabled": true } } }"#,
        )
        .unwrap();
        assert_eq!(cfg.calendar.mode, AutoRecord::Ask);
        assert!(cfg.calendar.microsoft);
        assert!(cfg.connectors.dropbox.enabled);
        assert_eq!(cfg.connectors.dropbox.folder, "/Transcripts");
        let json = serde_json::to_value(&cfg).unwrap();
        assert_eq!(json["calendar"]["mode"], "ask");
    }

    #[test]
    fn legacy_accounts_table_is_ignored() {
        let cfg: Config = serde_json::from_str(
            r#"{ "accounts": { "microsoft": { "client_id": "x", "tenant": "common" },
                               "google": { "client_id": "id", "client_secret": "s" },
                               "dropbox": { "app_key": "k" } },
                 "calendar": { "google": true } }"#,
        )
        .unwrap();
        assert!(cfg.calendar.google);
        assert!(serde_json::to_value(&cfg).unwrap().get("accounts").is_none());
    }

    #[test]
    fn builtin_accounts_match_compiled_in_values() {
        let b = builtin_accounts();
        assert_eq!(b.microsoft, builtin::MICROSOFT_CLIENT_ID.is_some());
        assert_eq!(b.google, builtin::GOOGLE_CREDENTIALS.is_some());
        assert_eq!(b.dropbox, builtin::DROPBOX_APP_KEY.is_some());
    }
}
