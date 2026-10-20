//! cpal-based capture shared by macOS (microphone) and Windows (microphone and
//! WASAPI loopback of the output device).

use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SizedSample};
use tokio::sync::mpsc::UnboundedSender;

use super::{downmix, AudioMsg, DeviceInfo, Resampler, SAMPLE_RATE};
use crate::transcript::Speaker;

const POLL: Duration = Duration::from_millis(50);

/// Runs `body` on a dedicated thread that owns the (non-`Send`) platform
/// stream until `stop` is set. Startup errors are returned to the caller.
pub(super) fn spawn_owner<G: 'static>(
    name: &str,
    stop: Arc<AtomicBool>,
    body: impl FnOnce() -> Result<G> + Send + 'static,
    on_stop: impl FnOnce(G) + Send + 'static,
) -> Result<JoinHandle<()>> {
    let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<()>>(1);
    let handle = std::thread::Builder::new().name(name.into()).spawn(move || {
        let guard = match body() {
            Ok(guard) => {
                let _ = ready_tx.send(Ok(()));
                guard
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                return;
            }
        };
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(POLL);
        }
        on_stop(guard);
    })?;
    ready_rx.recv().context("capture thread exited during startup")??;
    Ok(handle)
}

/// Keeps a source on the wall-clock timeline. Loopback and ScreenCaptureKit
/// deliver nothing while nothing plays; transcript timestamps are derived
/// from sample counts, so those gaps are filled with silence.
pub(super) struct GapFiller {
    start: Option<Instant>,
    emitted: u64,
}

/// Gaps shorter than this are normal buffering jitter.
const MIN_GAP: u64 = SAMPLE_RATE as u64 / 5;

impl GapFiller {
    pub fn new() -> Self {
        Self { start: None, emitted: 0 }
    }

    /// Returns `samples`, prefixed with silence if the source fell behind.
    pub fn fill(&mut self, samples: Vec<f32>) -> Vec<f32> {
        let start = *self.start.get_or_insert_with(Instant::now);
        let expected = (start.elapsed().as_secs_f64() * SAMPLE_RATE as f64) as u64;
        let after = self.emitted + samples.len() as u64;
        let out = if expected > after + MIN_GAP {
            let mut padded = vec![0.0; (expected - after) as usize];
            padded.extend(samples);
            padded
        } else {
            samples
        };
        self.emitted += out.len() as u64;
        out
    }
}

pub(super) fn list_inputs(host: &cpal::Host) -> Result<Vec<DeviceInfo>> {
    let mut out = Vec::new();
    for device in host.input_devices().context("listing input devices")? {
        if let Some(info) = describe(&device, false) {
            out.push(info);
        }
    }
    Ok(out)
}

pub(super) fn describe(device: &cpal::Device, is_monitor: bool) -> Option<DeviceInfo> {
    let id = device.id().ok()?;
    let name = device.description().map(|d| d.to_string()).unwrap_or_else(|_| id.to_string());
    Some(DeviceInfo { id: id.to_string(), name, is_monitor })
}

pub(super) fn device_by_id(host: &cpal::Host, id: &str) -> Result<cpal::Device> {
    let parsed = id.parse().map_err(|e| anyhow!("invalid device id {id:?}: {e:?}"))?;
    host.device_by_id(&parsed).with_context(|| format!("audio device {id} not found"))
}

/// Captures `device` (an input, or on Windows an output in loopback mode)
/// as 16 kHz mono frames for `speaker`.
pub(super) fn spawn_device(
    speaker: Speaker,
    thread_name: &str,
    select: impl FnOnce(&cpal::Host) -> Result<(cpal::Device, cpal::SupportedStreamConfig)> + Send + 'static,
    stop: Arc<AtomicBool>,
    tx: UnboundedSender<AudioMsg>,
) -> Result<JoinHandle<()>> {
    spawn_owner(
        thread_name,
        stop,
        move || {
            let host = cpal::default_host();
            let (device, supported) = select(&host)?;
            let stream = match supported.sample_format() {
                SampleFormat::F32 => build_stream::<f32>(speaker, &device, &supported, tx)?,
                SampleFormat::I16 => build_stream::<i16>(speaker, &device, &supported, tx)?,
                SampleFormat::I32 => build_stream::<i32>(speaker, &device, &supported, tx)?,
                other => return Err(anyhow!("unsupported sample format {other}")),
            };
            stream.play().context("starting audio stream")?;
            Ok(stream)
        },
        drop,
    )
}

/// Default (or configured) microphone.
pub(super) fn spawn_mic(
    device: Option<String>,
    stop: Arc<AtomicBool>,
    tx: UnboundedSender<AudioMsg>,
) -> Result<JoinHandle<()>> {
    spawn_device(
        Speaker::Me,
        "capture-mic",
        move |host| {
            let device = match device {
                Some(id) => device_by_id(host, &id)?,
                None => host.default_input_device().context("no default microphone")?,
            };
            let config = device.default_input_config().context("querying microphone format")?;
            Ok((device, config))
        },
        stop,
        tx,
    )
}

fn build_stream<T>(
    speaker: Speaker,
    device: &cpal::Device,
    supported: &cpal::SupportedStreamConfig,
    tx: UnboundedSender<AudioMsg>,
) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: cpal::FromSample<T>,
{
    let channels = supported.channels() as usize;
    let mut resampler = Resampler::new(supported.sample_rate(), SAMPLE_RATE);
    let mut gaps = GapFiller::new();
    let mut floats = Vec::new();
    let mut mono = Vec::new();
    let err_tx = tx.clone();
    let stream = device.build_input_stream::<T, _, _>(
        supported.config(),
        move |data: &[T], _| {
            floats.clear();
            floats.extend(data.iter().map(|&s| cpal::Sample::to_sample::<f32>(s)));
            mono.clear();
            downmix(&floats, channels, &mut mono);
            let mut samples = Vec::with_capacity(mono.len());
            resampler.process(&mono, &mut samples);
            if !samples.is_empty() {
                let samples = gaps.fill(samples);
                let _ = tx.send(AudioMsg::Frame { speaker, samples });
            }
        },
        move |err| {
            let _ = err_tx.send(AudioMsg::Error { speaker, message: err.to_string() });
        },
        None,
    )?;
    Ok(stream)
}
