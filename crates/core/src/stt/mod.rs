//! Speech-to-text engines. Each consumes the capture stream and emits
//! transcript segments; the session layer is engine-agnostic.

use anyhow::Result;
use serde::Serialize;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::audio::AudioMsg;
use crate::config::{SttConfig, SttProvider};
use crate::transcript::{Segment, Speaker};

pub mod deepgram;
pub mod models;
pub mod whisper;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SttEvent {
    /// Provisional text for the utterance in progress (streaming engines only).
    Partial { speaker: Speaker, text: String },
    /// Finalized segment.
    Final { segment: Segment },
    /// Non-fatal error, or fatal error right before the engine stops.
    Error { message: String },
}

pub enum Engine {
    Whisper(whisper::WhisperEngine),
    Deepgram(deepgram::DeepgramEngine),
}

impl Engine {
    /// Prepares the configured engine. Loading a Whisper model can take a few
    /// seconds, so this runs on the blocking pool.
    pub async fn from_config(cfg: &SttConfig) -> Result<Self> {
        match cfg.provider {
            SttProvider::Whisper => {
                let cfg = cfg.whisper.clone();
                let vad = models::ensure_vad().await?;
                let engine =
                    tokio::task::spawn_blocking(move || whisper::WhisperEngine::load(&cfg, &vad)).await??;
                Ok(Self::Whisper(engine))
            }
            SttProvider::Deepgram => Ok(Self::Deepgram(deepgram::DeepgramEngine::new(&cfg.deepgram)?)),
        }
    }

    /// Engine identifier stored with each transcript, e.g. `whisper:base.en`.
    pub fn label(&self) -> String {
        match self {
            Self::Whisper(e) => e.label(),
            Self::Deepgram(e) => e.label(),
        }
    }

    /// Starts transcribing. Returns once the engine is ready to accept audio
    /// (for Deepgram: the WebSocket is connected), so credential errors surface
    /// here. The returned task finishes after `audio` closes and all buffered
    /// audio is transcribed; `events` is dropped at that point.
    pub async fn start(
        self,
        speakers: Vec<Speaker>,
        audio: UnboundedReceiver<AudioMsg>,
        events: UnboundedSender<SttEvent>,
    ) -> Result<JoinHandle<()>> {
        match self {
            Self::Whisper(e) => Ok(e.start(audio, events)),
            Self::Deepgram(e) => e.start(speakers, audio, events).await,
        }
    }
}
