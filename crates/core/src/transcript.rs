use std::fmt::Write as _;

use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};

use crate::config::ExportFormat;

/// Who spoke. Speakers are separated by audio source: the microphone is `Me`,
/// system/meeting audio is `Them`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Speaker {
    Me,
    Them,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Segment {
    pub speaker: Speaker,
    /// Offset from recording start.
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpeakerLabels {
    pub me: String,
    pub them: String,
}

impl SpeakerLabels {
    pub fn get(&self, speaker: Speaker) -> &str {
        match speaker {
            Speaker::Me => &self.me,
            Speaker::Them => &self.them,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Transcript {
    pub id: String,
    pub title: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    /// Engine that produced the transcript, e.g. `whisper:base.en`.
    pub engine: String,
    pub labels: SpeakerLabels,
    /// Kept sorted by `start_ms`.
    pub segments: Vec<Segment>,
}

/// Consecutive segments from the same speaker closer than this are merged
/// into one paragraph when rendering text.
const MERGE_GAP_MS: u64 = 2_000;

impl Transcript {
    pub fn new(title: impl Into<String>, engine: impl Into<String>, labels: SpeakerLabels) -> Self {
        let started_at = Utc::now();
        let mut title = title.into().trim().to_string();
        if title.is_empty() {
            title = format!("Meeting {}", started_at.with_timezone(&Local).format("%Y-%m-%d %H:%M"));
        }
        Self {
            id: uuid::Uuid::new_v4().simple().to_string(),
            title,
            started_at,
            ended_at: None,
            engine: engine.into(),
            labels,
            segments: Vec::new(),
        }
    }

    /// Inserts a segment keeping chronological order (the two audio sources
    /// are transcribed independently, so segments arrive out of order).
    pub fn push(&mut self, segment: Segment) {
        let idx = self.segments.partition_point(|s| s.start_ms <= segment.start_ms);
        self.segments.insert(idx, segment);
    }

    pub fn duration_ms(&self) -> u64 {
        match self.ended_at {
            Some(end) => (end - self.started_at).num_milliseconds().max(0) as u64,
            None => self.segments.iter().map(|s| s.end_ms).max().unwrap_or(0),
        }
    }

    /// File name without extension: `2026-10-20 1405 Weekly sync`.
    pub fn file_stem(&self) -> String {
        let date = self.started_at.with_timezone(&Local).format("%Y-%m-%d %H%M");
        let title: String = self
            .title
            .chars()
            .map(|c| match c {
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '#' | '%' => '-',
                c if c.is_control() => ' ',
                c => c,
            })
            .collect();
        let title = title.trim().trim_end_matches('.');
        let title: String = title.chars().take(80).collect();
        format!("{date} {title}")
    }

    fn header_line(&self) -> String {
        let start = self.started_at.with_timezone(&Local);
        let mut line = start.format("%Y-%m-%d %H:%M").to_string();
        if let Some(end) = self.ended_at {
            let mins = (self.duration_ms() + 30_000) / 60_000;
            let _ = write!(line, " – {} ({mins} min)", end.with_timezone(&Local).format("%H:%M"));
        }
        line
    }

    /// Paragraphs of `(speaker, start_ms, text)` after merging adjacent
    /// same-speaker segments.
    fn paragraphs(&self) -> Vec<(Speaker, u64, String)> {
        let mut out: Vec<(Speaker, u64, u64, String)> = Vec::new();
        for seg in &self.segments {
            match out.last_mut() {
                Some((speaker, _, end, text))
                    if *speaker == seg.speaker && seg.start_ms <= *end + MERGE_GAP_MS =>
                {
                    text.push(' ');
                    text.push_str(&seg.text);
                    *end = (*end).max(seg.end_ms);
                }
                _ => out.push((seg.speaker, seg.start_ms, seg.end_ms, seg.text.clone())),
            }
        }
        out.into_iter().map(|(s, start, _, t)| (s, start, t)).collect()
    }

    pub fn to_text(&self) -> String {
        let mut out = format!("{}\n{}\n\n", self.title, self.header_line());
        for (speaker, start, text) in self.paragraphs() {
            let _ = writeln!(out, "[{}] {}: {}", fmt_ts(start), self.labels.get(speaker), text);
        }
        out
    }

    pub fn to_markdown(&self) -> String {
        let mut out = format!("# {}\n\n_{}_\n\n", self.title, self.header_line());
        for (speaker, start, text) in self.paragraphs() {
            let _ = writeln!(out, "**{}** `{}`  \n{}\n", self.labels.get(speaker), fmt_ts(start), text);
        }
        out
    }

    pub fn render(&self, format: ExportFormat) -> String {
        match format {
            ExportFormat::Txt => self.to_text(),
            ExportFormat::Md => self.to_markdown(),
            ExportFormat::Json => serde_json::to_string_pretty(self).expect("transcript serializes"),
        }
    }
}

/// `HH:MM:SS` for an offset in milliseconds.
pub fn fmt_ts(ms: u64) -> String {
    let s = ms / 1000;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(speaker: Speaker, start_ms: u64, end_ms: u64, text: &str) -> Segment {
        Segment { speaker, start_ms, end_ms, text: text.into() }
    }

    fn transcript() -> Transcript {
        Transcript::new("Sync", "test", SpeakerLabels { me: "Jiri".into(), them: "Them".into() })
    }

    #[test]
    fn out_of_order_segments_render_chronologically_with_merging() {
        let mut t = transcript();
        t.push(seg(Speaker::Them, 5_000, 7_000, "Hi there."));
        t.push(seg(Speaker::Me, 1_000, 3_000, "Hello."));
        t.push(seg(Speaker::Them, 7_500, 9_000, "How are you?"));
        t.push(seg(Speaker::Me, 65_000, 66_000, "Fine."));
        let text = t.to_text();
        let body: Vec<&str> = text.lines().skip(3).collect();
        assert_eq!(
            body,
            [
                "[00:00:01] Jiri: Hello.",
                "[00:00:05] Them: Hi there. How are you?",
                "[00:01:05] Jiri: Fine.",
            ]
        );
    }

    #[test]
    fn same_speaker_far_apart_is_not_merged() {
        let mut t = transcript();
        t.push(seg(Speaker::Me, 0, 1_000, "One."));
        t.push(seg(Speaker::Me, 10_000, 11_000, "Two."));
        assert_eq!(t.to_text().lines().skip(3).count(), 2);
    }

    #[test]
    fn file_stem_strips_path_separators() {
        let mut t = transcript();
        t.title = "Q3/Q4 plan: draft?".into();
        let stem = t.file_stem();
        assert!(stem.ends_with(" Q3-Q4 plan- draft-"), "{stem}");
        assert!(!stem.contains('/'));
    }
}
