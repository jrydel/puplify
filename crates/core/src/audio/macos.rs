//! macOS capture: microphone through CoreAudio (cpal), system audio through
//! ScreenCaptureKit (macOS 13+, needs the Screen & System Audio Recording
//! permission). ScreenCaptureKit resamples to 16 kHz mono for us.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SizedSample};
use screencapturekit::prelude::*;
use tokio::sync::mpsc::UnboundedSender;

use super::{downmix, AudioMsg, DeviceInfo, Resampler, SAMPLE_RATE};
use crate::transcript::Speaker;

const POLL: Duration = Duration::from_millis(50);

pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    let host = cpal::default_host();
    let mut out = Vec::new();
    for device in host.input_devices().context("listing input devices")? {
        let Ok(id) = device.id() else { continue };
        let name = device
            .description()
            .map(|d| d.to_string())
            .unwrap_or_else(|_| id.to_string());
        out.push(DeviceInfo { id: id.to_string(), name, is_monitor: false });
    }
    Ok(out)
}

/// Runs `body` on a dedicated thread that owns the (non-`Send`) platform
/// stream until `stop` is set. `body` returns the stream guard or an error,
/// which is reported back before this function returns.
fn spawn_owner<G: 'static>(
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

pub fn spawn_mic(
    device: Option<String>,
    stop: Arc<AtomicBool>,
    tx: UnboundedSender<AudioMsg>,
) -> Result<JoinHandle<()>> {
    spawn_owner(
        "capture-mic",
        stop,
        move || {
            let host = cpal::default_host();
            let device = match device {
                Some(id) => {
                    let parsed = id.parse().map_err(|e| anyhow!("invalid device id {id:?}: {e:?}"))?;
                    host.device_by_id(&parsed).with_context(|| format!("input device {id} not found"))?
                }
                None => host.default_input_device().context("no default microphone")?,
            };
            let supported = device.default_input_config().context("querying microphone format")?;
            let stream = match supported.sample_format() {
                SampleFormat::F32 => build_mic_stream::<f32>(&device, &supported, tx)?,
                SampleFormat::I16 => build_mic_stream::<i16>(&device, &supported, tx)?,
                SampleFormat::I32 => build_mic_stream::<i32>(&device, &supported, tx)?,
                other => return Err(anyhow!("unsupported microphone sample format {other}")),
            };
            stream.play().context("starting microphone")?;
            Ok(stream)
        },
        drop,
    )
}

fn build_mic_stream<T>(
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
                let _ = tx.send(AudioMsg::Frame { speaker: Speaker::Me, samples });
            }
        },
        move |err| {
            let _ = err_tx.send(AudioMsg::Error { speaker: Speaker::Me, message: err.to_string() });
        },
        None,
    )?;
    Ok(stream)
}

pub fn spawn_system(
    _device: Option<String>,
    stop: Arc<AtomicBool>,
    tx: UnboundedSender<AudioMsg>,
) -> Result<JoinHandle<()>> {
    spawn_owner(
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
                        if !samples.is_empty() {
                            let _ = tx.send(AudioMsg::Frame { speaker: Speaker::Them, samples });
                        }
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
