//! Core of Puplify: audio capture, speech-to-text, transcript storage and
//! delivery connectors (local folder, OneDrive, webhooks). Shared by the CLI and
//! the Tauri desktop app.

pub mod audio;
pub mod calendar;
pub mod config;
pub mod connectors;
pub mod paths;
pub mod session;
pub mod store;
pub mod stt;
pub mod transcript;

pub use config::Config;
pub use session::{Recording, SessionEvent};
pub use transcript::{Segment, Speaker, Transcript};
