import { useState } from "react";
import { SearchIcon, Settings2Icon } from "lucide-react";

import { UpdateCard } from "@/components/update-card";
import { Button } from "@/components/ui/button";
import { InputGroup, InputGroupAddon, InputGroupInput } from "@/components/ui/input-group";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Spinner } from "@/components/ui/spinner";
import type { TranscriptSummary } from "@/lib/api";
import { dayLabel, fmtDuration } from "@/lib/format";
import type { Phase } from "@/lib/session";
import { useUpdater } from "@/lib/updater";
import { cn } from "@/lib/utils";

interface AppSidebarProps {
  history: TranscriptSummary[];
  selectedId: string | null;
  liveId: string | null;
  phase: Phase;
  settingsOpen: boolean;
  onRecord: () => void;
  onSelect: (id: string) => void;
  onSettings: () => void;
}

export function AppSidebar({ history, selectedId, liveId, phase, settingsOpen, onRecord, onSelect, onSettings }: AppSidebarProps) {
  const [query, setQuery] = useState("");
  const q = query.trim().toLowerCase();
  const updater = useUpdater();
  const visible = q ? history.filter((t) => t.title.toLowerCase().includes(q)) : history;

  const groups: { day: string; items: TranscriptSummary[] }[] = [];
  for (const t of visible) {
    const day = dayLabel(new Date(t.started_at));
    const last = groups.at(-1);
    if (last?.day === day) last.items.push(t);
    else groups.push({ day, items: [t] });
  }

  const recording = phase === "recording" || phase === "starting";

  return (
    <aside className="flex h-svh w-72 shrink-0 flex-col border-r border-sidebar-border bg-sidebar text-sidebar-foreground">
      <div className="flex items-center gap-2.5 px-5 pt-5 pb-4">
        <img src="/icon.svg" alt="" className="size-6" />
        <span className="font-heading text-lg font-semibold tracking-tight">Puplify</span>
      </div>

      <div className="flex flex-col gap-3 px-3">
        <Button size="lg" className="w-full justify-start gap-2.5" onClick={onRecord}>
          <span className={cn("size-2.5 rounded-full bg-tally", recording && "animate-pulse")} aria-hidden />
          {recording ? "Recording…" : "New recording"}
        </Button>
        <InputGroup>
          <InputGroupAddon>
            <SearchIcon />
          </InputGroupAddon>
          <InputGroupInput
            placeholder="Search transcripts"
            aria-label="Search transcripts"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </InputGroup>
      </div>

      <ScrollArea className="mt-3 min-h-0 flex-1">
        <nav aria-label="Transcripts" className="flex flex-col gap-4 px-3 pb-3">
          {groups.map((g) => (
            <section key={g.day} className="flex flex-col gap-0.5">
              <h2 className="px-2.5 pb-1 text-[11px] font-medium tracking-wide text-muted-foreground uppercase">{g.day}</h2>
              {g.items.map((t) => {
                const live = t.id === liveId && recording;
                return (
                  <button
                    key={t.id}
                    type="button"
                    onClick={() => onSelect(t.id)}
                    aria-current={t.id === selectedId ? "page" : undefined}
                    className={cn(
                      "flex flex-col gap-0.5 rounded-md px-2.5 py-2 text-left outline-none hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-sidebar-ring",
                      t.id === selectedId && "bg-sidebar-accent",
                    )}
                  >
                    <span className="truncate text-sm font-medium">{t.title}</span>
                    <span className="flex items-center gap-1.5 font-mono text-[11px] text-muted-foreground">
                      {live ? (
                        <>
                          <span className="size-1.5 rounded-full bg-tally" aria-hidden /> recording
                        </>
                      ) : (
                        <>
                          {new Date(t.started_at).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })}
                          <span aria-hidden>·</span>
                          {fmtDuration(t.duration_ms)}
                        </>
                      )}
                    </span>
                  </button>
                );
              })}
            </section>
          ))}
          {groups.length === 0 && (
            <p className="px-2.5 py-6 text-sm text-muted-foreground">
              {q ? `No transcript titles match “${query.trim()}”.` : "Your transcripts will appear here."}
            </p>
          )}
        </nav>
      </ScrollArea>

      <UpdateCard state={updater.state} recording={phase !== "idle" && phase !== "done"} onInstall={updater.install} />

      <div className="flex items-center gap-1 border-t border-sidebar-border p-3">
        <Button
          variant="ghost"
          className={cn("flex-1 justify-start", settingsOpen && "bg-sidebar-accent")}
          onClick={onSettings}
        >
          <Settings2Icon data-icon="inline-start" />
          Settings
        </Button>
        {updater.version && (
          <Button
            variant="ghost"
            size="sm"
            className="font-mono text-xs text-muted-foreground"
            onClick={updater.check}
            disabled={updater.state.phase === "checking"}
            title="Check for updates"
          >
            {updater.state.phase === "checking" && <Spinner data-icon="inline-start" />}v{updater.version}
          </Button>
        )}
      </div>
    </aside>
  );
}
