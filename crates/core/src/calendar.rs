//! Online meetings from the connected Google and Microsoft calendars. The
//! desktop app combines this with microphone activity to start recordings
//! automatically.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, NaiveDateTime, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::config::Config;
use crate::connectors::oauth::{http, http_error};
use crate::connectors::{google, microsoft};

/// How far back events are still listed, so a meeting that already started is found.
const LOOKBACK: Duration = Duration::hours(1);
const GOOGLE_EVENTS: &str = "https://www.googleapis.com/calendar/v3/calendars/primary/events";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CalendarProvider {
    Google,
    Microsoft,
}

#[derive(Debug, Clone, Serialize)]
pub struct Meeting {
    /// Provider-prefixed event id, stable per occurrence of a recurring meeting.
    pub id: String,
    pub title: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub join_url: Option<String>,
    pub provider: CalendarProvider,
}

/// Online meetings (`join_url` is `Some`) overlapping `[now - 1 h, now + ahead]`
/// from every enabled and connected calendar, excluding cancelled,
/// declined-by-me and all-day events, sorted by start. Provider failures become
/// warnings so one broken account does not hide the other calendar.
pub async fn upcoming(cfg: &Config, ahead: Duration) -> (Vec<Meeting>, Vec<String>) {
    // Development aid: lets the autopilot be exercised without real calendar
    // accounts. Compiled out of release builds.
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("PUPLIFY_CALENDAR_FIXTURE") {
        return fixture(std::path::Path::new(&path));
    }
    let now = Utc::now();
    let (from, to) = (now - LOOKBACK, now + ahead);
    let client = match http() {
        Ok(client) => client,
        Err(e) => return (Vec::new(), vec![format!("calendar: {e:#}")]),
    };
    let google = async {
        if !cfg.calendar.google {
            return None;
        }
        Some(match google::status() {
            Ok(Some(_)) => fetch_google(cfg, &client, from, to).await,
            Ok(None) => Err(anyhow::anyhow!("calendar is enabled but no Google account is connected")),
            Err(e) => Err(e),
        })
    };
    let microsoft = async {
        if !cfg.calendar.microsoft {
            return None;
        }
        Some(match microsoft::status() {
            Ok(Some(_)) => fetch_microsoft(cfg, &client, from, to).await,
            Ok(None) => Err(anyhow::anyhow!("calendar is enabled but no Microsoft account is connected")),
            Err(e) => Err(e),
        })
    };
    let (google, microsoft) = futures_util::future::join(google, microsoft).await;

    let mut meetings = Vec::new();
    let mut warnings = Vec::new();
    for (name, result) in [("Google Calendar", google), ("Microsoft Calendar", microsoft)] {
        match result {
            Some(Ok(found)) => meetings.extend(found),
            Some(Err(e)) => warnings.push(format!("{name}: {e:#}")),
            None => {}
        }
    }
    meetings.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| a.id.cmp(&b.id)));
    (meetings, warnings)
}

/// Debug builds only: `[{"title": "…", "start": "<RFC 3339>", "end": "<RFC 3339>"}]`.
#[cfg(debug_assertions)]
fn fixture(path: &std::path::Path) -> (Vec<Meeting>, Vec<String>) {
    #[derive(serde::Deserialize)]
    struct Entry {
        title: String,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    }
    let parsed = std::fs::read(path)
        .map_err(anyhow::Error::from)
        .and_then(|bytes| Ok(serde_json::from_slice::<Vec<Entry>>(&bytes)?));
    match parsed {
        Ok(entries) => (
            entries
                .into_iter()
                .enumerate()
                .map(|(i, e)| Meeting {
                    id: format!("fixture:{i}"),
                    title: e.title,
                    start: e.start,
                    end: e.end,
                    join_url: Some("https://meet.google.com/fixture".into()),
                    provider: CalendarProvider::Google,
                })
                .collect(),
            Vec::new(),
        ),
        Err(e) => (Vec::new(), vec![format!("calendar fixture {}: {e:#}", path.display())]),
    }
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

async fn fetch_google(cfg: &Config, client: &reqwest::Client, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<Meeting>> {
    let token = google::access_token(&cfg.accounts.google, client).await?;
    let resp = client
        .get(GOOGLE_EVENTS)
        .query(&[
            ("timeMin", rfc3339(from).as_str()),
            ("timeMax", rfc3339(to).as_str()),
            ("singleEvents", "true"),
            ("orderBy", "startTime"),
            ("maxResults", "50"),
        ])
        .bearer_auth(token)
        .send()
        .await?;
    if !resp.status().is_success() {
        let forbidden = resp.status() == reqwest::StatusCode::FORBIDDEN;
        let err = http_error(resp).await;
        if forbidden {
            bail!("{err} (sign in again to grant calendar access, and check that the Google Calendar API is enabled for your project)");
        }
        bail!("{err}");
    }
    let body: Value = resp.json().await.context("unexpected Google Calendar response")?;
    Ok(google_meetings(&body))
}

async fn fetch_microsoft(cfg: &Config, client: &reqwest::Client, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<Meeting>> {
    let token = microsoft::access_token(&cfg.accounts.microsoft, client).await?;
    // `$`-prefixed OData options are kept literal; Graph does not reliably accept them percent-encoded.
    let url = format!(
        "{}/me/calendarView?startDateTime={}&endDateTime={}&$select=id,subject,start,end,isAllDay,isCancelled,responseStatus,onlineMeeting,onlineMeetingUrl,location,bodyPreview&$top=50",
        microsoft::GRAPH,
        urlencoding::encode(&rfc3339(from)),
        urlencoding::encode(&rfc3339(to)),
    );
    let resp = client
        .get(url)
        .bearer_auth(token)
        .header("Prefer", "outlook.timezone=\"UTC\"")
        .send()
        .await?;
    if !resp.status().is_success() {
        let forbidden = resp.status() == reqwest::StatusCode::FORBIDDEN;
        let err = http_error(resp).await;
        if forbidden {
            bail!("{err} (sign in again to grant calendar access, and check that the app registration has the Calendars.Read permission)");
        }
        bail!("{err}");
    }
    let body: Value = resp.json().await.context("unexpected Microsoft Graph response")?;
    Ok(graph_meetings(&body))
}

/// Maps a Google Calendar `events.list` response to online meetings.
fn google_meetings(body: &Value) -> Vec<Meeting> {
    let Some(items) = body["items"].as_array() else { return Vec::new() };
    items.iter().filter_map(google_meeting).collect()
}

fn google_meeting(event: &Value) -> Option<Meeting> {
    if event["status"].as_str() == Some("cancelled") {
        return None;
    }
    let declined = event["attendees"]
        .as_array()
        .is_some_and(|a| a.iter().any(|p| p["self"].as_bool() == Some(true) && p["responseStatus"].as_str() == Some("declined")));
    if declined {
        return None;
    }
    // All-day events carry `start.date` instead of `start.dateTime`.
    let start = DateTime::parse_from_rfc3339(event["start"]["dateTime"].as_str()?).ok()?.to_utc();
    let end = DateTime::parse_from_rfc3339(event["end"]["dateTime"].as_str()?).ok()?.to_utc();
    let join_url = event["hangoutLink"]
        .as_str()
        .filter(|u| !u.is_empty())
        .map(str::to_string)
        .or_else(|| {
            event["conferenceData"]["entryPoints"]
                .as_array()?
                .iter()
                .find(|p| p["entryPointType"].as_str() == Some("video"))
                .and_then(|p| p["uri"].as_str())
                .filter(|u| !u.is_empty())
                .map(str::to_string)
        })
        .or_else(|| event["location"].as_str().and_then(find_join_url))
        .or_else(|| event["description"].as_str().and_then(find_join_url))?;
    Some(Meeting {
        id: format!("google:{}", event["id"].as_str()?),
        title: title(event["summary"].as_str()),
        start,
        end,
        join_url: Some(join_url),
        provider: CalendarProvider::Google,
    })
}

/// Maps a Microsoft Graph `calendarView` response (requested with
/// `Prefer: outlook.timezone="UTC"`) to online meetings.
fn graph_meetings(body: &Value) -> Vec<Meeting> {
    let Some(items) = body["value"].as_array() else { return Vec::new() };
    items.iter().filter_map(graph_meeting).collect()
}

fn graph_meeting(event: &Value) -> Option<Meeting> {
    if event["isCancelled"].as_bool() == Some(true)
        || event["isAllDay"].as_bool() == Some(true)
        || event["responseStatus"]["response"].as_str() == Some("declined")
    {
        return None;
    }
    let start = graph_time(&event["start"])?;
    let end = graph_time(&event["end"])?;
    let join_url = event["onlineMeeting"]["joinUrl"]
        .as_str()
        .filter(|u| !u.is_empty())
        .or_else(|| event["onlineMeetingUrl"].as_str().filter(|u| !u.is_empty()))
        .map(str::to_string)
        .or_else(|| event["location"]["displayName"].as_str().and_then(find_join_url))
        .or_else(|| event["bodyPreview"].as_str().and_then(find_join_url))?;
    Some(Meeting {
        id: format!("microsoft:{}", event["id"].as_str()?),
        title: title(event["subject"].as_str()),
        start,
        end,
        join_url: Some(join_url),
        provider: CalendarProvider::Microsoft,
    })
}

/// Graph `dateTimeTimeZone`: with the UTC preference the `dateTime` has no
/// offset (`2026-10-20T14:00:00.0000000`) and `timeZone` is `UTC`.
fn graph_time(value: &Value) -> Option<DateTime<Utc>> {
    let text = value["dateTime"].as_str()?;
    if let Ok(t) = DateTime::parse_from_rfc3339(text) {
        return Some(t.to_utc());
    }
    if let Some(zone) = value["timeZone"].as_str() {
        if !zone.eq_ignore_ascii_case("UTC") {
            tracing::warn!("ignoring Microsoft calendar event in time zone {zone}; expected UTC");
            return None;
        }
    }
    Some(NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f").ok()?.and_utc())
}

fn title(raw: Option<&str>) -> String {
    match raw.map(str::trim) {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => "(No title)".to_string(),
    }
}

/// First video-call link (Google Meet, Zoom, Teams, Webex) in free text or HTML.
fn find_join_url(text: &str) -> Option<String> {
    let text = text.replace("&amp;", "&");
    let mut from = 0;
    while let Some(i) = text[from..].find("http") {
        let start = from + i;
        from = start + 4;
        let rest = &text[start..];
        if !(rest.starts_with("https://") || rest.starts_with("http://")) {
            continue;
        }
        let end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}' | '\\' | '|'))
            .unwrap_or(rest.len());
        let url = rest[..end].trim_end_matches(['.', ',', ';', ':', '!', '?']);
        if is_join_url(url) {
            return Some(url.to_string());
        }
    }
    None
}

fn is_join_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")) else { return false };
    let (authority, path) = rest.split_at(rest.find(['/', '?', '#']).unwrap_or(rest.len()));
    let host = authority.rsplit('@').next().and_then(|h| h.split(':').next()).unwrap_or_default().to_ascii_lowercase();
    match host.as_str() {
        "meet.google.com" => path.len() > 1 && path.starts_with('/'),
        "teams.microsoft.com" => path.starts_with("/l/meetup-join") || path.starts_with("/meet/"),
        "teams.live.com" => path.starts_with("/meet"),
        h if h == "zoom.us" || h.ends_with(".zoom.us") => path.starts_with("/j/"),
        h => h.ends_with(".webex.com") && path.len() > 1,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().to_utc()
    }

    #[test]
    fn finds_supported_join_urls() {
        for url in [
            "https://meet.google.com/abc-defg-hij",
            "https://us02web.zoom.us/j/81234567890?pwd=AbC123",
            "https://zoom.us/j/123",
            "https://teams.microsoft.com/l/meetup-join/19%3ameeting_abc%40thread.v2/0?context=%7b%7d",
            "https://teams.microsoft.com/meet/9876543210?p=xyz",
            "https://teams.live.com/meet/9300000000000",
            "https://acme.webex.com/acme/j.php?MTID=m123",
        ] {
            assert_eq!(find_join_url(&format!("Join here: {url}.")).as_deref(), Some(url), "{url}");
        }
    }

    #[test]
    fn join_url_scan_handles_html_entities_and_skips_other_links() {
        let html = r#"<p>Agenda: <a href="https://docs.example.com/d/1">doc</a></p>
            <a href="https://us02web.zoom.us/j/81234567890?pwd=a&amp;from=addon">Join</a>"#;
        assert_eq!(find_join_url(html).as_deref(), Some("https://us02web.zoom.us/j/81234567890?pwd=a&from=addon"));
        assert_eq!(find_join_url("see https://example.com/zoom.us/j/1 and (https://meet.google.com/abc-defg-hij)").as_deref(), Some("https://meet.google.com/abc-defg-hij"));
    }

    #[test]
    fn join_url_scan_rejects_lookalikes() {
        for text in [
            "",
            "no links here",
            "https://meet.google.com/",
            "https://meet.google.com.evil.example/abc",
            "https://evilzoom.us/j/1",
            "https://zoom.us/pricing",
            "https://teams.microsoft.com/l/channel/1",
            "https://webex.com/",
            "http",
            "httpx://zoom.us/j/1",
        ] {
            assert_eq!(find_join_url(text), None, "{text}");
        }
    }

    #[test]
    fn google_events_map_to_meetings() {
        let body = json!({ "items": [
            { "id": "meet1", "summary": "Standup", "status": "confirmed",
              "start": { "dateTime": "2026-10-20T10:00:00+02:00" }, "end": { "dateTime": "2026-10-20T10:30:00+02:00" },
              "hangoutLink": "https://meet.google.com/abc-defg-hij",
              "attendees": [ { "email": "me@x.com", "self": true, "responseStatus": "accepted" } ] },
            { "id": "zoom1_20261020T120000Z", "summary": "  ", "status": "confirmed",
              "start": { "dateTime": "2026-10-20T12:00:00Z" }, "end": { "dateTime": "2026-10-20T13:00:00Z" },
              "conferenceData": { "entryPoints": [
                  { "entryPointType": "phone", "uri": "tel:+1555" },
                  { "entryPointType": "video", "uri": "https://us02web.zoom.us/j/1" } ] } },
            { "id": "loc1", "summary": "Teams in location",
              "start": { "dateTime": "2026-10-20T14:00:00Z" }, "end": { "dateTime": "2026-10-20T15:00:00Z" },
              "location": "Microsoft Teams Meeting https://teams.microsoft.com/l/meetup-join/abc/0" },
            { "id": "desc1", "summary": "Webex in description",
              "start": { "dateTime": "2026-10-20T16:00:00Z" }, "end": { "dateTime": "2026-10-20T17:00:00Z" },
              "description": "<b>Join</b> <a href=\"https://acme.webex.com/meet/jane\">link</a>" },
            { "id": "cancelled1", "summary": "Cancelled", "status": "cancelled",
              "start": { "dateTime": "2026-10-20T10:00:00Z" }, "end": { "dateTime": "2026-10-20T11:00:00Z" },
              "hangoutLink": "https://meet.google.com/aaa-bbbb-ccc" },
            { "id": "declined1", "summary": "Declined",
              "start": { "dateTime": "2026-10-20T10:00:00Z" }, "end": { "dateTime": "2026-10-20T11:00:00Z" },
              "hangoutLink": "https://meet.google.com/aaa-bbbb-ccc",
              "attendees": [ { "email": "other@x.com", "responseStatus": "accepted" },
                             { "email": "me@x.com", "self": true, "responseStatus": "declined" } ] },
            { "id": "others-declined", "summary": "Someone else declined",
              "start": { "dateTime": "2026-10-20T18:00:00Z" }, "end": { "dateTime": "2026-10-20T19:00:00Z" },
              "hangoutLink": "https://meet.google.com/ddd-eeee-fff",
              "attendees": [ { "email": "other@x.com", "responseStatus": "declined" } ] },
            { "id": "allday1", "summary": "Offsite https://meet.google.com/aaa-bbbb-ccc",
              "start": { "date": "2026-10-20" }, "end": { "date": "2026-10-21" },
              "hangoutLink": "https://meet.google.com/aaa-bbbb-ccc" },
            { "id": "nolink1", "summary": "Lunch",
              "start": { "dateTime": "2026-10-20T11:00:00Z" }, "end": { "dateTime": "2026-10-20T12:00:00Z" },
              "location": "Cafeteria" }
        ]});
        let meetings = google_meetings(&body);
        let ids: Vec<_> = meetings.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["google:meet1", "google:zoom1_20261020T120000Z", "google:loc1", "google:desc1", "google:others-declined"]);
        assert!(meetings.iter().all(|m| m.provider == CalendarProvider::Google));

        assert_eq!(meetings[0].title, "Standup");
        assert_eq!(meetings[0].start, utc("2026-10-20T08:00:00Z"));
        assert_eq!(meetings[0].end, utc("2026-10-20T08:30:00Z"));
        assert_eq!(meetings[0].join_url.as_deref(), Some("https://meet.google.com/abc-defg-hij"));
        assert_eq!(meetings[1].title, "(No title)");
        assert_eq!(meetings[1].join_url.as_deref(), Some("https://us02web.zoom.us/j/1"));
        assert_eq!(meetings[2].join_url.as_deref(), Some("https://teams.microsoft.com/l/meetup-join/abc/0"));
        assert_eq!(meetings[3].join_url.as_deref(), Some("https://acme.webex.com/meet/jane"));
    }

    #[test]
    fn google_response_without_items_is_empty() {
        assert!(google_meetings(&json!({ "kind": "calendar#events" })).is_empty());
    }

    #[test]
    fn graph_events_map_to_meetings() {
        let body = json!({ "value": [
            { "id": "AAMkTeams", "subject": "Design review", "isAllDay": false, "isCancelled": false,
              "responseStatus": { "response": "accepted" },
              "start": { "dateTime": "2026-10-20T14:00:00.0000000", "timeZone": "UTC" },
              "end": { "dateTime": "2026-10-20T14:45:00.0000000", "timeZone": "UTC" },
              "onlineMeeting": { "joinUrl": "https://teams.microsoft.com/l/meetup-join/xyz/0" },
              "onlineMeetingUrl": null, "location": { "displayName": "" }, "bodyPreview": "" },
            { "id": "AAMkLegacy", "subject": "Legacy", "isAllDay": false, "isCancelled": false,
              "responseStatus": { "response": "organizer" },
              "start": { "dateTime": "2026-10-20T09:00:00.0000000", "timeZone": "UTC" },
              "end": { "dateTime": "2026-10-20T09:30:00.0000000", "timeZone": "UTC" },
              "onlineMeeting": null, "onlineMeetingUrl": "https://teams.microsoft.com/l/meetup-join/legacy/0" },
            { "id": "AAMkZoom", "subject": "Zoom call", "isAllDay": false, "isCancelled": false,
              "responseStatus": { "response": "tentativelyAccepted" },
              "start": { "dateTime": "2026-10-20T11:00:00.0000000", "timeZone": "UTC" },
              "end": { "dateTime": "2026-10-20T12:00:00.0000000", "timeZone": "UTC" },
              "onlineMeeting": null, "location": { "displayName": "Zoom" },
              "bodyPreview": "Join Zoom Meeting https://us02web.zoom.us/j/555?pwd=q Meeting ID: 555" },
            { "id": "AAMkCancelled", "subject": "Cancelled", "isAllDay": false, "isCancelled": true,
              "start": { "dateTime": "2026-10-20T14:00:00.0000000", "timeZone": "UTC" },
              "end": { "dateTime": "2026-10-20T15:00:00.0000000", "timeZone": "UTC" },
              "onlineMeeting": { "joinUrl": "https://teams.microsoft.com/l/meetup-join/c/0" } },
            { "id": "AAMkDeclined", "subject": "Declined", "isAllDay": false, "isCancelled": false,
              "responseStatus": { "response": "declined" },
              "start": { "dateTime": "2026-10-20T14:00:00.0000000", "timeZone": "UTC" },
              "end": { "dateTime": "2026-10-20T15:00:00.0000000", "timeZone": "UTC" },
              "onlineMeeting": { "joinUrl": "https://teams.microsoft.com/l/meetup-join/d/0" } },
            { "id": "AAMkAllDay", "subject": "Holiday", "isAllDay": true, "isCancelled": false,
              "start": { "dateTime": "2026-10-20T00:00:00.0000000", "timeZone": "UTC" },
              "end": { "dateTime": "2026-10-21T00:00:00.0000000", "timeZone": "UTC" },
              "onlineMeeting": { "joinUrl": "https://teams.microsoft.com/l/meetup-join/h/0" } },
            { "id": "AAMkOffice", "subject": "In person", "isAllDay": false, "isCancelled": false,
              "start": { "dateTime": "2026-10-20T10:00:00.0000000", "timeZone": "UTC" },
              "end": { "dateTime": "2026-10-20T11:00:00.0000000", "timeZone": "UTC" },
              "onlineMeeting": null, "location": { "displayName": "Room 4" }, "bodyPreview": "Bring laptops" },
            { "id": "AAMkPacific", "subject": "Wrong zone", "isAllDay": false, "isCancelled": false,
              "start": { "dateTime": "2026-10-20T10:00:00.0000000", "timeZone": "Pacific Standard Time" },
              "end": { "dateTime": "2026-10-20T11:00:00.0000000", "timeZone": "Pacific Standard Time" },
              "onlineMeeting": { "joinUrl": "https://teams.microsoft.com/l/meetup-join/p/0" } }
        ]});
        let meetings = graph_meetings(&body);
        let ids: Vec<_> = meetings.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["microsoft:AAMkTeams", "microsoft:AAMkLegacy", "microsoft:AAMkZoom"]);
        assert!(meetings.iter().all(|m| m.provider == CalendarProvider::Microsoft));

        assert_eq!(meetings[0].title, "Design review");
        assert_eq!(meetings[0].start, utc("2026-10-20T14:00:00Z"));
        assert_eq!(meetings[0].end, utc("2026-10-20T14:45:00Z"));
        assert_eq!(meetings[0].join_url.as_deref(), Some("https://teams.microsoft.com/l/meetup-join/xyz/0"));
        assert_eq!(meetings[1].join_url.as_deref(), Some("https://teams.microsoft.com/l/meetup-join/legacy/0"));
        assert_eq!(meetings[2].join_url.as_deref(), Some("https://us02web.zoom.us/j/555?pwd=q"));
    }

    #[test]
    fn graph_time_parsing() {
        let t = |dt: &str, tz: &str| graph_time(&json!({ "dateTime": dt, "timeZone": tz }));
        assert_eq!(t("2026-10-20T14:00:00.0000000", "UTC"), Some(utc("2026-10-20T14:00:00Z")));
        assert_eq!(t("2026-10-20T14:00:00", "UTC"), Some(utc("2026-10-20T14:00:00Z")));
        assert_eq!(t("2026-10-20T14:00:00.5000000", "UTC"), Some(utc("2026-10-20T14:00:00.5Z")));
        assert_eq!(t("2026-10-20T14:00:00Z", "UTC"), Some(utc("2026-10-20T14:00:00Z")));
        assert_eq!(t("2026-10-20T16:00:00+02:00", "UTC"), Some(utc("2026-10-20T14:00:00Z")));
        assert_eq!(t("2026-10-20T14:00:00.0000000", "W. Europe Standard Time"), None);
        assert_eq!(t("not a date", "UTC"), None);
    }

    #[test]
    fn meetings_serialize_with_lowercase_provider() {
        let m = google_meetings(&json!({ "items": [{
            "id": "a", "summary": "S", "start": { "dateTime": "2026-10-20T10:00:00Z" },
            "end": { "dateTime": "2026-10-20T11:00:00Z" }, "hangoutLink": "https://meet.google.com/abc-defg-hij" }]}));
        let v = serde_json::to_value(&m[0]).unwrap();
        assert_eq!(v["provider"], "google");
        assert_eq!(v["join_url"], "https://meet.google.com/abc-defg-hij");
        assert_eq!(v["start"], "2026-10-20T10:00:00Z");
    }

    #[tokio::test]
    async fn nothing_enabled_means_no_meetings_and_no_warnings() {
        let (meetings, warnings) = upcoming(&Config::default(), Duration::hours(8)).await;
        assert!(meetings.is_empty() && warnings.is_empty());
    }
}
