//! Detects whether another application is currently capturing from a
//! microphone, which is how Puplify notices that the user joined a call.

use anyhow::Result;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct MicActivity {
    /// Another application is currently capturing from a microphone.
    pub in_use: bool,
    /// Names of those applications when the OS reports them (may be empty).
    pub apps: Vec<String>,
    /// True when Puplify's own capture is excluded from `in_use` (Linux, Windows);
    /// false on macOS, where only "some process uses the default input" is observable.
    pub ignores_self: bool,
}

/// Reports microphone use by other applications. Blocking, well under 200 ms,
/// so it is safe to call every few seconds.
pub fn mic_activity() -> Result<MicActivity> {
    platform::mic_activity()
}

#[cfg(target_os = "linux")]
use pulse as platform;
#[cfg(target_os = "macos")]
use coreaudio as platform;
#[cfg(target_os = "windows")]
use consent_store as platform;

// ---------------------------------------------------------------------------
// Linux: PulseAudio / pipewire-pulse source outputs.
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod pulse {
    use std::cell::{Cell, RefCell};
    use std::collections::HashSet;
    use std::rc::Rc;

    use anyhow::{anyhow, bail, Context as _, Result};
    use libpulse_binding::{
        callbacks::ListResult,
        context::{Context, FlagSet as ContextFlags, State},
        mainloop::standard::{IterateResult, Mainloop},
    };

    use super::MicActivity;

    const APP_NAME: &str = "Puplify";

    pub fn mic_activity() -> Result<MicActivity> {
        let mut mainloop = Mainloop::new().context("creating PulseAudio mainloop")?;
        let mut context =
            Context::new(&mainloop, APP_NAME).context("creating PulseAudio context")?;
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
                State::Failed | State::Terminated => {
                    bail!("could not connect to PulseAudio/PipeWire")
                }
                _ => {}
            }
        }

        // Sources that are monitors of a sink carry system audio, not a microphone.
        let monitors = Rc::new(RefCell::new(HashSet::<u32>::new()));
        let done = Rc::new(Cell::new(false));
        let _op = {
            let monitors = monitors.clone();
            let done = done.clone();
            context.introspect().get_source_info_list(move |result| match result {
                ListResult::Item(info) => {
                    if info.monitor_of_sink.is_some() {
                        monitors.borrow_mut().insert(info.index);
                    }
                }
                ListResult::End | ListResult::Error => done.set(true),
            })
        };
        while !done.get() {
            iterate(&mut mainloop)?;
        }

        let own_pid = std::process::id().to_string();
        let apps = Rc::new(RefCell::new(Vec::<String>::new()));
        let streams = Rc::new(Cell::new(0usize));
        let done = Rc::new(Cell::new(false));
        let _op = {
            let monitors = monitors.clone();
            let apps = apps.clone();
            let streams = streams.clone();
            let done = done.clone();
            context.introspect().get_source_output_info_list(move |result| match result {
                ListResult::Item(info) => {
                    // A corked stream is paused and not capturing anything.
                    if info.corked || monitors.borrow().contains(&info.source) {
                        return;
                    }
                    // Volume meters (pavucontrol, desktop settings) open short
                    // peak-detect streams that are not a call.
                    if info.name.as_deref() == Some("Peak detect") {
                        return;
                    }
                    if info.proplist.get_str("application.process.id").as_deref()
                        == Some(own_pid.as_str())
                    {
                        return;
                    }
                    streams.set(streams.get() + 1);
                    if let Some(name) = info
                        .proplist
                        .get_str("application.name")
                        .filter(|n| !n.is_empty())
                    {
                        let mut apps = apps.borrow_mut();
                        if !apps.contains(&name) {
                            apps.push(name);
                        }
                    }
                }
                ListResult::End | ListResult::Error => done.set(true),
            })
        };
        while !done.get() {
            iterate(&mut mainloop)?;
        }
        context.disconnect();

        Ok(MicActivity {
            in_use: streams.get() > 0,
            apps: apps.take(),
            ignores_self: true,
        })
    }
}

// ---------------------------------------------------------------------------
// Windows: the per-app microphone consent store.
// ---------------------------------------------------------------------------

/// Pure helpers for the consent-store registry layout, compiled everywhere so
/// they are unit-tested on every platform.
#[cfg(any(target_os = "windows", test))]
mod consent {
    /// `NonPackaged` subkeys are executable paths with `\` replaced by `#`.
    pub fn nonpackaged_path(key_name: &str) -> String {
        key_name.replace('#', "\\")
    }

    /// True when the `NonPackaged` subkey names `own_exe`, compared
    /// case-insensitively and ignoring a `\\?\` verbatim prefix.
    pub fn is_own_exe(key_name: &str, own_exe: &str) -> bool {
        let strip = |p: &str| p.strip_prefix(r"\\?\").unwrap_or(p).to_lowercase();
        strip(&nonpackaged_path(key_name)) == strip(own_exe)
    }

    /// Human-readable name for a `NonPackaged` subkey: the executable's file
    /// name without extension.
    pub fn nonpackaged_label(key_name: &str) -> String {
        let path = nonpackaged_path(key_name);
        let file = path.rsplit('\\').next().unwrap_or(&path);
        match file.rsplit_once('.') {
            Some((stem, ext)) if ext.eq_ignore_ascii_case("exe") && !stem.is_empty() => {
                stem.to_string()
            }
            _ => file.to_string(),
        }
    }

    /// Human-readable name for a packaged-app subkey such as
    /// `Microsoft.WindowsCamera_8wekyb3d8bbwe`: the package name before the
    /// publisher hash.
    pub fn packaged_label(key_name: &str) -> String {
        key_name.split('_').next().unwrap_or(key_name).to_string()
    }

    /// An app holds the microphone while it has a start time and no stop time.
    pub fn in_use(last_used_start: u64, last_used_stop: u64) -> bool {
        last_used_start != 0 && last_used_stop == 0
    }
}

#[cfg(target_os = "windows")]
mod consent_store {
    use anyhow::{anyhow, Result};
    use windows_registry::{Key, CURRENT_USER};

    use super::{consent, MicActivity};

    const MICROPHONE_KEY: &str =
        r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";
    const NON_PACKAGED: &str = "NonPackaged";
    /// `HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND)`.
    const NOT_FOUND: i32 = 0x8007_0002_u32 as i32;

    pub fn mic_activity() -> Result<MicActivity> {
        let none = MicActivity { in_use: false, apps: Vec::new(), ignores_self: true };
        let root = match CURRENT_USER.open(MICROPHONE_KEY) {
            Ok(key) => key,
            // No app has ever asked for the microphone.
            Err(e) if e.code().0 == NOT_FOUND => return Ok(none),
            Err(e) => return Err(anyhow!("opening the microphone consent store: {e}")),
        };
        let own_exe = std::env::current_exe()
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();

        let mut apps = Vec::new();
        let mut push = |name: String| {
            if !apps.contains(&name) {
                apps.push(name);
            }
        };

        for name in root.keys().map_err(|e| anyhow!("listing microphone consent store: {e}"))? {
            if name == NON_PACKAGED {
                continue;
            }
            if capturing(&root, &name) {
                push(consent::packaged_label(&name));
            }
        }
        if let Ok(non_packaged) = root.open(NON_PACKAGED) {
            if let Ok(keys) = non_packaged.keys() {
                for name in keys {
                    if consent::is_own_exe(&name, &own_exe) {
                        continue;
                    }
                    if capturing(&non_packaged, &name) {
                        push(consent::nonpackaged_label(&name));
                    }
                }
            }
        }

        Ok(MicActivity { in_use: !apps.is_empty(), apps, ignores_self: true })
    }

    fn capturing(parent: &Key, name: &str) -> bool {
        let Ok(app) = parent.open(name) else { return false };
        let start = app.get_u64("LastUsedTimeStart").unwrap_or(0);
        let stop = app.get_u64("LastUsedTimeStop").unwrap_or(0);
        consent::in_use(start, stop)
    }
}

// ---------------------------------------------------------------------------
// macOS: CoreAudio default input device.
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod coreaudio {
    use std::ptr::{self, NonNull};

    use anyhow::{bail, Result};
    use objc2_core_audio::{
        kAudioDevicePropertyDeviceIsRunningSomewhere, kAudioHardwarePropertyDefaultInputDevice,
        kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
        kAudioObjectUnknown, AudioObjectGetPropertyData, AudioObjectID,
        AudioObjectPropertyAddress, AudioObjectPropertySelector,
    };

    use super::MicActivity;

    pub fn mic_activity() -> Result<MicActivity> {
        let device = read_u32(
            kAudioObjectSystemObject as AudioObjectID,
            kAudioHardwarePropertyDefaultInputDevice,
        )?;
        // Without an input device nothing can be capturing.
        let in_use = device != kAudioObjectUnknown
            && read_u32(device, kAudioDevicePropertyDeviceIsRunningSomewhere)? != 0;
        // CoreAudio does not say which process runs the device, so Puplify's own
        // capture cannot be told apart from a call.
        Ok(MicActivity { in_use, apps: Vec::new(), ignores_self: false })
    }

    fn read_u32(object: AudioObjectID, selector: AudioObjectPropertySelector) -> Result<u32> {
        let address = AudioObjectPropertyAddress {
            mSelector: selector,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let mut value: u32 = 0;
        let mut size = size_of::<u32>() as u32;
        // SAFETY: `address`, `size` and `value` are live locals, and `size`
        // matches the 4-byte buffer behind `value`.
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                NonNull::from(&address),
                0,
                ptr::null(),
                NonNull::from(&mut size),
                NonNull::from(&mut value).cast(),
            )
        };
        if status != 0 {
            bail!("reading CoreAudio property {selector:#x} failed (OSStatus {status})");
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::consent::*;

    #[test]
    fn nonpackaged_key_becomes_path() {
        assert_eq!(
            nonpackaged_path("C:#Program Files#Zoom#bin#Zoom.exe"),
            r"C:\Program Files\Zoom\bin\Zoom.exe"
        );
    }

    #[test]
    fn own_executable_is_excluded_case_insensitively() {
        let key = "C:#Users#Jiri#AppData#Local#Puplify#puplify-app.exe";
        assert!(is_own_exe(key, r"c:\users\jiri\appdata\local\puplify\PUPLIFY-APP.EXE"));
        assert!(is_own_exe(key, r"\\?\C:\Users\Jiri\AppData\Local\Puplify\puplify-app.exe"));
        assert!(!is_own_exe(key, r"C:\Program Files\Zoom\bin\Zoom.exe"));
        assert!(!is_own_exe(key, ""));
    }

    #[test]
    fn labels() {
        assert_eq!(nonpackaged_label("C:#Program Files#Zoom#bin#Zoom.exe"), "Zoom");
        assert_eq!(nonpackaged_label("C:#tools#recorder"), "recorder");
        assert_eq!(packaged_label("Microsoft.WindowsCamera_8wekyb3d8bbwe"), "Microsoft.WindowsCamera");
        assert_eq!(packaged_label("Plain"), "Plain");
    }

    #[test]
    fn in_use_needs_start_without_stop() {
        assert!(in_use(133_000_000_000, 0));
        assert!(!in_use(133_000_000_000, 133_000_000_100));
        assert!(!in_use(0, 0));
        assert!(!in_use(0, 5));
    }
}
