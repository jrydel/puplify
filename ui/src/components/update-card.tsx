import { ArrowDownToLineIcon } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import { Spinner } from "@/components/ui/spinner";
import type { UpdateState } from "@/lib/updater";

/** Sidebar notice with the one-click update, shown only while an update is on offer. */
export function UpdateCard({
  state,
  recording,
  onInstall,
}: {
  state: UpdateState;
  /** Updating restarts Puplify, so it waits until the recording is stopped. */
  recording: boolean;
  onInstall: () => void;
}) {
  if (state.phase !== "available" && state.phase !== "downloading" && state.phase !== "restarting") return null;
  const version = state.update.version;
  const percent =
    state.phase === "downloading" && state.total ? Math.min(100, Math.round((state.done / state.total) * 100)) : null;

  return (
    <div className="mx-3 mb-3 flex flex-col gap-2 rounded-lg border border-sidebar-border bg-sidebar-accent/60 p-3">
      <p className="text-sm font-medium">Puplify {version} is available</p>
      {state.phase === "available" ? (
        <>
          <Button size="sm" onClick={onInstall} disabled={recording}>
            <ArrowDownToLineIcon data-icon="inline-start" />
            Update and restart
          </Button>
          {recording && <p className="text-xs text-muted-foreground">Available once the current recording is saved.</p>}
        </>
      ) : (
        <div className="flex flex-col gap-1.5">
          {percent !== null && percent < 100 && <Progress value={percent} aria-label="Downloading update" />}
          <p className="flex items-center gap-1.5 text-xs text-muted-foreground">
            <Spinner className="size-3" />
            {state.phase === "restarting"
              ? "Restarting…"
              : percent !== null && percent < 100
                ? `Downloading… ${percent}%`
                : "Installing…"}
          </p>
        </div>
      )}
    </div>
  );
}
