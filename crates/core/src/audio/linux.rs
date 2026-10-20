//! Linux capture through the PulseAudio API, which PipeWire also serves via
//! `pipewire-pulse`. The server converts every source to 16 kHz mono f32.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::JoinHandle;

use anyhow::{anyhow, bail, Context as _, Result};
use libpulse_binding::{
    callbacks::ListResult,
    context::{Context, FlagSet as ContextFlags, State},
    def::BufferAttr,
    mainloop::standard::{IterateResult, Mainloop},
    sample::{Format, Spec},
    stream::Direction,
};
use libpulse_simple_binding::Simple;
use tokio::sync::mpsc::UnboundedSender;

use super::{AudioMsg, DeviceInfo, SAMPLE_RATE};
use crate::transcript::Speaker;

const APP_NAME: &str = "Verbatim";
/// Samples per read; 100 ms keeps stop latency and live-transcription delay low.
const CHUNK: usize = SAMPLE_RATE as usize / 10;

pub fn spawn_mic(
    device: Option<String>,
    stop: Arc<AtomicBool>,
    tx: UnboundedSender<AudioMsg>,
) -> Result<JoinHandle<()>> {
    spawn(Speaker::Me, device, "Microphone", stop, tx)
}

pub fn spawn_system(
    device: Option<String>,
    stop: Arc<AtomicBool>,
    tx: UnboundedSender<AudioMsg>,
) -> Result<JoinHandle<()>> {
    // Special name resolved by the server to the monitor of the default sink.
    let device = device.unwrap_or_else(|| "@DEFAULT_MONITOR@".into());
    spawn(Speaker::Them, Some(device), "System audio", stop, tx)
}

fn spawn(
    speaker: Speaker,
    device: Option<String>,
    stream_name: &'static str,
    stop: Arc<AtomicBool>,
    tx: UnboundedSender<AudioMsg>,
) -> Result<JoinHandle<()>> {
    let spec = Spec { format: Format::F32le, rate: SAMPLE_RATE, channels: 1 };
    let frag = (CHUNK * 4) as u32;
    let attr = BufferAttr {
        maxlength: u32::MAX,
        tlength: u32::MAX,
        prebuf: u32::MAX,
        minreq: u32::MAX,
        fragsize: frag,
    };
    let simple = Simple::new(
        None,
        APP_NAME,
        Direction::Record,
        device.as_deref(),
        stream_name,
        &spec,
        None,
        Some(&attr),
    )
    .map_err(|e| {
        anyhow!(
            "opening {} ({}): {}",
            stream_name.to_lowercase(),
            device.as_deref().unwrap_or("default"),
            e.to_string().unwrap_or_else(|| format!("{e:?}"))
        )
    })?;

    let handle = std::thread::Builder::new()
        .name(format!("capture-{stream_name}"))
        .spawn(move || {
            let mut bytes = vec![0u8; CHUNK * 4];
            while !stop.load(Ordering::Relaxed) {
                if let Err(e) = simple.read(&mut bytes) {
                    let message = e.to_string().unwrap_or_else(|| format!("{e:?}"));
                    let _ = tx.send(AudioMsg::Error { speaker, message });
                    return;
                }
                let samples = bytes
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .collect();
                if tx.send(AudioMsg::Frame { speaker, samples }).is_err() {
                    return; // consumer gone
                }
            }
        })?;
    Ok(handle)
}

pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    let mut mainloop = Mainloop::new().context("creating PulseAudio mainloop")?;
    let mut context = Context::new(&mainloop, APP_NAME).context("creating PulseAudio context")?;
    context
        .connect(None, ContextFlags::NOFLAGS, None)
        .map_err(|e| anyhow!("connecting to PulseAudio/PipeWire: {e:?}"))?;

    let iterate = |mainloop: &mut Mainloop| match mainloop.iterate(true) {
        IterateResult::Success(_) => Ok(()),
        IterateResult::Quit(_) => bail!("PulseAudio mainloop quit"),
        IterateResult::Err(e) => bail!("PulseAudio mainloop error: {e:?}"),
    };

    loop {
        iterate(&mut mainloop)?;
        match context.get_state() {
            State::Ready => break,
            State::Failed | State::Terminated => bail!("could not connect to PulseAudio/PipeWire"),
            _ => {}
        }
    }

    let devices = Rc::new(RefCell::new(Vec::new()));
    let done = Rc::new(Cell::new(false));
    let _op = {
        let devices = devices.clone();
        let done = done.clone();
        context.introspect().get_source_info_list(move |result| match result {
            ListResult::Item(info) => {
                let Some(id) = info.name.as_ref() else { return };
                devices.borrow_mut().push(DeviceInfo {
                    id: id.to_string(),
                    name: info
                        .description
                        .as_ref()
                        .map(|d| d.to_string())
                        .unwrap_or_else(|| id.to_string()),
                    is_monitor: info.monitor_of_sink.is_some(),
                });
            }
            ListResult::End | ListResult::Error => done.set(true),
        })
    };
    while !done.get() {
        iterate(&mut mainloop)?;
    }
    context.disconnect();
    let devices = devices.take();
    Ok(devices)
}
