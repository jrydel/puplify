//! Tray / menu-bar icon. Closing the window keeps Puplify running here so
//! auto-record still catches calls; the menu starts or stops a recording,
//! reopens the window and quits (after saving a running recording).

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, Window, Wry,
};

use crate::autopilot::notify;
use crate::recorder::{self, Origin, RecordingInfo};

const IDLE_ICON: &[u8] = include_bytes!("../icons/tray.png");
const RECORDING_ICON: &[u8] = include_bytes!("../icons/tray-recording.png");

/// Menu items whose text follows the recording state.
pub struct Tray {
    icon: TrayIcon,
    status: MenuItem<Wry>,
    toggle: MenuItem<Wry>,
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let status = MenuItem::with_id(app, "status", "Ready to record", false, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", "Start recording", true, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "Open Puplify", true, None::<&str>)?;
    let exit = MenuItem::with_id(app, "quit", "Quit Puplify", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[&status, &PredefinedMenuItem::separator(app)?, &toggle, &open, &PredefinedMenuItem::separator(app)?, &exit],
    )?;
    let icon = TrayIconBuilder::with_id("main")
        .icon(Image::from_bytes(IDLE_ICON)?)
        .tooltip("Puplify")
        .menu(&menu)
        // Left click opens the window (Windows, macOS); Linux tray icons always show the menu.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "toggle" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move { toggle_recording(&app).await });
            }
            "open" => show_window(app),
            "quit" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move { quit(&app).await });
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_window(tray.app_handle());
            }
        })
        .build(app)?;
    app.manage(Tray { icon, status, toggle });
    Ok(())
}

/// Mirrors the recording state in the icon, tooltip and menu.
pub fn refresh(app: &AppHandle, recording: Option<&RecordingInfo>) {
    let Some(tray) = app.try_state::<Tray>() else { return };
    let (icon, tooltip, status, toggle) = match recording {
        Some(info) => (RECORDING_ICON, "Puplify · recording", format!("Recording “{}”", info.title), "Stop recording"),
        None => (IDLE_ICON, "Puplify", "Ready to record".to_string(), "Start recording"),
    };
    let result = Image::from_bytes(icon)
        .and_then(|image| tray.icon.set_icon(Some(image)))
        .and_then(|_| tray.icon.set_tooltip(Some(tooltip)))
        .and_then(|_| tray.status.set_text(status))
        .and_then(|_| tray.toggle.set_text(toggle));
    if let Err(e) = result {
        tracing::warn!("updating the tray: {e}");
    }
}

/// Brings the main window back from the tray.
pub fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Closing the window only hides it; the first time, say where Puplify went.
pub fn hide_to_tray(window: &Window) {
    static TOLD: AtomicBool = AtomicBool::new(false);
    let _ = window.hide();
    if !TOLD.swap(true, Ordering::Relaxed) {
        notify(
            window.app_handle(),
            "Puplify is still running",
            "It keeps listening for calls. Open it or quit from the tray icon.",
        );
    }
}

async fn toggle_recording(app: &AppHandle) {
    if recorder::status(app).await.is_some() {
        recorder::stop(app).await;
        return;
    }
    if let Err(e) = recorder::start(app, String::new(), Origin::Manual).await {
        notify(app, "Couldn't start recording", &format!("{e:#}"));
    }
}

/// Quits once a running recording is saved and delivered, so nothing is lost.
pub async fn quit(app: &AppHandle) {
    if let Some(saving) = recorder::stop(app).await {
        if let Some(tray) = app.try_state::<Tray>() {
            let _ = tray.status.set_text("Saving the recording, then quitting…");
        }
        let _ = saving.await;
    }
    app.exit(0);
}
