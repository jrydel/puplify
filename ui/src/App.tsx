import { useCallback, useEffect, useReducer, useRef, useState } from "react";
import { toast } from "sonner";

import { AppSidebar } from "@/components/app-sidebar";
import { api, errorText, onSession, type Config, type TranscriptSummary } from "@/lib/api";
import { initialSession, sessionReducer } from "@/lib/session";
import { RecorderView } from "@/views/recorder-view";
import { SettingsView } from "@/views/settings-view";
import { TranscriptView } from "@/views/transcript-view";

type View = { kind: "record" } | { kind: "transcript"; id: string } | { kind: "settings" };

export default function App() {
  const [view, setView] = useState<View>({ kind: "record" });
  const [history, setHistory] = useState<TranscriptSummary[]>([]);
  const [config, setConfig] = useState<Config | null>(null);
  const [session, dispatch] = useReducer(sessionReducer, initialSession);
  const phase = useRef(session.phase);
  phase.current = session.phase;

  const refreshHistory = useCallback(() => {
    api.listTranscripts().then(setHistory).catch((e) => toast.error(errorText(e)));
  }, []);

  useEffect(() => {
    refreshHistory();
    api.getConfig().then(setConfig).catch((e) => toast.error(errorText(e)));
    // The window may have been reloaded while a recording was running.
    api.recordingStatus().then((active) => {
      if (active) dispatch({ type: "recording", transcriptId: active.transcript_id, title: active.title, startedAt: Date.now() });
    });
  }, [refreshHistory]);

  useEffect(() => {
    const off = onSession((event) => {
      dispatch({ type: "event", event });
      if (event.type === "error") toast.error(event.message);
      if (event.type === "finished") {
        // Transcription can end on its own (e.g. the cloud stream dropped); release the recorder.
        if (phase.current === "recording") api.stopRecording().catch((e) => toast.error(errorText(e)));
        refreshHistory();
      }
      if (event.type === "done") refreshHistory();
    });
    return () => {
      off.then((f) => f());
    };
  }, [refreshHistory]);

  async function start(title: string) {
    dispatch({ type: "starting", title: title.trim() || "Untitled meeting" });
    try {
      const info = await api.startRecording(title);
      dispatch({ type: "recording", transcriptId: info.transcript_id, title: info.title, startedAt: Date.now() });
      refreshHistory();
    } catch (e) {
      dispatch({ type: "start-failed" });
      toast.error(errorText(e), {
        action: { label: "Open settings", onClick: () => setView({ kind: "settings" }) },
      });
    }
  }

  async function stop() {
    dispatch({ type: "stopping" });
    try {
      await api.stopRecording();
    } catch (e) {
      toast.error(errorText(e));
    }
  }

  function openRecorder() {
    if (session.phase === "done") dispatch({ type: "reset" });
    setView({ kind: "record" });
  }

  const active = session.phase === "recording" || session.phase === "starting" || session.phase === "finishing";

  return (
    <div className="flex h-svh overflow-hidden">
      <AppSidebar
        history={history}
        selectedId={view.kind === "transcript" ? view.id : null}
        liveId={session.transcriptId}
        phase={session.phase}
        settingsOpen={view.kind === "settings"}
        onRecord={openRecorder}
        onSelect={(id) =>
          active && id === session.transcriptId ? setView({ kind: "record" }) : setView({ kind: "transcript", id })
        }
        onSettings={() => setView({ kind: "settings" })}
      />
      <main className="min-w-0 flex-1 overflow-y-auto px-10 py-8">
        {view.kind === "record" && (
          <RecorderView
            session={session}
            config={config}
            onStart={start}
            onStop={stop}
            onNew={() => dispatch({ type: "reset" })}
            onOpenTranscript={(id) => setView({ kind: "transcript", id })}
            onOpenSettings={() => setView({ kind: "settings" })}
          />
        )}
        {view.kind === "transcript" && (
          <TranscriptView
            key={view.id}
            id={view.id}
            onChanged={refreshHistory}
            onDeleted={() => {
              refreshHistory();
              setView({ kind: "record" });
            }}
          />
        )}
        {view.kind === "settings" && <SettingsView onSaved={setConfig} />}
      </main>
    </div>
  );
}
