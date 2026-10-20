//! Recording session: capture → speech-to-text → transcript (autosaved) →
//! connectors. Progress is reported as [`SessionEvent`]s for the UI/CLI.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::Serialize;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::audio::{AudioMsg, Capture, Resampler, SAMPLE_RATE};
use crate::config::Config;
use crate::connectors::{self, DeliveryResult};
use crate::stt::{Engine, SttEvent};
use crate::transcript::{Segment, Speaker, SpeakerLabels, Transcript};
use crate::store;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    Started { transcript_id: String, title: String, engine: String },
    Partial { speaker: Speaker, text: String },
    Segment { segment: Segment },
    Error { message: String },
    /// Input levels (RMS, 0–1) of each source since the previous event, ~12 per second.
    Levels { me: f32, them: f32 },
    /// Transcription ended and the transcript is saved locally.
    Finished { transcript_id: String },
    Delivery { result: DeliveryResult },
    /// All connectors have run; nothing more will be emitted.
    Done { transcript_id: String },
}

const AUTOSAVE_EVERY: Duration = Duration::from_secs(10);
const LEVELS_EVERY: Duration = Duration::from_millis(80);

pub struct Recording {
    pub transcript_id: String,
    pub title: String,
    capture: Option<Capture>,
    task: JoinHandle<Result<Transcript>>,
}

impl Recording {
    /// Loads the engine, starts capturing and transcribing. Fails fast on
    /// missing models, bad credentials or unavailable audio devices.
    pub async fn start(cfg: Config, title: String, events: UnboundedSender<SessionEvent>) -> Result<Self> {
        let mut speakers = Vec::new();
        if cfg.capture.mic {
            speakers.push(Speaker::Me);
        }
        if cfg.capture.system {
            speakers.push(Speaker::Them);
        }
        if speakers.is_empty() {
            bail!("enable at least one audio source (microphone or system audio)");
        }
        let engine = Engine::from_config(&cfg.stt).await?;
        let (audio_tx, audio_rx) = mpsc::unbounded_channel();
        let (capture_tx, capture_rx) = mpsc::unbounded_channel();
        tokio::spawn(meter(capture_rx, audio_tx, events.clone()));
        let (transcript, task) = spawn_pipeline(cfg.clone(), title, engine, speakers, audio_rx, None, events).await?;
        let capture = match tokio::task::block_in_place(|| Capture::start(&cfg.capture, capture_tx)) {
            Ok(c) => c,
            Err(e) => {
                // Nothing was recorded: stop the pipeline before it delivers anything.
                task.abort();
                let _ = store::delete(&transcript.id);
                return Err(e);
            }
        };
        Ok(Self { transcript_id: transcript.id, title: transcript.title, capture: Some(capture), task })
    }

    /// True once transcription ended on its own (e.g. the cloud stream failed).
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    /// Stops capture, waits for the remaining audio to be transcribed, saves
    /// the transcript and runs all connectors.
    pub async fn stop(mut self) -> Result<Transcript> {
        if let Some(capture) = self.capture.take() {
            tokio::task::spawn_blocking(move || capture.stop()).await?;
        }
        self.task.await?
    }
}

/// Forwards capture audio to the engine while reporting input levels, so the
/// UI can show that both sources are actually live.
async fn meter(
    mut capture: UnboundedReceiver<AudioMsg>,
    engine: UnboundedSender<AudioMsg>,
    events: UnboundedSender<SessionEvent>,
) {
    let (mut me, mut them) = (0f32, 0f32);
    let mut last = Instant::now();
    while let Some(msg) = capture.recv().await {
        if let AudioMsg::Frame { speaker, samples } = &msg {
            if !samples.is_empty() {
                let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
                let level = match speaker {
                    Speaker::Me => &mut me,
                    Speaker::Them => &mut them,
                };
                *level = level.max(rms);
            }
        }
        if engine.send(msg).is_err() {
            return;
        }
        if last.elapsed() >= LEVELS_EVERY {
            let _ = events.send(SessionEvent::Levels { me, them });
            (me, them) = (0.0, 0.0);
            last = Instant::now();
        }
    }
}

/// Transcribes a WAV file through the configured engine, as if it were
/// system audio, then saves and delivers it like a recording.
pub async fn transcribe_file(cfg: Config, path: &Path, title: String, events: UnboundedSender<SessionEvent>) -> Result<Transcript> {
    let samples = read_wav_16k_mono(path)?;
    let duration = chrono::Duration::milliseconds((samples.len() as u64 * 1000 / SAMPLE_RATE as u64) as i64);
    let engine = Engine::from_config(&cfg.stt).await?;
    let (audio_tx, audio_rx) = mpsc::unbounded_channel();
    let (_, task) =
        spawn_pipeline(cfg, title, engine, vec![Speaker::Them], audio_rx, Some(duration), events).await?;
    for chunk in samples.chunks(SAMPLE_RATE as usize / 10) {
        audio_tx
            .send(AudioMsg::Frame { speaker: Speaker::Them, samples: chunk.to_vec() })
            .ok()
            .context("transcription engine stopped early")?;
    }
    drop(audio_tx);
    task.await?
}

fn read_wav_16k_mono(path: &Path) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
    let spec = reader.spec();
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|s| s.map(|s| s as f32 * scale)).collect::<Result<_, _>>()?
        }
    };
    let mut mono = Vec::with_capacity(interleaved.len() / spec.channels as usize);
    crate::audio::downmix(&interleaved, spec.channels as usize, &mut mono);
    let mut out = Vec::new();
    Resampler::new(spec.sample_rate, SAMPLE_RATE).process(&mono, &mut out);
    Ok(out)
}

/// Starts the engine and the collector task. Returns the initial (saved)
/// transcript and the task that yields the finished one.
async fn spawn_pipeline(
    cfg: Config,
    title: String,
    engine: Engine,
    speakers: Vec<Speaker>,
    audio_rx: UnboundedReceiver<AudioMsg>,
    fixed_duration: Option<chrono::Duration>,
    events: UnboundedSender<SessionEvent>,
) -> Result<(Transcript, JoinHandle<Result<Transcript>>)> {
    let labels = SpeakerLabels { me: cfg.capture.me_label.clone(), them: cfg.capture.them_label.clone() };
    let mut transcript = Transcript::new(title, engine.label(), labels);
    let (stt_tx, mut stt_rx) = mpsc::unbounded_channel();
    let engine_task = engine.start(speakers, audio_rx, stt_tx).await?;
    // Timestamps are relative to the moment audio starts flowing.
    transcript.started_at = Utc::now();
    store::save(&transcript)?;
    let initial = transcript.clone();

    let task = tokio::spawn(async move {
        let _ = events.send(SessionEvent::Started {
            transcript_id: transcript.id.clone(),
            title: transcript.title.clone(),
            engine: transcript.engine.clone(),
        });
        connectors::notify_started(&cfg, &transcript);

        let mut last_save = Instant::now();
        while let Some(event) = stt_rx.recv().await {
            match event {
                SttEvent::Final { segment } => {
                    connectors::notify_segment(&cfg, &transcript, &segment);
                    let _ = events.send(SessionEvent::Segment { segment: segment.clone() });
                    transcript.push(segment);
                    if last_save.elapsed() >= AUTOSAVE_EVERY {
                        if let Err(e) = store::save(&transcript) {
                            tracing::warn!("autosave failed: {e:#}");
                        }
                        last_save = Instant::now();
                    }
                }
                SttEvent::Partial { speaker, text } => {
                    let _ = events.send(SessionEvent::Partial { speaker, text });
                }
                SttEvent::Error { message } => {
                    let _ = events.send(SessionEvent::Error { message });
                }
            }
        }
        if let Err(e) = engine_task.await {
            let _ = events.send(SessionEvent::Error { message: format!("transcription task failed: {e}") });
        }

        transcript.ended_at = Some(match fixed_duration {
            Some(d) => transcript.started_at + d,
            None => Utc::now(),
        });
        store::save(&transcript)?;
        let _ = events.send(SessionEvent::Finished { transcript_id: transcript.id.clone() });

        if transcript.segments.is_empty() {
            let _ = events.send(SessionEvent::Error {
                message: "no speech was transcribed; connectors skipped".into(),
            });
        } else {
            for result in connectors::deliver(&cfg, &transcript).await {
                let _ = events.send(SessionEvent::Delivery { result });
            }
        }
        let _ = events.send(SessionEvent::Done { transcript_id: transcript.id.clone() });
        Ok(transcript)
    });
    Ok((initial, task))
}
