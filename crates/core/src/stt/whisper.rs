//! Local transcription with whisper.cpp. Each audio source is split into
//! utterances by a simple energy VAD and every utterance is transcribed as
//! soon as the speaker pauses, giving near-live output fully offline.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

use super::{models, SttEvent};
use crate::audio::{AudioMsg, SAMPLE_RATE};
use crate::config::WhisperConfig;
use crate::transcript::{Segment, Speaker};

pub struct WhisperEngine {
    ctx: Arc<WhisperContext>,
    vad_model: String,
    model: String,
    language: String,
    threads: i32,
}

impl WhisperEngine {
    pub fn load(cfg: &WhisperConfig, vad_model: &Path) -> Result<Self> {
        let path = models::resolve(&cfg.model)?;
        whisper_rs::install_logging_hooks();
        let ctx = WhisperContext::new_with_params(&path, WhisperContextParameters::default())
            .map_err(|e| anyhow!("loading whisper model {}: {e}", path.display()))?;
        let threads = match cfg.threads {
            0 => std::thread::available_parallelism().map_or(4, |n| n.get().min(8)) as i32,
            n => n as i32,
        };
        let model = if Path::new(&cfg.model).is_absolute() {
            path.file_stem().map_or_else(|| cfg.model.clone(), |s| s.to_string_lossy().into_owned())
        } else {
            cfg.model.clone()
        };
        let language = match cfg.language.trim() {
            "" => "auto".to_string(),
            lang => lang.to_string(),
        };
        let vad_model = vad_model.to_string_lossy().into_owned();
        Ok(Self { ctx: Arc::new(ctx), vad_model, model, language, threads })
    }

    pub fn label(&self) -> String {
        format!("whisper:{}", self.model)
    }

    pub fn start(
        self,
        mut audio: UnboundedReceiver<AudioMsg>,
        events: UnboundedSender<SttEvent>,
    ) -> JoinHandle<()> {
        tokio::task::spawn_blocking(move || {
            let mut state = match self.ctx.create_state() {
                Ok(state) => state,
                Err(e) => {
                    let _ = events.send(SttEvent::Error { message: format!("whisper: {e}") });
                    return;
                }
            };
            // BTreeMap: deterministic flush order at the end.
            let mut chunkers: BTreeMap<Speaker, Chunker> = BTreeMap::new();
            while let Some(msg) = audio.blocking_recv() {
                match msg {
                    AudioMsg::Frame { speaker, samples } => {
                        let chunker = chunkers.entry(speaker).or_default();
                        for chunk in chunker.push(&samples) {
                            self.transcribe(&mut state, speaker, chunk, &events);
                        }
                    }
                    AudioMsg::Error { speaker, message } => {
                        let _ = events.send(SttEvent::Error {
                            message: format!("{speaker:?} audio stopped: {message}"),
                        });
                    }
                }
            }
            for (speaker, chunker) in &mut chunkers {
                if let Some(chunk) = chunker.flush() {
                    self.transcribe(&mut state, *speaker, chunk, &events);
                }
            }
        })
    }

    fn transcribe(
        &self,
        state: &mut WhisperState,
        speaker: Speaker,
        chunk: Chunk,
        events: &UnboundedSender<SttEvent>,
    ) {
        let mut samples = chunk.samples;
        // whisper.cpp rejects inputs shorter than one second.
        let min_len = SAMPLE_RATE as usize * 11 / 10;
        if samples.len() < min_len {
            samples.resize(min_len, 0.0);
        }
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(self.threads);
        params.set_language(Some(&self.language));
        params.set_translate(false);
        params.set_no_context(true);
        params.set_suppress_blank(true);
        params.set_suppress_nst(true);
        params.set_no_speech_thold(0.6);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        // Silero VAD drops non-speech inside the chunk (timestamps stay on the
        // original timeline), so background noise does not turn into text.
        params.set_vad_model_path(Some(&self.vad_model));
        params.enable_vad(true);

        if let Err(e) = state.full(params, &samples) {
            let _ = events.send(SttEvent::Error { message: format!("whisper: {e}") });
            return;
        }
        let offset_ms = chunk.start_sample * 1000 / SAMPLE_RATE as u64;
        let mut previous = String::new();
        for seg in state.as_iter() {
            if seg.no_speech_probability() > 0.6 {
                continue;
            }
            let Ok(text) = seg.to_str_lossy() else { continue };
            let text = text.trim();
            // Repeated identical lines are a classic whisper decoding loop.
            if is_non_speech(text) || text == previous {
                continue;
            }
            previous = text.to_string();
            // Whisper timestamps are in 10 ms units.
            let start_ms = offset_ms + seg.start_timestamp().max(0) as u64 * 10;
            let end_ms = offset_ms + seg.end_timestamp().max(0) as u64 * 10;
            let segment = Segment { speaker, start_ms, end_ms, text: text.to_string() };
            if events.send(SttEvent::Final { segment }).is_err() {
                return;
            }
        }
    }
}

/// Whisper emits markers such as `[BLANK_AUDIO]`, `(music)` or `♪` for non-speech.
fn is_non_speech(text: &str) -> bool {
    let bracketed = |open: char, close: char| text.starts_with(open) && text.ends_with(close);
    text.is_empty()
        || bracketed('[', ']')
        || bracketed('(', ')')
        || bracketed('*', '*')
        || !text.chars().any(char::is_alphanumeric)
}

pub(crate) struct Chunk {
    /// Index of the first sample since the source started.
    pub start_sample: u64,
    pub samples: Vec<f32>,
}

const FRAME: usize = SAMPLE_RATE as usize * 30 / 1000; // 30 ms
const PREROLL_FRAMES: usize = 10; // 300 ms kept before speech onset
const MIN_SPEECH_FRAMES: usize = 5; // 150 ms of voiced audio to count as an utterance
const END_SILENCE_FRAMES: usize = 27; // ~800 ms pause ends an utterance
const LONG_END_SILENCE_FRAMES: usize = 10; // ~300 ms once the utterance is long
const LONG_UTTERANCE: usize = SAMPLE_RATE as usize * 15;
const MAX_UTTERANCE: usize = SAMPLE_RATE as usize * 25; // whisper window is 30 s
/// Initial noise-floor estimate is capped so speech at the very start still registers.
const MAX_INITIAL_FLOOR: f32 = 0.02;
const MIN_SPEECH_RMS: f32 = 0.004;

/// Energy-based utterance segmenter with an adaptive noise floor.
pub(crate) struct Chunker {
    buf: Vec<f32>,
    buf_start: u64,
    pending: Vec<f32>,
    speech_frames: usize,
    silence_frames: usize,
    /// Minimum-tracking estimate of background noise RMS.
    noise_floor: Option<f32>,
}

impl Default for Chunker {
    fn default() -> Self {
        Self {
            buf: Vec::new(),
            buf_start: 0,
            pending: Vec::new(),
            speech_frames: 0,
            silence_frames: 0,
            noise_floor: None,
        }
    }
}

impl Chunker {
    pub fn push(&mut self, samples: &[f32]) -> Vec<Chunk> {
        self.pending.extend_from_slice(samples);
        let mut out = Vec::new();
        let mut offset = 0;
        while self.pending.len() - offset >= FRAME {
            let frame = &self.pending[offset..offset + FRAME];
            offset += FRAME;
            let rms = (frame.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt();
            let floor = *self.noise_floor.get_or_insert(rms.min(MAX_INITIAL_FLOOR));
            let voiced = rms > (floor * 3.0).max(MIN_SPEECH_RMS);
            // Falls fast, rises ~18 %/s: learns a noisy room within seconds while
            // the dips between words keep it from climbing during speech.
            let next = if rms < floor { floor * 0.7 + rms * 0.3 } else { floor * 1.005 };
            self.noise_floor = Some(next.max(1e-4));
            self.buf.extend_from_slice(frame);
            if voiced {
                self.speech_frames += 1;
                self.silence_frames = 0;
            } else {
                self.silence_frames += 1;
            }

            if self.speech_frames == 0 {
                let keep = PREROLL_FRAMES * FRAME;
                if self.buf.len() > keep {
                    let drop = self.buf.len() - keep;
                    self.buf.drain(..drop);
                    self.buf_start += drop as u64;
                }
                continue;
            }

            let end_silence = if self.buf.len() >= LONG_UTTERANCE {
                LONG_END_SILENCE_FRAMES
            } else {
                END_SILENCE_FRAMES
            };
            let paused = self.silence_frames >= end_silence;
            if paused && self.speech_frames < MIN_SPEECH_FRAMES {
                // A click or cough, not speech: discard and keep listening.
                self.reset_utterance(false);
            } else if paused || self.buf.len() + FRAME > MAX_UTTERANCE {
                out.push(self.reset_utterance(true).expect("utterance"));
            }
        }
        self.pending.drain(..offset);
        out
    }

    /// Returns the in-progress utterance, if it contains speech.
    pub fn flush(&mut self) -> Option<Chunk> {
        if self.speech_frames >= MIN_SPEECH_FRAMES {
            self.reset_utterance(true)
        } else {
            None
        }
    }

    fn reset_utterance(&mut self, emit: bool) -> Option<Chunk> {
        let samples = std::mem::take(&mut self.buf);
        let start_sample = self.buf_start;
        self.buf_start += samples.len() as u64;
        self.speech_frames = 0;
        self.silence_frames = 0;
        emit.then_some(Chunk { start_sample, samples })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(seconds: f32, amp: f32) -> Vec<f32> {
        let n = (SAMPLE_RATE as f32 * seconds) as usize;
        (0..n).map(|i| amp * (i as f32 * 0.07).sin()).collect()
    }

    #[test]
    fn utterances_split_on_pauses_with_correct_offsets() {
        let mut c = Chunker::default();
        let mut chunks = Vec::new();
        chunks.extend(c.push(&tone(2.0, 0.0)));
        chunks.extend(c.push(&tone(1.5, 0.3)));
        chunks.extend(c.push(&tone(1.0, 0.0)));
        chunks.extend(c.push(&tone(1.0, 0.3)));
        assert_eq!(chunks.len(), 1, "second utterance is still open");
        // Speech starts at 2.0 s; 300 ms pre-roll is kept.
        let start_s = chunks[0].start_sample as f32 / SAMPLE_RATE as f32;
        assert!((1.6..=1.75).contains(&start_s), "{start_s}");
        let tail = c.flush().expect("open utterance flushed");
        assert!(tail.start_sample > chunks[0].start_sample + chunks[0].samples.len() as u64 - 1);
        assert!(c.flush().is_none());
    }

    #[test]
    fn silence_and_clicks_produce_nothing() {
        let mut c = Chunker::default();
        let mut chunks = c.push(&tone(3.0, 0.0));
        chunks.extend(c.push(&tone(0.06, 0.5)));
        chunks.extend(c.push(&tone(2.0, 0.0)));
        assert!(chunks.is_empty());
        assert!(c.flush().is_none());
    }

    #[test]
    fn noisy_room_is_learned_and_speech_still_detected() {
        // Constant background at RMS ~0.05 (a loud laptop mic) must not be one
        // endless utterance; speech on top of it must still be cut out.
        let mut c = Chunker::default();
        let mut chunks = c.push(&tone(8.0, 0.07));
        assert!(chunks.is_empty(), "background noise alone produced a chunk");
        let speech: Vec<f32> = tone(1.5, 0.4);
        chunks.extend(c.push(&speech));
        chunks.extend(c.push(&tone(1.5, 0.07)));
        assert_eq!(chunks.len(), 1);
        let start_s = chunks[0].start_sample as f32 / SAMPLE_RATE as f32;
        assert!((7.6..=8.0).contains(&start_s), "{start_s}");
    }

    #[test]
    fn long_monologue_is_capped() {
        let mut c = Chunker::default();
        let chunks = c.push(&tone(40.0, 0.3));
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].samples.len() <= MAX_UTTERANCE);
    }

    #[test]
    fn non_speech_markers_are_filtered() {
        assert!(is_non_speech("[BLANK_AUDIO]"));
        assert!(is_non_speech("(upbeat music)"));
        assert!(is_non_speech("♪ ♪"));
        assert!(!is_non_speech("Let's start."));
    }
}
