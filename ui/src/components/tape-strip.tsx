import type { Segment, Speaker } from "@/lib/api";
import { fmtClock, fmtTs, speakingShare } from "@/lib/format";
import { cn } from "@/lib/utils";

const LANES: Speaker[] = ["me", "them"];

/** Tick spacing that yields roughly 4–8 labels across the meeting. */
const TICK_STEPS_MS = [10e3, 30e3, 60e3, 120e3, 300e3, 600e3, 900e3, 1800e3, 3600e3];

interface TapeStripProps {
  segments: Segment[];
  /** Timeline length; for live recordings, the elapsed time. */
  durationMs: number;
  labels: { me: string; them: string };
  live?: boolean;
  onSeek?: (segmentIndex: number) => void;
}

/**
 * Two-track overview of a meeting: one lane per audio source, each utterance
 * drawn at its real position. Shows who spoke when at a glance.
 */
export function TapeStrip({ segments, durationMs, labels, live, onSeek }: TapeStripProps) {
  const span = Math.max(durationMs, ...segments.map((s) => s.end_ms), 1000);
  const share = speakingShare(segments);
  const step = TICK_STEPS_MS.find((s) => span / s <= 8) ?? 7200e3;
  const ticks: number[] = [];
  for (let t = 0; t <= span; t += step) ticks.push(t);

  return (
    <figure className="flex flex-col gap-1.5" aria-label="Who spoke when">
      <div className="grid grid-cols-[5.5rem_1fr] items-center gap-x-3 gap-y-1.5">
        {LANES.map((lane) => (
          <Lane
            key={lane}
            lane={lane}
            label={labels[lane]}
            share={share[lane]}
            segments={segments}
            span={span}
            live={live}
            onSeek={onSeek}
          />
        ))}
        <span />
        <div className="relative h-4 font-mono text-[10px] text-muted-foreground">
          {ticks.map((t) => (
            <span
              key={t}
              className="absolute -translate-x-1/2 first:translate-x-0"
              style={{ left: `${(t / span) * 100}%` }}
            >
              {fmtClock(t)}
            </span>
          ))}
        </div>
      </div>
    </figure>
  );
}

function Lane({
  lane,
  label,
  share,
  segments,
  span,
  live,
  onSeek,
}: {
  lane: Speaker;
  label: string;
  share: number;
  segments: Segment[];
  span: number;
  live?: boolean;
  onSeek?: (segmentIndex: number) => void;
}) {
  const color = lane === "me" ? "bg-me" : "bg-them";
  return (
    <>
      <div className="flex min-w-0 items-baseline justify-between gap-2">
        <span className={cn("truncate text-xs font-semibold", lane === "me" ? "text-me" : "text-them")}>{label}</span>
        <span className="font-mono text-[10px] tabular-nums text-muted-foreground">{Math.round(share * 100)}%</span>
      </div>
      <div className="relative h-3.5 overflow-hidden rounded-full bg-muted">
        {segments.map((seg, i) =>
          seg.speaker !== lane ? null : (
            <button
              key={`${seg.start_ms}-${i}`}
              type="button"
              tabIndex={onSeek ? 0 : -1}
              disabled={!onSeek}
              aria-label={`${label} at ${fmtTs(seg.start_ms)}`}
              title={`${fmtTs(seg.start_ms)} · ${seg.text}`}
              onClick={() => onSeek?.(i)}
              className={cn(
                "absolute inset-y-0 rounded-full transition-opacity outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1",
                color,
                onSeek && "cursor-pointer hover:opacity-70",
                live && "animate-in fade-in duration-500",
              )}
              style={{
                left: `${(seg.start_ms / span) * 100}%`,
                width: `max(4px, ${((seg.end_ms - seg.start_ms) / span) * 100}%)`,
              }}
            />
          ),
        )}
        {live && <span className="absolute inset-y-0 right-0 w-0.5 animate-pulse bg-tally" aria-hidden />}
      </div>
    </>
  );
}
