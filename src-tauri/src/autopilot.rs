//! Autopilot: notices when another app starts using the microphone (the user
//! joined a call) and records it, or asks first. Online meetings from the
//! watched calendars give the recording its title and scheduled end; other
//! calls are handled by their own setting. Recordings stop when the user
//! leaves the call, the meeting is over, or nobody has spoken for a while.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use chrono::Utc;
use puplify_core::{
    audio::activity::{mic_activity, MicActivity},
    calendar::{self, Meeting},
    config::{AutoRecord, Config},
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

use crate::recorder::{self, AppState, Call, Origin};

const TICK: Duration = Duration::from_secs(10);
const REFRESH: Duration = Duration::from_secs(60);
/// How far ahead the calendar is read.
pub const AHEAD_HOURS: i64 = 12;
/// Joining a little early still counts.
const EARLY_JOIN: chrono::Duration = chrono::Duration::minutes(5);
/// The meeting app has to release the mic this long before we stop.
const LEFT_CALL_AFTER: Duration = Duration::from_secs(45);
/// After the scheduled end, stop once nobody has spoken for this long…
const QUIET_AFTER_END: Duration = Duration::from_secs(60);
/// …and in any case this long after the scheduled end.
const HARD_STOP_AFTER_END: chrono::Duration = chrono::Duration::minutes(15);
/// A call without a calendar event stops after this long without speech; the
/// only stop signal on macOS, where our own capture keeps the mic "in use".
const CALL_QUIET: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Serialize)]
pub struct CalendarSnapshot {
    pub meetings: Vec<Meeting>,
    pub warnings: Vec<String>,
}

pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut pilot = Pilot::default();
        loop {
            pilot.tick(&app).await;
            tokio::time::sleep(TICK).await;
        }
    });
}

#[derive(Default)]
struct Pilot {
    meetings: Vec<Meeting>,
    fetched: Option<Instant>,
    /// Meetings already started, offered or skipped; never acted on twice.
    handled: HashSet<String>,
    /// The current spell of microphone use was already recorded, offered or
    /// skipped; cleared once the mic is free again.
    call_handled: bool,
    /// Since when the meeting app has released the mic during our recording.
    released_since: Option<Instant>,
}

impl Pilot {
    async fn tick(&mut self, app: &AppHandle) {
        let Ok(cfg) = Config::load() else { return };
        let c = &cfg.calendar;
        let watch_calendar = c.mode != AutoRecord::Off
            && (c.google || c.microsoft || c.links.iter().any(|l| !l.trim().is_empty()));
        if !watch_calendar {
            self.meetings.clear();
            self.fetched = None;
        } else if self.fetched.is_none_or(|t| t.elapsed() >= REFRESH) {
            let (meetings, warnings) = calendar::upcoming(&cfg, chrono::Duration::hours(AHEAD_HOURS)).await;
            for w in &warnings {
                tracing::warn!("calendar: {w}");
            }
            self.meetings = meetings.clone();
            self.fetched = Some(Instant::now());
            let _ = app.emit("calendar", CalendarSnapshot { meetings, warnings });
        }

        let mic = match tokio::task::spawn_blocking(mic_activity).await {
            Ok(Ok(mic)) => mic,
            Ok(Err(e)) => {
                tracing::warn!("microphone activity: {e:#}");
                return;
            }
            Err(_) => return,
        };
        if !mic.in_use {
            self.call_handled = false;
        }

        if self.maybe_stop(app, &mic).await {
            return;
        }
        self.maybe_start(app, &cfg, &mic).await;
    }

    /// Returns true while a recording is active (so nothing new starts).
    async fn maybe_stop(&mut self, app: &AppHandle, mic: &MicActivity) -> bool {
        let state = app.state::<AppState>();
        let guard = state.active.lock().await;
        let Some(active) = guard.as_ref() else {
            self.released_since = None;
            return false;
        };
        // Whatever is recording covers the call in progress; don't offer it again afterwards.
        if mic.in_use {
            self.call_handled = true;
        }
        let Origin::Call(call) = &active.origin else {
            return true; // manual recordings are the user's to stop
        };
        // Only meaningful where our own capture is excluded from the reading.
        let left_call = mic.ignores_self
            && if mic.in_use {
                self.released_since = None;
                false
            } else {
                self.released_since.get_or_insert_with(Instant::now).elapsed() >= LEFT_CALL_AFTER
            };
        let quiet = active.last_speech.elapsed();
        let over = match &call.meeting {
            Some(meeting) => {
                let now = Utc::now();
                now > meeting.end && (quiet >= QUIET_AFTER_END || now > meeting.end + HARD_STOP_AFTER_END)
            }
            None => quiet >= CALL_QUIET,
        };
        tracing::debug!(
            in_use = mic.in_use,
            apps = ?mic.apps,
            released_for = ?self.released_since.map(|t| t.elapsed()),
            over,
            "autopilot: recording a call"
        );
        if !(left_call || over) {
            return true;
        }
        let title = call.title.clone();
        drop(guard);
        self.released_since = None;
        recorder::stop(app).await;
        notify(app, "Recording stopped", &format!("“{title}” is saved and on its way to your destinations."));
        true
    }

    async fn maybe_start(&mut self, app: &AppHandle, cfg: &Config, mic: &MicActivity) {
        if !mic.in_use {
            return;
        }
        let now = Utc::now();
        let app_name = mic.apps.first().cloned();
        let mut current = self.meetings.iter().filter(|m| m.start - EARLY_JOIN <= now && now < m.end);
        let (call, mode) = match current.clone().find(|m| !self.handled.contains(&m.id)) {
            Some(meeting) => {
                self.handled.insert(meeting.id.clone());
                let call = Call { title: meeting.title.clone(), meeting: Some(meeting.clone()), app: app_name };
                (call, cfg.calendar.mode)
            }
            // A meeting under way was already dealt with; this is the same call.
            None if current.next().is_some() => return,
            None if self.call_handled => return,
            None => {
                let title = app_name.as_deref().map_or_else(|| "Call".to_string(), |a| format!("Call in {a}"));
                (Call { title, meeting: None, app: app_name }, cfg.calendar.calls)
            }
        };
        self.call_handled = true;
        match mode {
            AutoRecord::Auto => match recorder::start(app, call.title.clone(), Origin::Call(call.clone())).await {
                Ok(_) => notify(app, "Recording started", &format!("“{}”: Puplify is transcribing.", call.title)),
                Err(e) => notify(app, "Couldn't start recording", &format!("“{}”: {e:#}", call.title)),
            },
            AutoRecord::Ask => {
                *app.state::<AppState>().offered.lock().await = Some(call.clone());
                let _ = app.emit("call-prompt", call.clone());
                let (heading, body) = match &call.meeting {
                    Some(m) => (format!("“{}” has started", m.title), "Open Puplify to record it."),
                    None => (format!("{} started", call.title), "Your microphone is in use. Open Puplify to record the call."),
                };
                notify(app, &heading, body);
            }
            AutoRecord::Off => {}
        }
    }
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        tracing::warn!("notification: {e}");
    }
}
