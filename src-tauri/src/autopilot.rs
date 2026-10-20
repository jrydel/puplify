//! Calendar autopilot: starts a recording when an online meeting from the
//! user's calendar is in progress and another app is using the microphone
//! (they joined the call), and stops it when they leave or the meeting is over.

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

use crate::recorder::{self, AppState, Origin};

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
    /// Since when the meeting app has released the mic during our recording.
    released_since: Option<Instant>,
}

impl Pilot {
    async fn tick(&mut self, app: &AppHandle) {
        let Ok(cfg) = Config::load() else { return };
        let enabled = cfg.calendar.mode != AutoRecord::Off && (cfg.calendar.google || cfg.calendar.microsoft);
        if !enabled {
            self.meetings.clear();
            self.fetched = None;
            return;
        }
        if self.fetched.is_none_or(|t| t.elapsed() >= REFRESH) {
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
        let Origin::Calendar(meeting) = &active.origin else {
            return true; // manual recordings are the user's to stop
        };
        let now = Utc::now();
        // Only meaningful where our own capture is excluded from the reading.
        let left_call = mic.ignores_self
            && if mic.in_use {
                self.released_since = None;
                false
            } else {
                self.released_since.get_or_insert_with(Instant::now).elapsed() >= LEFT_CALL_AFTER
            };
        let over = now > meeting.end
            && (active.last_speech.elapsed() >= QUIET_AFTER_END || now > meeting.end + HARD_STOP_AFTER_END);
        tracing::debug!(
            in_use = mic.in_use,
            apps = ?mic.apps,
            released_for = ?self.released_since.map(|t| t.elapsed()),
            over,
            "autopilot: recording from calendar"
        );
        if !(left_call || over) {
            return true;
        }
        let title = meeting.title.clone();
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
        let Some(meeting) = self
            .meetings
            .iter()
            .find(|m| m.start - EARLY_JOIN <= now && now < m.end && !self.handled.contains(&m.id))
            .cloned()
        else {
            return;
        };
        self.handled.insert(meeting.id.clone());
        match cfg.calendar.mode {
            AutoRecord::Auto => {
                match recorder::start(app, meeting.title.clone(), Origin::Calendar(meeting.clone())).await {
                    Ok(_) => notify(app, "Recording started", &format!("“{}”: Puplify is transcribing.", meeting.title)),
                    Err(e) => notify(app, "Couldn't start recording", &format!("“{}”: {e:#}", meeting.title)),
                }
            }
            AutoRecord::Ask => {
                *app.state::<AppState>().offered.lock().await = Some(meeting.clone());
                let _ = app.emit("meeting-prompt", meeting.clone());
                notify(app, &format!("“{}” has started", meeting.title), "Open Puplify to record it.");
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
