import type { Speaker } from "@/lib/api";
import { cn } from "@/lib/utils";

const BARS = 24;
/** Meter floor; quieter input shows as empty. */
const FLOOR_DB = -60;

interface LevelMeterProps {
  speaker: Speaker;
  label: string;
  /** RMS level, 0–1. */
  level: number;
  source: string;
}

/** Segmented input meter showing that a source is actually live. */
export function LevelMeter({ speaker, label, level, source }: LevelMeterProps) {
  const db = level > 0 ? 20 * Math.log10(level) : FLOOR_DB;
  const lit = Math.round(Math.min(1, Math.max(0, (db - FLOOR_DB) / -FLOOR_DB)) * BARS);
  return (
    <div className="flex min-w-0 flex-1 flex-col gap-1.5">
      <div className="flex items-baseline justify-between gap-2 text-xs">
        <span className={cn("font-semibold", speaker === "me" ? "text-me" : "text-them")}>{label}</span>
        <span className="truncate text-muted-foreground">{source}</span>
      </div>
      <div
        className="flex h-2 gap-0.5"
        role="meter"
        aria-label={`${label} input level`}
        aria-valuemin={FLOOR_DB}
        aria-valuemax={0}
        aria-valuenow={Math.round(db)}
      >
        {Array.from({ length: BARS }, (_, i) => (
          <span
            key={i}
            className={cn(
              "flex-1 rounded-[1px] transition-colors duration-75",
              i < lit ? (speaker === "me" ? "bg-me" : "bg-them") : "bg-muted",
            )}
          />
        ))}
      </div>
    </div>
  );
}
