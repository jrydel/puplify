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
  connectors: {
    folder: { enabled: boolean; path: string; formats: ExportFormat[] };
    onedrive: { enabled: boolean; client_id: string; tenant: string; folder: string; formats: ExportFormat[] };
    google_drive: { enabled: boolean; client_id: string; client_secret: string; folder: string; formats: ExportFormat[] };
    dropbox: { enabled: boolean; app_key: string; folder: string; formats: ExportFormat[] };
    notion: { enabled: boolean; token: string; parent_page_id: string };
    webhooks: WebhookConfig[];
  };
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
}

export interface DeviceLogin {
  user_code: string;
  verification_uri: string;
  expires_in: number;
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

export interface LoginResult {
  provider: "onedrive" | "google_drive";
  ok: boolean;
  account: string | null;
  error: string | null;
}

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
  recordingStatus: () => invoke<{ transcript_id: string; title: string } | null>("recording_status"),
  startRecording: (title: string) => invoke<{ transcript_id: string; title: string }>("start_recording", { title }),
  stopRecording: () => invoke<void>("stop_recording"),
  listTranscripts: () => invoke<TranscriptSummary[]>("list_transcripts"),
  getTranscript: (id: string) => invoke<{ transcript: Transcript; text: string }>("get_transcript", { id }),
  renameTranscript: (id: string, title: string) => invoke<void>("rename_transcript", { id, title }),
  deleteTranscript: (id: string) => invoke<void>("delete_transcript", { id }),
  deliverTranscript: (id: string) => invoke<DeliveryResult[]>("deliver_transcript", { id }),
  listModels: () => invoke<ModelStatus[]>("list_models"),
  downloadModel: (name: string) => invoke<void>("download_model", { name }),
  onedriveStatus: () => invoke<string | null>("onedrive_status"),
  onedriveLogin: () => invoke<DeviceLogin>("onedrive_login"),
  onedriveLogout: () => invoke<void>("onedrive_logout"),
  gdriveStatus: () => invoke<string | null>("gdrive_status"),
  gdriveLogin: () => invoke<DeviceLogin>("gdrive_login"),
  gdriveLogout: () => invoke<void>("gdrive_logout"),
  dropboxStatus: () => invoke<string | null>("dropbox_status"),
  dropboxBeginLogin: () => invoke<{ url: string; verifier: string }>("dropbox_begin_login"),
  dropboxFinishLogin: (verifier: string, code: string) => invoke<string>("dropbox_finish_login", { verifier, code }),
  dropboxLogout: () => invoke<void>("dropbox_logout"),
  testWebhook: (index: number) => invoke<string>("test_webhook", { index }),
};

export function onSession(handler: (event: SessionEvent) => void): Promise<UnlistenFn> {
  return listen<SessionEvent>("session", (e) => handler(e.payload));
}

export function onLogin(handler: (result: LoginResult) => void): Promise<UnlistenFn> {
  return listen<LoginResult>("account-login", (e) => handler(e.payload));
}

export function onModelProgress(handler: (p: ModelProgress) => void): Promise<UnlistenFn> {
  return listen<ModelProgress>("model-progress", (e) => handler(e.payload));
}

/** Commands reject with the Rust error string. */
export function errorText(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
}
