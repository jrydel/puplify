//! Cloud transcription via Deepgram's streaming API. Microphone and system
//! audio are sent as separate channels of one multichannel stream, so speaker
//! attribution stays exact.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, bail, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest, http::HeaderValue, Message};

use super::SttEvent;
use crate::audio::{AudioMsg, SAMPLE_RATE};
use crate::config::DeepgramConfig;
use crate::transcript::{Segment, Speaker};

pub struct DeepgramEngine {
    cfg: DeepgramConfig,
}

impl DeepgramEngine {
    pub fn new(cfg: &DeepgramConfig) -> Result<Self> {
        if cfg.api_key.trim().is_empty() {
            bail!("Deepgram API key is not set");
        }
        Ok(Self { cfg: cfg.clone() })
    }

    pub fn label(&self) -> String {
        format!("deepgram:{}", self.cfg.model)
    }

    pub async fn start(
        self,
        speakers: Vec<Speaker>,
        mut audio: UnboundedReceiver<AudioMsg>,
        events: UnboundedSender<SttEvent>,
    ) -> Result<JoinHandle<()>> {
        let channels = speakers.len();
        let url = format!(
            "wss://api.deepgram.com/v1/listen?model={model}&language={lang}&encoding=linear16\
             &sample_rate={SAMPLE_RATE}&channels={channels}&multichannel={multi}&punctuate=true\
             &smart_format=true&interim_results=true&endpointing=300",
            model = urlencoding::encode(&self.cfg.model),
            lang = urlencoding::encode(&self.cfg.language),
            multi = channels > 1,
        );
        let mut request = url.into_client_request()?;
        request.headers_mut().insert(
            "Authorization",
            HeaderValue::from_str(&format!("Token {}", self.cfg.api_key.trim()))?,
        );
        let (ws, _) = tokio_tungstenite::connect_async(request).await.map_err(|e| match e {
            tungstenite::Error::Http(resp) => {
                let body = resp.body().as_deref().map(String::from_utf8_lossy).unwrap_or_default();
                anyhow!("Deepgram rejected the connection (HTTP {}): {body}", resp.status())
            }
            e => anyhow!("connecting to Deepgram: {e}"),
        })?;
        let (mut sink, mut stream) = ws.split();
        let closing = Arc::new(AtomicBool::new(false));

        let mixer_speakers = speakers.clone();
        let sender = {
            let events = events.clone();
            let closing = closing.clone();
            tokio::spawn(async move {
                let mut mixer = Mixer::new(mixer_speakers);
                while let Some(msg) = audio.recv().await {
                    match msg {
                        AudioMsg::Frame { speaker, samples } => {
                            mixer.push(speaker, &samples);
                            while let Some(block) = mixer.pop(false) {
                                sink.send(Message::binary(block)).await?;
                            }
                        }
                        AudioMsg::Error { speaker, message } => {
                            let _ = events.send(SttEvent::Error {
                                message: format!("{speaker:?} audio stopped: {message}"),
                            });
                        }
                    }
                }
                while let Some(block) = mixer.pop(true) {
                    sink.send(Message::binary(block)).await?;
                }
                closing.store(true, Ordering::SeqCst);
                sink.send(Message::text(r#"{"type":"CloseStream"}"#)).await?;
                Ok::<_, tungstenite::Error>(sink)
            })
        };

        Ok(tokio::spawn(async move {
            let mut close_reason = None;
            while let Some(msg) = stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => handle_message(&text, &speakers, &events),
                    Ok(Message::Close(frame)) => {
                        close_reason = frame.map(|f| format!("{} {}", f.code, f.reason));
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        close_reason = Some(e.to_string());
                        break;
                    }
                }
            }
            if !closing.load(Ordering::SeqCst) {
                // Stop consuming audio; capture notices the closed channel and the
                // session finalizes with what was transcribed so far.
                sender.abort();
                let reason = close_reason.unwrap_or_else(|| "connection lost".into());
                let _ = events.send(SttEvent::Error {
                    message: format!("Deepgram closed the stream early: {reason}"),
                });
                return;
            }
            match sender.await {
                Ok(Ok(_sink)) => {}
                Ok(Err(e)) => {
                    let _ = events.send(SttEvent::Error { message: format!("Deepgram: {e}") });
                }
                Err(e) => {
                    let _ = events.send(SttEvent::Error { message: format!("Deepgram sender: {e}") });
                }
            }
        }))
    }
}

#[derive(Deserialize)]
struct DgMessage {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    channel_index: Vec<usize>,
    #[serde(default)]
    start: f64,
    #[serde(default)]
    duration: f64,
    #[serde(default)]
    is_final: bool,
    channel: Option<DgChannel>,
    /// Present on `Error` messages.
    description: Option<String>,
}

#[derive(Deserialize)]
struct DgChannel {
    alternatives: Vec<DgAlternative>,
}

#[derive(Deserialize)]
struct DgAlternative {
    transcript: String,
}

fn handle_message(text: &str, speakers: &[Speaker], events: &UnboundedSender<SttEvent>) {
    let Ok(msg) = serde_json::from_str::<DgMessage>(text) else {
        tracing::debug!("unparsed Deepgram message: {text}");
        return;
    };
    match msg.kind.as_str() {
        "Results" => {
            let channel = msg.channel_index.first().copied().unwrap_or(0);
            let Some(&speaker) = speakers.get(channel) else { return };
            let Some(text) = msg
                .channel
                .and_then(|c| c.alternatives.into_iter().next())
                .map(|a| a.transcript.trim().to_string())
            else {
                return;
            };
            if text.is_empty() {
                return;
            }
            let event = if msg.is_final {
                let start_ms = (msg.start * 1000.0).round() as u64;
                let end_ms = ((msg.start + msg.duration) * 1000.0).round() as u64;
                SttEvent::Final { segment: Segment { speaker, start_ms, end_ms, text } }
            } else {
                SttEvent::Partial { speaker, text }
            };
            let _ = events.send(event);
        }
        "Error" => {
            let message = msg.description.unwrap_or_else(|| text.to_string());
            let _ = events.send(SttEvent::Error { message: format!("Deepgram: {message}") });
        }
        _ => {}
    }
}

/// 100 ms blocks keep latency low without flooding the socket.
const BLOCK: usize = SAMPLE_RATE as usize / 10;
/// If one source stalls longer than this, pad it with silence so the other
/// keeps streaming.
const MAX_SKEW: usize = SAMPLE_RATE as usize / 2;

/// Interleaves per-speaker mono streams into little-endian i16 frames,
/// channel order = `speakers` order.
struct Mixer {
    speakers: Vec<Speaker>,
    buffers: Vec<VecDeque<f32>>,
}

impl Mixer {
    fn new(speakers: Vec<Speaker>) -> Self {
        let buffers = speakers.iter().map(|_| VecDeque::new()).collect();
        Self { speakers, buffers }
    }

    fn push(&mut self, speaker: Speaker, samples: &[f32]) {
        if let Some(i) = self.speakers.iter().position(|&s| s == speaker) {
            self.buffers[i].extend(samples);
        }
    }

    /// Next block ready to send. With `flush`, drains everything (padding
    /// shorter channels with silence).
    fn pop(&mut self, flush: bool) -> Option<Vec<u8>> {
        let max = self.buffers.iter().map(VecDeque::len).max()?;
        let min = self.buffers.iter().map(VecDeque::len).min()?;
        let frames = if min >= BLOCK {
            BLOCK
        } else if max > 0 && (flush || max - min >= MAX_SKEW) {
            BLOCK.min(max)
        } else {
            return None;
        };
        let mut out = Vec::with_capacity(frames * self.buffers.len() * 2);
        for _ in 0..frames {
            for buf in &mut self.buffers {
                let s = buf.pop_front().unwrap_or(0.0).clamp(-1.0, 1.0);
                out.extend_from_slice(&((s * i16::MAX as f32) as i16).to_le_bytes());
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixer_interleaves_in_speaker_order() {
        let mut m = Mixer::new(vec![Speaker::Me, Speaker::Them]);
        m.push(Speaker::Them, &vec![-1.0; BLOCK]);
        assert!(m.pop(false).is_none(), "waits for the other channel");
        m.push(Speaker::Me, &vec![1.0; BLOCK]);
        let block = m.pop(false).expect("block");
        assert_eq!(block.len(), BLOCK * 4);
        assert_eq!(i16::from_le_bytes([block[0], block[1]]), i16::MAX);
        assert_eq!(i16::from_le_bytes([block[2], block[3]]), -i16::MAX);
        assert!(m.pop(true).is_none());
    }

    #[test]
    fn mixer_pads_stalled_channel() {
        let mut m = Mixer::new(vec![Speaker::Me, Speaker::Them]);
        m.push(Speaker::Me, &vec![0.5; MAX_SKEW]);
        let block = m.pop(false).expect("stalled channel is padded");
        assert_eq!(i16::from_le_bytes([block[2], block[3]]), 0);
    }

    #[test]
    fn results_map_channels_to_speakers() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let speakers = [Speaker::Me, Speaker::Them];
        let msg = r#"{"type":"Results","channel_index":[1,2],"start":1.5,"duration":2.0,"is_final":true,
            "channel":{"alternatives":[{"transcript":" Hello there. ","confidence":0.9}]}}"#;
        handle_message(msg, &speakers, &tx);
        let interim = r#"{"type":"Results","channel_index":[0,2],"start":0,"duration":1,"is_final":false,
            "channel":{"alternatives":[{"transcript":"Hel"}]}}"#;
        handle_message(interim, &speakers, &tx);
        match rx.try_recv().unwrap() {
            SttEvent::Final { segment } => {
                assert_eq!(segment, Segment { speaker: Speaker::Them, start_ms: 1500, end_ms: 3500, text: "Hello there.".into() });
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(rx.try_recv().unwrap(), SttEvent::Partial { speaker: Speaker::Me, .. }));
    }
}
