//! macOS capture: microphone through CoreAudio (cpal), system audio through
//! ScreenCaptureKit (macOS 13+, needs the Screen & System Audio Recording
//! permission). ScreenCaptureKit resamples to 16 kHz mono for us.

use std::sync::{atomic::AtomicBool, Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::{anyhow, Context, Result};
use screencapturekit::prelude::*;
use tokio::sync::mpsc::UnboundedSender;

use super::cpal_input::{self, GapFiller};
use super::{AudioMsg, DeviceInfo, SAMPLE_RATE};
use crate::transcript::Speaker;

pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    cpal_input::list_inputs(&cpal::default_host())
}

pub fn spawn_mic(
    device: Option<String>,
    stop: Arc<AtomicBool>,
    tx: UnboundedSender<AudioMsg>,
) -> Result<JoinHandle<()>> {
    cpal_input::spawn_mic(device, stop, tx)
}

pub fn spawn_system(
    _device: Option<String>,
    stop: Arc<AtomicBool>,
    tx: UnboundedSender<AudioMsg>,
) -> Result<JoinHandle<()>> {
    cpal_input::spawn_owner(
        "capture-system",
        stop,
        move || {
            let content = SCShareableContent::get().map_err(|e| {
                anyhow!("system audio needs the Screen & System Audio Recording permission: {e}")
            })?;
            let display = content.displays().into_iter().next().context("no display found")?;
            let filter = SCContentFilter::create()
                .with_display(&display)
                .with_excluding_windows(&[])
                .build()
                .map_err(|e| anyhow!("creating capture filter: {e}"))?;
            // Video is required by the API; keep it tiny and slow.
            let config = SCStreamConfiguration::new()
                .with_width(2)
                .with_height(2)
                .with_minimum_frame_interval(&CMTime::new(1, 1))
                .with_captures_audio(true)
                .with_excludes_current_process_audio(true)
                .with_sample_rate(SAMPLE_RATE as i32)
                .with_channel_count(1);
            let mut stream = SCStream::new(&filter, &config)
                .map_err(|e| anyhow!("creating system audio stream: {e}"))?;
            let gaps = Mutex::new(GapFiller::new());
            stream
                .add_output_handler(
                    move |sample: CMSampleBuffer, kind: SCStreamOutputType| {
                        if !matches!(kind, SCStreamOutputType::Audio) {
                            return;
                        }
                        let Ok(list) = sample.audio_buffer_list() else { return };
                        let mut samples = Vec::new();
                        for buffer in &list {
                            samples.extend(
                                buffer
                                    .data()
                                    .chunks_exact(4)
                                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                            );
                        }
                        if samples.is_empty() {
                            return;
                        }
                        let samples = match gaps.lock() {
                            Ok(mut gaps) => gaps.fill(samples),
                            Err(_) => samples,
                        };
                        let _ = tx.send(AudioMsg::Frame { speaker: Speaker::Them, samples });
                    },
                    SCStreamOutputType::Audio,
                )
                .map_err(|e| anyhow!("registering audio handler: {e}"))?;
            stream.start_capture().map_err(|e| anyhow!("starting system audio capture: {e}"))?;
            Ok(stream)
        },
        |stream| {
            let _ = stream.stop_capture();
        },
    )
}
