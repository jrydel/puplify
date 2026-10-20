import type { Meeting, Segment, Speaker } from "./api";

/** Where a meeting came from, for "From …" lines. */
export const CALENDAR_NAME: Record<Meeting["provider"], string> = {
  google: "Google Calendar",
  microsoft: "Outlook",
  ical: "your calendar link",
};

export function fmtTs(ms: number): string {
  const s = Math.floor(ms / 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(Math.floor(s / 3600))}:${pad(Math.floor(s / 60) % 60)}:${pad(s % 60)}`;
}

/** Compact clock for axis ticks: `12:30` or `1:02:30`. */
export function fmtClock(ms: number): string {
  const s = Math.round(ms / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor(s / 60) % 60;
  const sec = String(s % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
}

export function fmtDuration(ms: number): string {
  const m = Math.round(ms / 60000);
  if (m < 1) return "under a minute";
  if (m < 60) return `${m} min`;
  return `${Math.floor(m / 60)} h ${m % 60} min`;
}

export function dayLabel(date: Date): string {
  const today = new Date();
  const yesterday = new Date();
  yesterday.setDate(today.getDate() - 1);
  if (date.toDateString() === today.toDateString()) return "Today";
  if (date.toDateString() === yesterday.toDateString()) return "Yesterday";
  return date.toLocaleDateString(undefined, { weekday: "long", month: "short", day: "numeric" });
}

export function timeOfDay(date: Date): string {
  return date.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}

export interface Paragraph {
  speaker: Speaker;
  start_ms: number;
  end_ms: number;
  text: string;
  /** Indexes into the segment list this paragraph covers. */
  first: number;
  last: number;
}

/** Same merge rule as the Rust renderer: same speaker, gap ≤ 2 s. */
export function paragraphs(segments: Segment[]): Paragraph[] {
  const out: Paragraph[] = [];
  segments.forEach((seg, i) => {
    const prev = out.at(-1);
    if (prev && prev.speaker === seg.speaker && seg.start_ms <= prev.end_ms + 2000) {
      prev.text += ` ${seg.text}`;
      prev.end_ms = Math.max(prev.end_ms, seg.end_ms);
      prev.last = i;
    } else {
      out.push({ speaker: seg.speaker, start_ms: seg.start_ms, end_ms: seg.end_ms, text: seg.text, first: i, last: i });
    }
  });
  return out;
}

/** Share of speaking time per speaker, 0–1. */
export function speakingShare(segments: Segment[]): Record<Speaker, number> {
  const total = { me: 0, them: 0 };
  for (const s of segments) total[s.speaker] += Math.max(0, s.end_ms - s.start_ms);
  const sum = total.me + total.them;
  return sum ? { me: total.me / sum, them: total.them / sum } : { me: 0, them: 0 };
}

/** Insert keeping chronological order (sources are transcribed independently). */
export function insertSorted(segments: Segment[], seg: Segment): Segment[] {
  const i = segments.findIndex((s) => s.start_ms > seg.start_ms);
  return i === -1 ? [...segments, seg] : [...segments.slice(0, i), seg, ...segments.slice(i)];
}
