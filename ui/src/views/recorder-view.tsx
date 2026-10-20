import { useEffect, useRef, useState } from "react";
import { CalendarClockIcon, FileTextIcon, MicIcon, SquareIcon, Volume2Icon } from "lucide-react";

import { LevelMeter } from "@/components/level-meter";
import { DeliveryList } from "@/components/delivery-list";
import { TapeStrip } from "@/components/tape-strip";
import { TranscriptBody } from "@/components/transcript-body";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card";
import { Empty, EmptyDescription, EmptyHeader, EmptyTitle } from "@/components/ui/empty";
import { Separator } from "@/components/ui/separator";
import { Spinner } from "@/components/ui/spinner";
import type { CalendarSnapshot, Config, Meeting } from "@/lib/api";
import { fmtTs, timeOfDay } from "@/lib/format";
import type { SessionState } from "@/lib/session";
import { cn } from "@/lib/utils";

interface RecorderViewProps {
  session: SessionState;
  config: Config | null;
  calendar: CalendarSnapshot | null;
  onStart: (title: string) => void;
  onStop: () => void;
  onNew: () => void;
  onOpenTranscript: (id: string) => void;
  onOpenSettings: () => void;
}

const PROVIDER: Record<Meeting["provider"], string> = { google: "Google Calendar", microsoft: "Outlook" };

/** The next (or current) online meeting from the connected calendars. */
function NextMeeting({ calendar, config }: { calendar: CalendarSnapshot; config: Config }) {
  const now = Date.now();
  const next = calendar.meetings.find((m) => new Date(m.end).getTime() > now);
  const live = next && new Date(next.start).getTime() - 5 * 60_000 <= now;
  const behaviour =
    config.calendar.mode === "auto" ? "Recording starts when you join the call." : "Puplify asks when you join the call.";
  return (
    <Card size="sm">
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <CalendarClockIcon className="size-4 text-muted-foreground" aria-hidden />
          {next ? (live ? "Meeting in progress" : "Next meeting") : "No more online meetings today"}
        </CardTitle>
        {next ? (
          <CardDescription>
            <span className="font-medium text-foreground">{next.title}</span> · {timeOfDay(new Date(next.start))}–
            {timeOfDay(new Date(next.end))} · {PROVIDER[next.provider]}. {behaviour}
          </CardDescription>
        ) : (
          <CardDescription>Puplify watches your calendar for meetings with a video link.</CardDescription>
        )}
      </CardHeader>
      {calendar.warnings.length > 0 && (
        <CardContent className="flex flex-col gap-1 text-sm text-destructive">
          {calendar.warnings.map((w) => (
            <p key={w}>{w}</p>
          ))}
        </CardContent>
      )}
    </Card>
  );
}

export function RecorderView(props: RecorderViewProps) {
  return props.session.phase === "idle" || props.session.phase === "starting" ? (
    <ReadyPanel {...props} />
  ) : (
    <LivePanel {...props} />
  );
}

function ReadyPanel({ session, config, calendar, onStart, onOpenSettings }: RecorderViewProps) {
  const [title, setTitle] = useState("");
  const starting = session.phase === "starting";
  const capture = config?.capture;
  const noSource = capture ? !capture.mic && !capture.system : false;

  return (
    <div className="mx-auto flex w-full max-w-xl flex-col gap-6 pt-[12vh]">
      <form
        className="flex flex-col gap-6"
        onSubmit={(e) => {
          e.preventDefault();
          onStart(title);
        }}
      >
        <label className="flex flex-col gap-2">
          <span className="text-sm text-muted-foreground">Meeting title</span>
          <input
            autoFocus
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            placeholder="Untitled meeting"
            disabled={starting}
            className="w-full border-b border-border bg-transparent pb-2 font-heading text-3xl font-semibold tracking-tight outline-none placeholder:text-muted-foreground/50 focus-visible:border-ring"
          />
        </label>

        {calendar && config && <NextMeeting calendar={calendar} config={config} />}

        <Card size="sm">
          <CardHeader>
            <CardTitle>What gets recorded</CardTitle>
            <CardDescription>
              {!config
                ? "Loading settings…"
                : config.stt.provider === "whisper"
                  ? `Local whisper.cpp · ${config.stt.whisper.model}`
                  : `Deepgram · ${config.stt.deepgram.model}`}
            </CardDescription>
          </CardHeader>
          <CardContent className="flex flex-col gap-3">
            <SourceRow
              icon={MicIcon}
              color="text-me"
              label={capture?.me_label ?? "Me"}
              source="Microphone"
              device={capture?.mic_device ?? "System default"}
              enabled={capture?.mic ?? true}
            />
            <SourceRow
              icon={Volume2Icon}
              color="text-them"
              label={capture?.them_label ?? "Them"}
              source="System audio"
              device={capture?.system_device ?? "Everything you hear"}
              enabled={capture?.system ?? true}
            />
          </CardContent>
          <CardFooter>
            <Button type="button" variant="link" className="h-auto px-0" onClick={onOpenSettings}>
              Change sources or engine
            </Button>
          </CardFooter>
        </Card>

        <Button type="submit" size="lg" disabled={starting || noSource || !config} className="h-12 text-base">
          {starting ? <Spinner data-icon="inline-start" /> : <span className="size-3 rounded-full bg-tally" aria-hidden />}
          {starting ? "Starting…" : "Start recording"}
        </Button>
        {noSource && (
          <p className="text-sm text-destructive">Turn on the microphone or system audio in Settings to record.</p>
        )}
      </form>
    </div>
  );
}

function SourceRow({
  icon: Icon,
  color,
  label,
  source,
  device,
  enabled,
}: {
  icon: typeof MicIcon;
  color: string;
  label: string;
  source: string;
  device: string;
  enabled: boolean;
}) {
  return (
    <div className={cn("flex items-center gap-3 text-sm", !enabled && "opacity-50")}>
      <Icon className={cn("size-4 shrink-0", color)} aria-hidden />
      <span className={cn("w-20 shrink-0 font-semibold", color)}>{label}</span>
      <span className="min-w-0 flex-1 truncate">
        {source} <span className="text-muted-foreground">· {enabled ? device : "off"}</span>
      </span>
    </div>
  );
}

function LivePanel({ session, config, onStop, onNew, onOpenTranscript }: RecorderViewProps) {
  const [now, setNow] = useState(Date.now());
  const bottom = useRef<HTMLDivElement>(null);
  const recording = session.phase === "recording";
  const labels = { me: config?.capture.me_label ?? "Me", them: config?.capture.them_label ?? "Them" };
  const elapsed = session.startedAt ? (recording ? now : (session.stoppedAt ?? now)) - session.startedAt : 0;

  useEffect(() => {
    if (!recording) return;
    const id = window.setInterval(() => setNow(Date.now()), 500);
    return () => window.clearInterval(id);
  }, [recording]);

  // Follow new text while recording.
  useEffect(() => {
    if (recording) bottom.current?.scrollIntoView({ block: "end" });
  }, [recording, session.segments.length, session.partials]);

  return (
    <div className="mx-auto flex w-full max-w-3xl flex-col gap-6">
      <header className="sticky top-0 z-10 -mx-4 flex flex-col gap-5 bg-background/90 px-4 pt-2 pb-4 backdrop-blur">
        <div className="flex items-center gap-4">
          <div className="flex min-w-0 flex-1 flex-col">
            <h1 className="truncate font-heading text-2xl font-semibold tracking-tight">{session.title}</h1>
            {session.meeting && (
              <p className="flex items-center gap-1.5 text-sm text-muted-foreground">
                <CalendarClockIcon className="size-3.5" aria-hidden />
                From {PROVIDER[session.meeting.provider]} · stops when you leave the call
              </p>
            )}
          </div>
          {recording && (
            <span className="flex items-center gap-2 font-mono text-sm tabular-nums">
              <span className="size-2.5 animate-pulse rounded-full bg-tally" aria-hidden />
              <span className="sr-only">Recording, elapsed</span>
              {fmtTs(elapsed)}
            </span>
          )}
          {recording ? (
            <Button onClick={onStop}>
              <SquareIcon data-icon="inline-start" />
              Stop
            </Button>
          ) : session.phase === "done" ? (
            <div className="flex gap-2">
              {session.transcriptId && (
                <Button variant="outline" onClick={() => onOpenTranscript(session.transcriptId!)}>
                  <FileTextIcon data-icon="inline-start" />
                  Open transcript
                </Button>
              )}
              <Button onClick={onNew}>New recording</Button>
            </div>
          ) : (
            <span className="flex items-center gap-2 text-sm text-muted-foreground">
              <Spinner /> Finishing transcription…
            </span>
          )}
        </div>

        {recording && (
          <div className="flex gap-6">
            <LevelMeter
              speaker="me"
              label={labels.me}
              level={session.levels.me}
              source={config?.capture.mic ? "Microphone" : "Off"}
            />
            <LevelMeter
              speaker="them"
              label={labels.them}
              level={session.levels.them}
              source={config?.capture.system ? "System audio" : "Off"}
            />
          </div>
        )}

        <TapeStrip segments={session.segments} durationMs={elapsed} labels={labels} live={recording} />
      </header>

      <DeliveryList results={session.deliveries} />

      <Separator />

      {session.segments.length === 0 && !Object.values(session.partials).some(Boolean) ? (
        <Empty>
          <EmptyHeader>
            <EmptyTitle>{recording ? "Listening" : "Nothing was transcribed"}</EmptyTitle>
            <EmptyDescription>
              {recording
                ? config?.stt.provider === "deepgram"
                  ? "Text appears as people speak."
                  : "Text appears a few seconds after each pause."
                : "Check that the meters moved while you recorded."}
            </EmptyDescription>
          </EmptyHeader>
        </Empty>
      ) : (
        <TranscriptBody segments={session.segments} labels={labels} partials={session.partials} />
      )}
      <div ref={bottom} />
    </div>
  );
}
