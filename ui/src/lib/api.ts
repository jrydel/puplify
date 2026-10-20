import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/* ---- types mirroring crates/core ---- */

export type Speaker = "me" | "them";
export type ExportFormat = "txt" | "md" | "json";
export type WebhookEvent = "recording.started" | "transcript.segment" | "recording.completed";

export interface Segment {
  speaker: Speaker;
  start_ms: number;
  end_ms: number;
  text: string;
}

export interface Transcript {
  id: string;
  title: string;
  started_at: string;
  ended_at: string | null;
  engine: string;
  labels: { me: string; them: string };
  segments: Segment[];
}

export interface TranscriptSummary {
  id: string;
  title: string;
  started_at: string;
  ended_at: string | null;
  duration_ms: number;
  segments: number;
}

export interface DeliveryResult {
  connector: string;
  ok: boolean;
  detail: string;
}

export interface WebhookConfig {
  name: string;
  url: string;
  secret: string;
  enabled: boolean;
  events: WebhookEvent[];
}

export interface Config {
  capture: {
    mic: boolean;
    system: boolean;
    mic_device: string | null;
    system_device: string | null;
    me_label: string;
    them_label: string;
  };
  stt: {
    provider: "whisper" | "deepgram";
    whisper: { model: string; language: string; threads: number };
    deepgram: { api_key: string; model: string; language: string };
  };
  calendar: { mode: AutoRecord; calls: AutoRecord; google: boolean; microsoft: boolean; links: string[] };
  connectors: {
    folder: { enabled: boolean; path: string; formats: ExportFormat[] };
    onedrive: { enabled: boolean; folder: string; formats: ExportFormat[] };
    google_drive: { enabled: boolean; folder: string; formats: ExportFormat[] };
    dropbox: { enabled: boolean; folder: string; formats: ExportFormat[] };
    notion: { enabled: boolean; token: string; parent_page_id: string };
    webhooks: WebhookConfig[];
  };
}

export type AutoRecord = "auto" | "ask" | "off";

export interface Meeting {
  id: string;
  title: string;
  start: string;
  end: string;
  join_url: string | null;
  provider: "google" | "microsoft" | "ical";
}

export interface CalendarSnapshot {
  meetings: Meeting[];
  warnings: string[];
}

/** A call the autopilot noticed: another app took the microphone, during a calendar meeting or not. */
export interface Call {
  title: string;
  meeting: Meeting | null;
  /** App holding the microphone, when the OS names it. */
  app: string | null;
}

export interface RecordingInfo {
  transcript_id: string;
  title: string;
  /** Set when the autopilot (or its Ask prompt) started the recording. */
  call: Call | null;
}

export interface DeviceInfo {
  id: string;
  name: string;
  is_monitor: boolean;
}

export interface ModelStatus {
  name: string;
  size_mb: number;
  downloaded: boolean;
  bundled: boolean;
}

/** Services whose sign-in works without the user's own app registration in this build. */
export interface BuiltinAccounts {
  microsoft: boolean;
  google: boolean;
  dropbox: boolean;
}

export type SessionEvent =
  | { type: "started"; transcript_id: string; title: string; engine: string }
  | { type: "partial"; speaker: Speaker; text: string }
  | { type: "segment"; segment: Segment }
  | { type: "error"; message: string }
  | { type: "levels"; me: number; them: number }
  | { type: "finished"; transcript_id: string }
  | { type: "delivery"; result: DeliveryResult }
  | { type: "done"; transcript_id: string };

export interface ModelProgress {
  name: string;
  done: number;
  total: number | null;
}

/* ---- commands ---- */

export const api = {
  getConfig: () => invoke<Config>("get_config"),
  saveConfig: (config: Config) => invoke<void>("save_config", { config }),
  listDevices: () => invoke<DeviceInfo[]>("list_devices"),
  recordingStatus: () => invoke<RecordingInfo | null>("recording_status"),
  startRecording: (title: string) => invoke<RecordingInfo>("start_recording", { title }),
  recordOfferedCall: () => invoke<RecordingInfo>("record_offered_call"),
  dismissOfferedCall: () => invoke<void>("dismiss_offered_call"),
  stopRecording: () => invoke<void>("stop_recording"),
  listTranscripts: () => invoke<TranscriptSummary[]>("list_transcripts"),
  getTranscript: (id: string) => invoke<{ transcript: Transcript; text: string }>("get_transcript", { id }),
  renameTranscript: (id: string, title: string) => invoke<void>("rename_transcript", { id, title }),
  deleteTranscript: (id: string) => invoke<void>("delete_transcript", { id }),
  deliverTranscript: (id: string) => invoke<DeliveryResult[]>("deliver_transcript", { id }),
  listModels: () => invoke<ModelStatus[]>("list_models"),
  downloadModel: (name: string) => invoke<void>("download_model", { name }),
  builtinAccounts: () => invoke<BuiltinAccounts>("builtin_accounts"),
  /** Each login opens the browser and resolves with the account once the user approves. */
  microsoftStatus: () => invoke<string | null>("microsoft_status"),
  microsoftLogin: () => invoke<string>("microsoft_login"),
  microsoftLogout: () => invoke<void>("microsoft_logout"),
  googleStatus: () => invoke<string | null>("google_status"),
  googleLogin: () => invoke<string>("google_login"),
  googleLogout: () => invoke<void>("google_logout"),
  dropboxStatus: () => invoke<string | null>("dropbox_status"),
  dropboxLogin: () => invoke<string>("dropbox_login"),
  dropboxLogout: () => invoke<void>("dropbox_logout"),
  /** Aborts the sign-in that is waiting for the browser. */
  cancelLogin: () => invoke<void>("cancel_login"),
  upcomingMeetings: () => invoke<CalendarSnapshot>("upcoming_meetings"),
  testWebhook: (index: number) => invoke<string>("test_webhook", { index }),
};

export function onSession(handler: (event: SessionEvent) => void): Promise<UnlistenFn> {
  return listen<SessionEvent>("session", (e) => handler(e.payload));
}

export function onModelProgress(handler: (p: ModelProgress) => void): Promise<UnlistenFn> {
  return listen<ModelProgress>("model-progress", (e) => handler(e.payload));
}

/** A recording started outside the UI (calendar autopilot) or by it. */
export function onRecordingStarted(handler: (info: RecordingInfo) => void): Promise<UnlistenFn> {
  return listen<RecordingInfo>("recording-started", (e) => handler(e.payload));
}

/** Capture stopped (by the user or the calendar autopilot); transcription is finishing. */
export function onRecordingStopped(handler: (transcriptId: string) => void): Promise<UnlistenFn> {
  return listen<string>("recording-stopped", (e) => handler(e.payload));
}

/** Ask mode: the user joined a call (a calendar meeting or any other). */
export function onCallPrompt(handler: (call: Call) => void): Promise<UnlistenFn> {
  return listen<Call>("call-prompt", (e) => handler(e.payload));
}

export function onCalendar(handler: (snapshot: CalendarSnapshot) => void): Promise<UnlistenFn> {
  return listen<CalendarSnapshot>("calendar", (e) => handler(e.payload));
}

/** Commands reject with the Rust error string. */
export function errorText(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
}
