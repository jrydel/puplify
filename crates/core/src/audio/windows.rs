//! Windows capture through WASAPI (cpal): the microphone as a normal input,
//! system audio by opening an output device as an input, which WASAPI turns
//! into loopback recording of everything that device plays.

use std::sync::{atomic::AtomicBool, Arc};
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait};
use tokio::sync::mpsc::UnboundedSender;

use super::cpal_input;
use super::{AudioMsg, DeviceInfo};
use crate::transcript::Speaker;

/// Inputs plus output devices; outputs are flagged as monitors because they
/// are captured in loopback mode.
pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    let host = cpal::default_host();
    let mut out = cpal_input::list_inputs(&host)?;
    for device in host.output_devices().context("listing output devices")? {
        if let Some(info) = cpal_input::describe(&device, true) {
            out.push(info);
        }
    }
    Ok(out)
}

pub fn spawn_mic(
    device: Option<String>,
    stop: Arc<AtomicBool>,
    tx: UnboundedSender<AudioMsg>,
) -> Result<JoinHandle<()>> {
    cpal_input::spawn_mic(device, stop, tx)
}

pub fn spawn_system(
    device: Option<String>,
    stop: Arc<AtomicBool>,
    tx: UnboundedSender<AudioMsg>,
) -> Result<JoinHandle<()>> {
    cpal_input::spawn_device(
        Speaker::Them,
        "capture-system",
        move |host| {
            let device = match device {
                Some(id) => cpal_input::device_by_id(host, &id)?,
                None => host.default_output_device().context("no default output device")?,
            };
            let config = device.default_output_config().context("querying output format")?;
            Ok((device, config))
        },
        stop,
        tx,
    )
}
