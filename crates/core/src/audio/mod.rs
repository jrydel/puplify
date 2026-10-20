//! Audio capture. Every source is delivered as 16 kHz mono f32 frames tagged
//! with the speaker it represents (microphone = Me, system audio = Them).

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::JoinHandle;

use anyhow::{bail, Result};
use serde::Serialize;
use tokio::sync::mpsc::UnboundedSender;

use crate::{config::CaptureConfig, transcript::Speaker};

mod resample;
pub use resample::Resampler;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod cpal_input;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows as platform;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
compile_error!("puplify-core supports Linux, macOS and Windows only");

/// Sample rate of every frame delivered by the capture layer.
pub const SAMPLE_RATE: u32 = 16_000;

#[derive(Debug)]
pub enum AudioMsg {
    Frame { speaker: Speaker, samples: Vec<f32> },
    /// A source failed after it started; it delivers no further frames.
    Error { speaker: Speaker, message: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    /// Value to put in `mic_device` / `system_device`.
    pub id: String,
    pub name: String,
    /// Monitor of an output device (system audio) rather than an input.
    pub is_monitor: bool,
}

/// Lists capture devices usable in the config.
pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    platform::list_devices()
}

/// Running capture; dropping or calling [`Capture::stop`] stops all sources.
pub struct Capture {
    stop: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
}

impl Capture {
    /// Starts every source enabled in `cfg`. Returns only after each source
    /// opened successfully, so device/permission errors surface here.
    pub fn start(cfg: &CaptureConfig, tx: UnboundedSender<AudioMsg>) -> Result<Self> {
        if !cfg.mic && !cfg.system {
            bail!("enable at least one audio source (microphone or system audio)");
        }
        let mut capture = Self { stop: Arc::new(AtomicBool::new(false)), workers: Vec::new() };
        if cfg.mic {
            let worker =
                platform::spawn_mic(cfg.mic_device.clone(), capture.stop.clone(), tx.clone())?;
            capture.workers.push(worker);
        }
        if cfg.system {
            let worker =
                platform::spawn_system(cfg.system_device.clone(), capture.stop.clone(), tx)?;
            capture.workers.push(worker);
        }
        Ok(capture)
    }

    /// Stops all sources and waits for their threads; the frame sender is
    /// dropped afterwards, which ends the receiving stream.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Downmixes interleaved frames to mono.
pub(crate) fn downmix(interleaved: &[f32], channels: usize, out: &mut Vec<f32>) {
    if channels <= 1 {
        out.extend_from_slice(interleaved);
        return;
    }
    let scale = 1.0 / channels as f32;
    out.extend(interleaved.chunks_exact(channels).map(|f| f.iter().sum::<f32>() * scale));
}
