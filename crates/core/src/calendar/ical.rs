//! Online meetings from an iCalendar (RFC 5545) feed: the private "secret
//! address" links of Google Calendar and the "published calendar" links of
//! Outlook. Handles recurring series (RRULE, RDATE, EXDATE), moved or cancelled
//! occurrences (RECURRENCE-ID) and the time zone flavours both services emit.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use rrule::{RRule, RRuleSet, Tz, Unvalidated};

use super::windows_zones::WINDOWS_TO_IANA;
use super::{find_join_url, rfc3339, title, CalendarProvider, Meeting};

const DATE_TIME: &str = "%Y%m%dT%H%M%S";

/// Properties searched for a video-call link, most specific first.
const JOIN_URL_PROPERTIES: [&str; 8] = [
    "X-GOOGLE-CONFERENCE",
    "X-MICROSOFT-SKYPETEAMSMEETINGURL",
    "CONFERENCE",
    "X-MICROSOFT-ONLINEMEETINGEXTERNALLINK",
    "LOCATION",
    "URL",
    "DESCRIPTION",
    "X-ALT-DESC",
];

/// One content line: `NAME;PARAM=VALUE:value`.
struct Prop {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

impl Prop {
    fn parse(line: &str) -> Option<Self> {
        let mut quoted = false;
        let colon = line.char_indices().find(|&(_, c)| {
            quoted ^= c == '"';
            c == ':' && !quoted
        })?;
        let (head, value) = (&line[..colon.0], &line[colon.0 + 1..]);

        let mut parts = Vec::new();
        let (mut start, mut quoted) = (0, false);
        for (i, c) in head.char_indices() {
            quoted ^= c == '"';
            if c == ';' && !quoted {
                parts.push(&head[start..i]);
                start = i + 1;
            }
        }
        parts.push(&head[start..]);

        let params = parts[1..]
            .iter()
            .filter_map(|p| p.split_once('='))
            .map(|(k, v)| (k.trim().to_ascii_uppercase(), v.trim().trim_matches('"').to_string()))
            .collect();
        Some(Self { name: parts[0].trim().to_ascii_uppercase(), params, value: value.to_string() })
    }

    fn param(&self, name: &str) -> Option<&str> {
        self.params.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

/// Property lines with RFC 5545 line folding (CRLF followed by a space or tab) undone.
fn unfold(ics: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in ics.trim_start_matches('\u{feff}').lines() {
        match (raw.strip_prefix([' ', '\t']), lines.last_mut()) {
            (Some(continuation), Some(last)) => last.push_str(continuation),
            _ if raw.is_empty() => {}
            _ => lines.push(raw.to_string()),
        }
    }
    lines
}

/// TEXT value escapes: `\n`, `\,`, `\;`, `\\`.
fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(escaped) => out.push(escaped),
            None => out.push('\\'),
        }
    }
    out
}

/// Time zone context while reading one feed.
struct Zones<'a> {
    /// Zone for times without `Z` or `TZID`.
    floating: Tz,
    /// `X-LIC-LOCATION` of each VTIMEZONE, keyed by its `TZID`.
    locations: &'a HashMap<String, String>,
}

impl Zones<'_> {
    /// IANA name, Windows name (Outlook), or the IANA `X-LIC-LOCATION` of the
    /// feed's VTIMEZONE; UTC when none of these is known.
    fn resolve(&self, tzid: &str) -> Tz {
        let tzid = tzid.trim().trim_matches('"');
        let iana = |name: &str| chrono_tz::Tz::from_str(name).ok();
        iana(tzid)
            .or_else(|| {
                let (_, name) = WINDOWS_TO_IANA.iter().find(|(windows, _)| windows.eq_ignore_ascii_case(tzid))?;
                iana(name)
            })
            .or_else(|| iana(self.locations.get(tzid)?))
            // `/mozilla.org/20050126_1/Europe/Berlin`-style globally unique ids.
            .or_else(|| tzid.match_indices('/').find_map(|(i, _)| iana(&tzid[i + 1..])))
            .map_or(Tz::UTC, Tz::Tz)
    }

    /// A `DATE-TIME` value (`20260120T140000`, with optional `Z`); `None` for dates and garbage.
    fn date_time(&self, value: &str, tzid: Option<&str>) -> Option<DateTime<Tz>> {
        let (text, utc) = match value.trim().strip_suffix(['Z', 'z']) {
            Some(text) => (text, true),
            None => (value.trim(), false),
        };
        let naive = NaiveDateTime::parse_from_str(text, DATE_TIME).ok()?;
        let tz = if utc { Tz::UTC } else { tzid.map_or(self.floating, |id| self.resolve(id)) };
        local(naive, tz)
    }
}

/// Wall-clock time in `tz`: the earlier of two readings across a DST fall-back,
/// moved one hour on when it falls into a spring-forward gap.
fn local(naive: NaiveDateTime, tz: Tz) -> Option<DateTime<Tz>> {
    tz.from_local_datetime(&naive)
        .earliest()
        .or_else(|| tz.from_local_datetime(&(naive + Duration::hours(1))).earliest())
}

/// `P1DT2H30M`, `PT45M`, `P1W`.
fn parse_duration(value: &str) -> Option<Duration> {
    let value = value.trim();
    let (negative, rest) = match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value.strip_prefix('+').unwrap_or(value)),
    };
    let (mut total, mut number, mut in_time) = (Duration::zero(), 0i64, false);
    for c in rest.strip_prefix('P')?.chars() {
        if let Some(digit) = c.to_digit(10) {
            number = number.checked_mul(10)?.checked_add(i64::from(digit))?;
            continue;
        }
        total += match (c, in_time) {
            ('T', false) => {
                in_time = true;
                continue;
            }
            ('W', false) => Duration::try_weeks(number)?,
            ('D', false) => Duration::try_days(number)?,
            ('H', true) => Duration::try_hours(number)?,
            ('M', true) => Duration::try_minutes(number)?,
            ('S', true) => Duration::try_seconds(number)?,
            _ => return None,
        };
        number = 0;
    }
    Some(if negative { -total } else { total })
}

struct Event {
    uid: String,
    /// Set on an overriding VEVENT: the original start of the occurrence it replaces.
    recurrence_id: Option<DateTime<Utc>>,
    start: DateTime<Tz>,
    duration: Duration,
    title: String,
    join_url: Option<String>,
    cancelled: bool,
    rrules: Vec<String>,
    rdates: Vec<DateTime<Tz>>,
    exdates: HashSet<DateTime<Utc>>,
}

/// `None` for events without a usable `DTSTART` and for all-day events.
fn event(props: &[Prop], zones: &Zones) -> Option<Event> {
    let first = |name: &str| props.iter().find(|p| p.name == name);
    let time = |prop: &Prop| zones.date_time(&prop.value, prop.param("TZID"));

    let start = time(first("DTSTART")?)?;
    let duration = match (first("DTEND").and_then(time), first("DURATION")) {
        (Some(end), _) => end.to_utc() - start.to_utc(),
        (None, Some(d)) => parse_duration(&d.value).unwrap_or_default(),
        (None, None) => Duration::zero(),
    }
    .max(Duration::zero());
    let recurrence_id = match first("RECURRENCE-ID") {
        Some(prop) => Some(time(prop)?.to_utc()),
        None => None,
    };

    // Overrides never carry their own recurrence rules.
    let recurring = recurrence_id.is_none();
    let all = |name: &'static str| props.iter().filter(move |p| recurring && p.name == name);
    let rrules = all("RRULE").map(|p| p.value.clone()).collect();
    let dates = |name| {
        all(name).flat_map(|p| {
            p.value.split(',').filter_map(|v| zones.date_time(v, p.param("TZID"))).collect::<Vec<_>>()
        })
    };
    let rdates = dates("RDATE").collect();
    let exdates = dates("EXDATE").map(|d| d.to_utc()).collect();

    let join_url = JOIN_URL_PROPERTIES.iter().find_map(|name| {
        props.iter().filter(|p| p.name == *name).find_map(|p| find_join_url(&unescape(&p.value)))
    });
    let uid = first("UID").map(|p| p.value.trim().to_string()).filter(|u| !u.is_empty());
    let title = title(first("SUMMARY").map(|p| unescape(&p.value)).as_deref());
    Some(Event {
        uid: uid.unwrap_or_else(|| title.clone()),
        recurrence_id,
        start,
        duration,
        title,
        join_url,
        cancelled: first("STATUS").is_some_and(|p| p.value.trim().eq_ignore_ascii_case("CANCELLED")),
        rrules,
        rdates,
        exdates,
    })
}

/// A validated RRULE; `UNTIL` is handled here because the `rrule` crate only
/// accepts it in UTC, while feeds also use floating and date-only values.
fn build_rule(rule: &str, start: &DateTime<Tz>) -> Option<RRule> {
    let mut until = None;
    let kept: Vec<&str> = rule
        .split(';')
        .filter(|part| match part.split_once('=') {
            Some((key, value)) if key.trim().eq_ignore_ascii_case("UNTIL") => {
                until = Some(value.trim());
                false
            }
            _ => true,
        })
        .collect();
    let mut parsed: RRule<Unvalidated> = kept.join(";").parse().ok()?;
    if let Some(until) = until {
        let tz = start.timezone();
        let until = match until.strip_suffix(['Z', 'z']) {
            Some(utc) => NaiveDateTime::parse_from_str(utc, DATE_TIME).ok()?.and_utc(),
            None => {
                let naive = NaiveDateTime::parse_from_str(until, DATE_TIME)
                    .ok()
                    .or_else(|| NaiveDate::parse_from_str(until, "%Y%m%d").ok()?.and_hms_opt(23, 59, 59))?;
                local(naive, tz)?.to_utc()
            }
        };
        parsed = parsed.until(until.with_timezone(&Tz::UTC));
    }
    parsed.validate(*start).ok()
}

/// Start instants of the event's occurrences that begin in `[from - duration, to]`.
/// Only that range is expanded, so long-running series cost nothing extra.
fn occurrences(event: &Event, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<DateTime<Utc>> {
    let first = from - event.duration;
    let tz = event.start.timezone();
    let mut starts = vec![event.start.to_utc()];
    starts.extend(event.rdates.iter().map(DateTime::to_utc));
    for rule in event.rrules.iter().filter_map(|rule| build_rule(rule, &event.start)) {
        let set = RRuleSet::new(event.start).rrule(rule).after(first.with_timezone(&tz)).before(to.with_timezone(&tz));
        starts.extend(set.all(u16::MAX).dates.iter().map(DateTime::to_utc));
    }
    starts.retain(|s| (first..=to).contains(s) && !event.exdates.contains(s));
    starts.sort_unstable();
    starts.dedup();
    starts
}

/// Online meetings of one feed overlapping `[from, to]`. `link` is the position
/// of the feed in the configuration and `floating` the zone of times written
/// without any zone information.
pub(super) fn meetings(ics: &str, link: usize, floating: Tz, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Meeting> {
    let mut vevents: Vec<Vec<Prop>> = Vec::new();
    let mut locations = HashMap::new();
    // The component being read, and how many nested components (VALARM,
    // STANDARD, DAYLIGHT) deep inside it; their properties are not the event's own.
    let mut current: Option<(String, Vec<Prop>)> = None;
    let mut nested = 0usize;
    for prop in unfold(ics).iter().filter_map(|line| Prop::parse(line)) {
        match (prop.name.as_str(), &mut current) {
            ("BEGIN", None) => {
                let kind = prop.value.trim().to_ascii_uppercase();
                if kind == "VEVENT" || kind == "VTIMEZONE" {
                    current = Some((kind, Vec::new()));
                }
            }
            ("BEGIN", Some(_)) => nested += 1,
            ("END", Some(_)) if nested > 0 => nested -= 1,
            ("END", Some(_)) => {
                let Some((kind, props)) = current.take() else { continue };
                if kind == "VEVENT" {
                    vevents.push(props);
                    continue;
                }
                let get = |name: &str| props.iter().find(|p| p.name == name).map(|p| p.value.trim().to_string());
                if let (Some(tzid), Some(location)) = (get("TZID"), get("X-LIC-LOCATION")) {
                    locations.insert(tzid, location);
                }
            }
            (_, Some((_, props))) if nested == 0 => props.push(prop),
            _ => {}
        }
    }

    let zones = Zones { floating, locations: &locations };
    let events: Vec<Event> = vevents.iter().filter_map(|props| event(props, &zones)).collect();
    let replaced: HashSet<(&str, DateTime<Utc>)> =
        events.iter().filter_map(|e| Some((e.uid.as_str(), e.recurrence_id?))).collect();

    let mut found = Vec::new();
    for event in &events {
        let Some(join_url) = &event.join_url else { continue };
        if event.cancelled {
            continue;
        }
        for start in occurrences(event, from, to) {
            if event.recurrence_id.is_none() && replaced.contains(&(event.uid.as_str(), start)) {
                continue;
            }
            found.push(Meeting {
                id: format!("ical:{link}:{}:{}", event.uid, rfc3339(start)),
                title: event.title.clone(),
                start,
                end: start + event.duration,
                join_url: Some(join_url.clone()),
                provider: CalendarProvider::Ical,
            });
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().to_utc()
    }

    fn prague() -> Tz {
        Tz::Tz(chrono_tz::Europe::Prague)
    }

    fn starts(found: &[Meeting]) -> Vec<String> {
        let mut starts: Vec<_> = found.iter().map(|m| rfc3339(m.start)).collect();
        starts.sort();
        starts
    }

    const GOOGLE: &str = "BEGIN:VCALENDAR\r
PRODID:-//Google Inc//Google Calendar 70.9054//EN\r
VERSION:2.0\r
X-WR-TIMEZONE:Europe/Prague\r
BEGIN:VEVENT\r
DTSTART;TZID=Europe/Prague:20250106T100000\r
DTEND;TZID=Europe/Prague:20250106T103000\r
RRULE:FREQ=WEEKLY;BYDAY=MO\r
EXDATE;TZID=Europe/Prague:20260119T100000\r
UID:standup@google.com\r
STATUS:CONFIRMED\r
SUMMARY:Team standup\r
X-GOOGLE-CONFERENCE:https://meet.google.com/abc-defg-hij\r
END:VEVENT\r
BEGIN:VEVENT\r
DTSTART;TZID=Europe/Prague:20260126T150000\r
DTEND;TZID=Europe/Prague:20260126T153000\r
RECURRENCE-ID;TZID=Europe/Prague:20260126T100000\r
UID:standup@google.com\r
STATUS:CONFIRMED\r
SUMMARY:Team standup (moved)\r
X-GOOGLE-CONFERENCE:https://meet.google.com/abc-defg-hij\r
END:VEVENT\r
BEGIN:VEVENT\r
DTSTART;TZID=Europe/Prague:20260202T100000\r
DTEND;TZID=Europe/Prague:20260202T103000\r
RECURRENCE-ID;TZID=Europe/Prague:20260202T100000\r
UID:standup@google.com\r
STATUS:CANCELLED\r
SUMMARY:Canceled: Team standup\r
END:VEVENT\r
BEGIN:VEVENT\r
DTSTART;TZID=Europe/Prague:20260121T133000\r
DTEND;TZID=Europe/Prague:20260121T143000\r
UID:lunch@google.com\r
SUMMARY:Planning\\, Q1 \\; roadmap\r
DESCRIPTION:Agenda\\nJoin: https://us02web.zoom.us/j/81234567890?pwd=Ab\r
 C123\\nSee you\r
LOCATION:Room 4\r
END:VEVENT\r
BEGIN:VEVENT\r
DTSTART;TZID=Europe/Prague:20260121T160000\r
DTEND;TZID=Europe/Prague:20260121T170000\r
UID:no-link@google.com\r
SUMMARY:Dentist\r
LOCATION:Main street 1\r
END:VEVENT\r
END:VCALENDAR\r
";

    #[test]
    fn google_series_with_exdate_moved_and_cancelled_occurrences() {
        let found = meetings(GOOGLE, 0, prague(), utc("2026-01-05T00:00:00Z"), utc("2026-02-10T00:00:00Z"));
        let standups: Vec<_> = found.iter().filter(|m| m.id.contains("standup@google.com")).collect();
        assert_eq!(
            starts(&found.iter().filter(|m| m.id.contains("standup")).cloned().collect::<Vec<_>>()),
            [
                "2026-01-05T09:00:00Z",
                "2026-01-12T09:00:00Z",
                // 2026-01-19 is an EXDATE, the 26th moved to 15:00 local, 2026-02-02 is cancelled.
                "2026-01-26T14:00:00Z",
                "2026-02-09T09:00:00Z",
            ]
        );
        assert_eq!(standups[0].id, "ical:0:standup@google.com:2026-01-05T09:00:00Z");
        assert_eq!(standups[0].end, utc("2026-01-05T09:30:00Z"));
        assert_eq!(standups[0].join_url.as_deref(), Some("https://meet.google.com/abc-defg-hij"));
        assert!(standups.iter().all(|m| m.provider == CalendarProvider::Ical));
        assert_eq!(standups.iter().filter(|m| m.title == "Team standup (moved)").count(), 1);
    }

    #[test]
    fn google_series_follows_daylight_saving_time() {
        let found = meetings(GOOGLE, 3, prague(), utc("2026-07-06T00:00:00Z"), utc("2026-07-07T00:00:00Z"));
        assert_eq!(starts(&found), ["2026-07-06T08:00:00Z"]);
        assert_eq!(found[0].id, "ical:3:standup@google.com:2026-07-06T08:00:00Z");
    }

    #[test]
    fn unfolds_and_unescapes_text() {
        let found = meetings(GOOGLE, 0, prague(), utc("2026-01-21T00:00:00Z"), utc("2026-01-22T00:00:00Z"));
        assert_eq!(found.len(), 1, "the event without a link is left out");
        assert_eq!(found[0].title, "Planning, Q1 ; roadmap");
        assert_eq!(found[0].join_url.as_deref(), Some("https://us02web.zoom.us/j/81234567890?pwd=AbC123"));
        assert_eq!(found[0].start, utc("2026-01-21T12:30:00Z"));
    }

    #[test]
    fn only_expands_the_requested_window() {
        let daily = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:daily\nSUMMARY:Daily\n\
            DTSTART;TZID=Europe/Prague:20100104T090000\nDTEND;TZID=Europe/Prague:20100104T091500\n\
            RRULE:FREQ=DAILY\nLOCATION:https://meet.google.com/daily-room\nEND:VEVENT\nEND:VCALENDAR\n";
        let started = std::time::Instant::now();
        let found = meetings(daily, 1, prague(), utc("2026-03-10T08:10:00Z"), utc("2026-03-12T08:30:00Z"));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(starts(&found), ["2026-03-10T08:00:00Z", "2026-03-11T08:00:00Z", "2026-03-12T08:00:00Z"]);
    }

    #[test]
    fn running_meeting_is_included() {
        let found = meetings(GOOGLE, 0, prague(), utc("2026-01-12T09:10:00Z"), utc("2026-01-12T09:20:00Z"));
        assert_eq!(starts(&found), ["2026-01-12T09:00:00Z"]);
    }

    const OUTLOOK: &str = "BEGIN:VCALENDAR\r
METHOD:PUBLISH\r
PRODID:Microsoft Exchange Server 2010\r
VERSION:2.0\r
X-WR-CALNAME:Calendar\r
BEGIN:VTIMEZONE\r
TZID:W. Europe Standard Time\r
BEGIN:STANDARD\r
DTSTART:16010101T030000\r
TZOFFSETFROM:+0200\r
TZOFFSETTO:+0100\r
RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=10\r
END:STANDARD\r
BEGIN:DAYLIGHT\r
DTSTART:16010101T020000\r
TZOFFSETFROM:+0100\r
TZOFFSETTO:+0200\r
RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=3\r
END:DAYLIGHT\r
END:VTIMEZONE\r
BEGIN:VTIMEZONE\r
TZID:Customized Time Zone\r
BEGIN:STANDARD\r
DTSTART:16010101T000000\r
TZOFFSETFROM:+0300\r
TZOFFSETTO:+0300\r
END:STANDARD\r
END:VTIMEZONE\r
BEGIN:VEVENT\r
UID:040000008200E00074C5B7101A82E008000000000000000000000000\r
SUMMARY:Design review\r
DTSTART;TZID=W. Europe Standard Time:20260120T140000\r
DTEND;TZID=W. Europe Standard Time:20260120T150000\r
DESCRIPTION:Join the call\r
X-MICROSOFT-SKYPETEAMSMEETINGURL:https://teams.microsoft.com/l/meetup-join/19%3ameeting_abc%40thread.v2/0?context=%7b%22Tid%22%3a%22x%22%7d\r
BEGIN:VALARM\r
TRIGGER:-PT15M\r
ACTION:DISPLAY\r
DESCRIPTION:Reminder https://zoom.us/j/999\r
END:VALARM\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:allday\r
SUMMARY:Company offsite\r
DTSTART;VALUE=DATE:20260121\r
DTEND;VALUE=DATE:20260122\r
DESCRIPTION:https://teams.microsoft.com/l/meetup-join/19%3ameeting_def%40thread.v2/0\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:alarm-only\r
SUMMARY:Alarm link only\r
DTSTART;TZID=W. Europe Standard Time:20260120T160000\r
DTEND;TZID=W. Europe Standard Time:20260120T170000\r
BEGIN:VALARM\r
TRIGGER:-PT15M\r
ACTION:DISPLAY\r
DESCRIPTION:Reminder https://zoom.us/j/999\r
END:VALARM\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:us-sync\r
SUMMARY:US sync\r
DTSTART;TZID=Pacific Standard Time:20260120T080000\r
DTEND;TZID=Pacific Standard Time:20260120T083000\r
RRULE:FREQ=DAILY;COUNT=3\r
LOCATION:Microsoft Teams Meeting\r
X-ALT-DESC;FMTTYPE=text/html:<html><body><a href=\"https://teams.microsoft.com/l/meetup-join/19%3ameeting_ghi%40thread.v2/0\">Join</a></body></html>\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:custom\r
SUMMARY:Custom zone\r
DTSTART;TZID=Customized Time Zone:20260122T100000\r
DTEND;TZID=Customized Time Zone:20260122T110000\r
URL:https://acme.webex.com/acme/j.php?MTID=m123\r
END:VEVENT\r
END:VCALENDAR\r
";

    #[test]
    fn outlook_windows_time_zones_teams_links_and_exclusions() {
        let found = meetings(OUTLOOK, 1, prague(), utc("2026-01-19T00:00:00Z"), utc("2026-01-25T00:00:00Z"));
        let by_title = |t: &str| found.iter().filter(|m| m.title == t).map(|m| rfc3339(m.start)).collect::<Vec<_>>();
        assert_eq!(by_title("Design review"), ["2026-01-20T13:00:00Z"]);
        // Pacific Standard Time is UTC-8 in winter.
        assert_eq!(by_title("US sync"), ["2026-01-20T16:00:00Z", "2026-01-21T16:00:00Z", "2026-01-22T16:00:00Z"]);
        // Unknown zone: falls back to UTC instead of failing.
        assert_eq!(by_title("Custom zone"), ["2026-01-22T10:00:00Z"]);
        assert!(by_title("Company offsite").is_empty(), "all-day events are skipped");
        assert!(by_title("Alarm link only").is_empty(), "VALARM text is not the event's own");
        let review = found.iter().find(|m| m.title == "Design review").unwrap();
        assert_eq!(
            review.join_url.as_deref(),
            Some("https://teams.microsoft.com/l/meetup-join/19%3ameeting_abc%40thread.v2/0?context=%7b%22Tid%22%3a%22x%22%7d")
        );
        let sync = found.iter().find(|m| m.title == "US sync").unwrap();
        assert_eq!(sync.join_url.as_deref(), Some("https://teams.microsoft.com/l/meetup-join/19%3ameeting_ghi%40thread.v2/0"));
        assert_eq!(found.len(), 5);
    }

    #[test]
    fn outlook_windows_zone_daylight_saving_time() {
        let found = meetings(OUTLOOK, 0, prague(), utc("2026-01-19T00:00:00Z"), utc("2026-01-25T00:00:00Z"));
        assert!(found.iter().any(|m| m.title == "Design review" && m.start == utc("2026-01-20T13:00:00Z")));
        let summer = OUTLOOK.replace("20260120T140000", "20260721T140000").replace("20260120T150000", "20260721T150000");
        let found = meetings(&summer, 0, prague(), utc("2026-07-21T00:00:00Z"), utc("2026-07-22T00:00:00Z"));
        assert_eq!(starts(&found.iter().filter(|m| m.title == "Design review").cloned().collect::<Vec<_>>()), ["2026-07-21T12:00:00Z"]);
    }

    #[test]
    fn utc_floating_and_rule_end_dates() {
        let ics = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:utc\nSUMMARY:UTC\nDTSTART:20260120T140000Z\nDTEND:20260120T143000Z\n\
            LOCATION:https://meet.google.com/utc-room\nEND:VEVENT\n\
            BEGIN:VEVENT\nUID:floating\nSUMMARY:Floating\nDTSTART:20260120T140000\nDURATION:PT1H30M\n\
            URL:https://zoom.us/j/123\nEND:VEVENT\n\
            BEGIN:VEVENT\nUID:until-date\nSUMMARY:Until date\nDTSTART:20260120T150000Z\nDTEND:20260120T153000Z\n\
            RRULE:FREQ=DAILY;UNTIL=20260121\nLOCATION:https://meet.google.com/until-date\nEND:VEVENT\n\
            BEGIN:VEVENT\nUID:until-utc\nSUMMARY:Until UTC\nDTSTART;TZID=Europe/Prague:20260120T170000\nDTEND;TZID=Europe/Prague:20260120T173000\n\
            RRULE:FREQ=DAILY;UNTIL=20260121T150000Z\nLOCATION:https://meet.google.com/until-utc\nEND:VEVENT\n\
            BEGIN:VEVENT\nUID:rdate\nSUMMARY:Rdate\nDTSTART:20260120T180000Z\nDTEND:20260120T183000Z\n\
            RDATE:20260123T180000Z,20260124T180000Z\nEXDATE:20260124T180000Z\nLOCATION:https://meet.google.com/rdate\nEND:VEVENT\n\
            END:VCALENDAR\n";
        let found = meetings(ics, 2, prague(), utc("2026-01-20T00:00:00Z"), utc("2026-01-30T00:00:00Z"));
        let of = |uid: &str| {
            let prefix = format!("ical:2:{uid}:");
            found.iter().filter(|m| m.id.starts_with(&prefix)).map(|m| rfc3339(m.start)).collect::<Vec<_>>()
        };
        assert_eq!(of("utc"), ["2026-01-20T14:00:00Z"]);
        // Floating time is read in the supplied zone (Prague, UTC+1).
        assert_eq!(of("floating"), ["2026-01-20T13:00:00Z"]);
        assert_eq!(found.iter().find(|m| m.id.contains("floating")).unwrap().end, utc("2026-01-20T14:30:00Z"));
        assert_eq!(of("until-date"), ["2026-01-20T15:00:00Z", "2026-01-21T15:00:00Z"]);
        assert_eq!(of("until-utc"), ["2026-01-20T16:00:00Z"]);
        assert_eq!(of("rdate"), ["2026-01-20T18:00:00Z", "2026-01-23T18:00:00Z"]);
    }

    #[test]
    fn malformed_input_yields_no_meetings() {
        for ics in [
            "",
            "garbage\nBEGIN:VEVENT\nDTSTART:nonsense\nLOCATION:https://zoom.us/j/1\nEND:VEVENT",
            "BEGIN:VEVENT\nDTSTART:20260120T140000Z\nRRULE:FREQ=NOPE\nLOCATION:https://zoom.us/j/1\n",
            "BEGIN:VEVENT\nDTSTART;TZID=Nowhere/Land:20260120T140000\nEND:VEVENT\nEND:VEVENT\nEND:VCALENDAR",
        ] {
            let found = meetings(ics, 0, prague(), utc("2026-01-19T00:00:00Z"), utc("2026-01-25T00:00:00Z"));
            // Must not panic; a bad RRULE falls back to the single DTSTART occurrence.
            assert!(found.len() <= 1, "{ics:?}");
        }
    }
}
