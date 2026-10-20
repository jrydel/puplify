import { useCallback, useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { toast } from "sonner";

import { errorText } from "./api";

/** Background checks run at start-up and then this often. */
const CHECK_EVERY_MS = 6 * 60 * 60 * 1000;

export type UpdateState =
  | { phase: "idle" }
  | { phase: "checking" }
  | { phase: "available"; update: Update }
  | { phase: "downloading"; update: Update; done: number; total: number | null }
  | { phase: "restarting"; update: Update };

/**
 * App version plus the update found on GitHub Releases, if any. Background
 * checks stay silent on failure (offline, no release yet); a manual check
 * reports the outcome.
 */
export function useUpdater() {
  const [version, setVersion] = useState<string | null>(null);
  const [state, setState] = useState<UpdateState>({ phase: "idle" });

  const runCheck = useCallback(async (manual: boolean) => {
    setState((s) => (s.phase === "idle" ? { phase: "checking" } : s));
    try {
      const update = await check();
      setState((s) => {
        if (s.phase !== "idle" && s.phase !== "checking") return s; // already downloading
        return update ? { phase: "available", update } : { phase: "idle" };
      });
      if (manual && !update) toast.success("Puplify is up to date");
    } catch (e) {
      setState((s) => (s.phase === "checking" ? { phase: "idle" } : s));
      if (manual) toast.error(`Couldn't check for updates: ${errorText(e)}`);
    }
  }, []);

  useEffect(() => {
    getVersion().then(setVersion).catch(() => {});
    runCheck(false);
    const timer = setInterval(() => runCheck(false), CHECK_EVERY_MS);
    return () => clearInterval(timer);
  }, [runCheck]);

  /** Downloads, verifies and installs the update, then restarts Puplify. */
  const install = useCallback(async () => {
    if (state.phase !== "available") return;
    const { update } = state;
    let done = 0;
    let total: number | null = null;
    setState({ phase: "downloading", update, done, total });
    try {
      // Installing follows the download inside this call; on Linux deb/rpm it asks for the admin password.
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") total = event.data.contentLength ?? null;
        if (event.event === "Progress") done += event.data.chunkLength;
        setState({ phase: "downloading", update, done, total });
      });
      setState({ phase: "restarting", update });
      await relaunch();
    } catch (e) {
      toast.error(`Update failed: ${errorText(e)}`);
      setState({ phase: "available", update });
    }
  }, [state]);

  return { version, state, check: () => runCheck(true), install };
}
