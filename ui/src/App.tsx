import { useCallback, useEffect, useReducer, useRef, useState } from "react";
import { toast } from "sonner";

import { AppSidebar } from "@/components/app-sidebar";
import { CallPrompt } from "@/components/call-prompt";
import {
  api,
  errorText,
  onCalendar,
  onCallPrompt,
  onRecordingStarted,
  onRecordingStopped,
  onSession,
  type CalendarSnapshot,
  type Config,
  type Call,
  type RecordingInfo,
  type TranscriptSummary,
} from "@/lib/api";
import { initialSession, sessionReducer } from "@/lib/session";
import { RecorderView } from "@/views/recorder-view";
import { SettingsView } from "@/views/settings-view";
import { TranscriptView } from "@/views/transcript-view";

type View = { kind: "record" } | { kind: "transcript"; id: string } | { kind: "settings" };

export default function App() {
  const [view, setView] = useState<View>({ kind: "record" });
  const [history, setHistory] = useState<TranscriptSummary[]>([]);
  const [config, setConfig] = useState<Config | null>(null);
  const [calendar, setCalendar] = useState<CalendarSnapshot | null>(null);
  const [offer, setOffer] = useState<Call | null>(null);
  const [session, dispatch] = useReducer(sessionReducer, initialSession);
  const current = useRef(session);
  current.current = session;

  const refreshHistory = useCallback(() => {
    api.listTranscripts().then(setHistory).catch((e) => toast.error(errorText(e)));
  }, []);

  /** Follows a recording whether the user, the calendar or a reload started it. */
  const follow = useCallback((info: RecordingInfo) => {
    const s = current.current;
    if (s.phase === "recording" && s.transcriptId === info.transcript_id) return;
    dispatch({ type: "starting", title: info.title });
    dispatch({
      type: "recording",
      transcriptId: info.transcript_id,
      title: info.title,
      startedAt: Date.now(),
      call: info.call,
    });
  }, []);

  const refreshCalendar = useCallback((cfg: Config) => {
    const c = cfg.calendar;
    const enabled = c.mode !== "off" && (c.google || c.microsoft || c.links.some((l) => l.trim()));
    if (!enabled) return setCalendar(null);
    api.upcomingMeetings().then(setCalendar).catch((e) => toast.error(errorText(e)));
  }, []);

  useEffect(() => {
    refreshHistory();
    api
      .getConfig()
      .then((cfg) => {
        setConfig(cfg);
        refreshCalendar(cfg);
      })
      .catch((e) => toast.error(errorText(e)));
    api.recordingStatus().then((active) => active && follow(active));
  }, [refreshHistory, refreshCalendar, follow]);

  useEffect(() => {
    const offs = [
      onSession((event) => {
        dispatch({ type: "event", event });
        if (event.type === "error") toast.error(event.message);
        if (event.type === "finished") {
          // Transcription can end on its own (e.g. the cloud stream dropped); release the recorder.
          if (current.current.phase === "recording") api.stopRecording().catch((e) => toast.error(errorText(e)));
          refreshHistory();
        }
        if (event.type === "done") refreshHistory();
      }),
      onRecordingStarted((info) => {
        follow(info);
        refreshHistory();
        setOffer(null);
        if (info.call) toast.info(`Recording “${info.title}”`);
      }),
      onRecordingStopped((id) => {
        const s = current.current;
        if (s.phase === "recording" && s.transcriptId === id) dispatch({ type: "stopping" });
      }),
      onCallPrompt(setOffer),
      onCalendar(setCalendar),
    ];
    return () => {
      for (const off of offs) off.then((f) => f());
    };
  }, [refreshHistory, follow]);

  async function start(title: string) {
    dispatch({ type: "starting", title: title.trim() || "Untitled meeting" });
    try {
      follow(await api.startRecording(title));
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
            calendar={calendar}
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
              // Same render as leaving the page, so the sidebar doesn't change a frame later.
              setHistory((items) => items.filter((t) => t.id !== view.id));
              setView({ kind: "record" });
            }}
          />
        )}
        {view.kind === "settings" && (
          <SettingsView
            onSaved={(cfg) => {
              setConfig(cfg);
              refreshCalendar(cfg);
            }}
          />
        )}
      </main>
      <CallPrompt
        call={offer}
        onRecord={async () => {
          setOffer(null);
          try {
            follow(await api.recordOfferedCall());
            setView({ kind: "record" });
          } catch (e) {
            toast.error(errorText(e));
          }
        }}
        onDismiss={() => {
          setOffer(null);
          api.dismissOfferedCall().catch(() => {});
        }}
      />
    </div>
  );
}
