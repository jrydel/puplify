import { forwardRef, useImperativeHandle, useRef, useState } from "react";

import type { Segment, Speaker } from "@/lib/api";
import { fmtTs, paragraphs } from "@/lib/format";
import { cn } from "@/lib/utils";

export interface TranscriptBodyHandle {
  /** Scrolls to the paragraph containing the segment and flashes it. */
  reveal: (segmentIndex: number) => void;
}

interface TranscriptBodyProps {
  segments: Segment[];
  labels: { me: string; them: string };
  partials?: Partial<Record<Speaker, string>>;
}

export const TranscriptBody = forwardRef<TranscriptBodyHandle, TranscriptBodyProps>(function TranscriptBody(
  { segments, labels, partials },
  ref,
) {
  const paras = paragraphs(segments);
  const nodes = useRef(new Map<number, HTMLElement>());
  const [flash, setFlash] = useState<number | null>(null);

  useImperativeHandle(ref, () => ({
    reveal(segmentIndex) {
      const idx = paras.findIndex((p) => segmentIndex >= p.first && segmentIndex <= p.last);
      const node = nodes.current.get(idx);
      if (!node) return;
      node.scrollIntoView({ behavior: "smooth", block: "center" });
      setFlash(idx);
      window.setTimeout(() => setFlash((f) => (f === idx ? null : f)), 1400);
    },
  }));

  return (
    <div className="flex flex-col">
      {paras.map((p, i) => (
        <article
          key={`${p.start_ms}-${p.first}`}
          ref={(el) => {
            if (el) nodes.current.set(i, el);
            else nodes.current.delete(i);
          }}
          className={cn(
            "grid grid-cols-[4.75rem_1fr] gap-x-4 rounded-lg px-3 py-2.5 transition-colors duration-700",
            flash === i && "bg-accent",
          )}
        >
          <time className="pt-0.5 font-mono text-xs tabular-nums text-muted-foreground">{fmtTs(p.start_ms)}</time>
          <div className="flex min-w-0 flex-col gap-0.5">
            <span className={cn("text-xs font-semibold", p.speaker === "me" ? "text-me" : "text-them")}>
              {labels[p.speaker]}
            </span>
            <p className="text-[15px] leading-relaxed text-pretty">{p.text}</p>
          </div>
        </article>
      ))}
      {(["me", "them"] as const).map((speaker) =>
        partials?.[speaker] ? (
          <div key={speaker} className="grid grid-cols-[4.75rem_1fr] gap-x-4 px-3 py-2.5" aria-live="polite">
            <span className="pt-0.5 font-mono text-xs text-muted-foreground">…</span>
            <div className="flex min-w-0 flex-col gap-0.5">
              <span className={cn("text-xs font-semibold", speaker === "me" ? "text-me" : "text-them")}>
                {labels[speaker]}
              </span>
              <p className="text-[15px] leading-relaxed text-muted-foreground italic">{partials[speaker]}</p>
            </div>
          </div>
        ) : null,
      )}
    </div>
  );
});
