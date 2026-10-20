import { useCallback, useEffect, useState, type ReactNode } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { DownloadIcon, ExternalLinkIcon, PlusIcon, SendIcon, Trash2Icon } from "lucide-react";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Field,
  FieldContent,
  FieldDescription,
  FieldGroup,
  FieldLabel,
  FieldLegend,
  FieldSet,
} from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Progress } from "@/components/ui/progress";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Spinner } from "@/components/ui/spinner";
import { Switch } from "@/components/ui/switch";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import {
  api,
  errorText,
  onLogin,
  onModelProgress,
  type Config,
  type DeviceInfo,
  type DeviceLogin,
  type ExportFormat,
  type ModelProgress,
  type ModelStatus,
  type WebhookEvent,
} from "@/lib/api";

/** Radix Select can't use "" as a value; this stands for "use the default device". */
const DEFAULT_DEVICE = "__default";

const WEBHOOK_EVENTS: { id: WebhookEvent; label: string; hint: string }[] = [
  { id: "recording.started", label: "Recording started", hint: "Title and start time" },
  { id: "transcript.segment", label: "Each new line", hint: "Sent live while recording" },
  { id: "recording.completed", label: "Recording finished", hint: "Full transcript text and segments" },
];

type Accounts = Record<"onedrive" | "google_drive" | "dropbox", string | null>;

export function SettingsView({ onSaved }: { onSaved: (config: Config) => void }) {
  const [saved, setSaved] = useState<Config | null>(null);
  const [draft, setDraft] = useState<Config | null>(null);
  const [models, setModels] = useState<ModelStatus[]>([]);
  const [devices, setDevices] = useState<DeviceInfo[]>([]);
  const [accounts, setAccounts] = useState<Accounts>({ onedrive: null, google_drive: null, dropbox: null });
  const [saving, setSaving] = useState(false);

  const refreshAccounts = useCallback(async () => {
    const [onedrive, google_drive, dropbox] = await Promise.all([
      api.onedriveStatus().catch(() => null),
      api.gdriveStatus().catch(() => null),
      api.dropboxStatus().catch(() => null),
    ]);
    setAccounts({ onedrive, google_drive, dropbox });
  }, []);

  useEffect(() => {
    api
      .getConfig()
      .then((c) => {
        setSaved(c);
        setDraft(c);
      })
      .catch((e) => toast.error(errorText(e)));
    api.listModels().then(setModels).catch((e) => toast.error(errorText(e)));
    api.listDevices().then(setDevices).catch((e) => toast.error(`Couldn't list audio devices: ${errorText(e)}`));
    refreshAccounts();
  }, [refreshAccounts]);

  if (!draft || !saved) return null;

  const dirty = JSON.stringify(draft) !== JSON.stringify(saved);

  /** Apply a mutation to a copy of the draft. */
  const edit = (fn: (c: Config) => void) =>
    setDraft((prev) => {
      if (!prev) return prev;
      const next = structuredClone(prev);
      fn(next);
      return next;
    });

  async function save(): Promise<boolean> {
    if (!draft) return false;
    setSaving(true);
    try {
      await api.saveConfig(draft);
      setSaved(draft);
      onSaved(draft);
      return true;
    } catch (e) {
      toast.error(errorText(e));
      return false;
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="mx-auto flex w-full max-w-3xl flex-col gap-6 pb-24">
      <h1 className="font-heading text-3xl font-semibold tracking-tight">Settings</h1>
      <Tabs defaultValue="transcription">
        <TabsList>
          <TabsTrigger value="transcription">Transcription</TabsTrigger>
          <TabsTrigger value="audio">Audio</TabsTrigger>
          <TabsTrigger value="destinations">Destinations</TabsTrigger>
          <TabsTrigger value="webhooks">Webhooks</TabsTrigger>
        </TabsList>

        <TabsContent value="transcription" className="pt-4">
          <TranscriptionTab draft={draft} edit={edit} models={models} onModels={setModels} />
        </TabsContent>
        <TabsContent value="audio" className="pt-4">
          <AudioTab draft={draft} edit={edit} devices={devices} />
        </TabsContent>
        <TabsContent value="destinations" className="pt-4">
          <DestinationsTab draft={draft} edit={edit} accounts={accounts} save={save} refreshAccounts={refreshAccounts} />
        </TabsContent>
        <TabsContent value="webhooks" className="pt-4">
          <WebhooksTab draft={draft} edit={edit} save={save} />
        </TabsContent>
      </Tabs>

      <div className="fixed right-0 bottom-0 left-72 border-t bg-background/95 px-10 backdrop-blur">
        <div className="mx-auto flex w-full max-w-3xl items-center justify-end gap-3 py-3">
          <span className="mr-auto text-sm text-muted-foreground">{dirty ? "You have unsaved changes." : "All changes saved."}</span>
          <Button variant="ghost" disabled={!dirty || saving} onClick={() => setDraft(saved)}>
            Discard
          </Button>
          <Button
            disabled={!dirty || saving}
            onClick={async () => {
              if (await save()) toast.success("Changes saved");
            }}
          >
            {saving && <Spinner data-icon="inline-start" />}
            Save changes
          </Button>
        </div>
      </div>
    </div>
  );
}

type Edit = (fn: (c: Config) => void) => void;

/* ---------------- Transcription ---------------- */

function TranscriptionTab({
  draft,
  edit,
  models,
  onModels,
}: {
  draft: Config;
  edit: Edit;
  models: ModelStatus[];
  onModels: (m: ModelStatus[]) => void;
}) {
  const [progress, setProgress] = useState<ModelProgress | null>(null);
  const model = draft.stt.whisper.model;
  const selected = models.find((m) => m.name === model);

  useEffect(() => {
    const off = onModelProgress(setProgress);
    return () => {
      off.then((f) => f());
    };
  }, []);

  async function download() {
    setProgress({ name: model, done: 0, total: null });
    try {
      await api.downloadModel(model);
      onModels(await api.listModels());
      toast.success(`${model} downloaded`);
    } catch (e) {
      toast.error(errorText(e));
    } finally {
      setProgress(null);
    }
  }

  return (
    <FieldGroup>
      <Field>
        <FieldLabel>Engine</FieldLabel>
        <ToggleGroup
          type="single"
          variant="outline"
          value={draft.stt.provider}
          onValueChange={(v) => v && edit((c) => (c.stt.provider = v as Config["stt"]["provider"]))}
          className="justify-start"
        >
          <ToggleGroupItem value="whisper">On this computer</ToggleGroupItem>
          <ToggleGroupItem value="deepgram">Deepgram (cloud)</ToggleGroupItem>
        </ToggleGroup>
        <FieldDescription>
          {draft.stt.provider === "whisper"
            ? "whisper.cpp runs locally. Audio never leaves your computer; text appears a few seconds after each pause."
            : "Audio streams to Deepgram for live text with exact speaker separation. Needs an API key."}
        </FieldDescription>
      </Field>

      {draft.stt.provider === "whisper" ? (
        <>
          <Field>
            <FieldLabel htmlFor="model">Model</FieldLabel>
            <div className="flex gap-2">
              <Select value={model} onValueChange={(v) => edit((c) => (c.stt.whisper.model = v))}>
                <SelectTrigger id="model" className="flex-1">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    {models.map((m) => (
                      <SelectItem key={m.name} value={m.name}>
                        <span className="font-mono">{m.name}</span>
                        <span className="text-muted-foreground">
                          {m.size_mb} MB{m.downloaded ? " · downloaded" : ""}
                        </span>
                      </SelectItem>
                    ))}
                  </SelectGroup>
                </SelectContent>
              </Select>
              {selected && !selected.downloaded && (
                <Button variant="outline" onClick={download} disabled={!!progress}>
                  <DownloadIcon data-icon="inline-start" />
                  Download
                </Button>
              )}
            </div>
            {progress && (
              <div className="flex items-center gap-3">
                <Progress value={progress.total ? (progress.done / progress.total) * 100 : 0} className="flex-1" />
                <span className="font-mono text-xs text-muted-foreground tabular-nums">
                  {Math.round(progress.done / 1e6)}
                  {progress.total ? ` / ${Math.round(progress.total / 1e6)}` : ""} MB
                </span>
              </div>
            )}
            <FieldDescription>
              <span className="font-mono">base.en</span> is fast on any computer;{" "}
              <span className="font-mono">small.en</span> and <span className="font-mono">large-v3-turbo-q5_0</span> are
              more accurate. Models without <span className="font-mono">.en</span> handle other languages.
            </FieldDescription>
          </Field>
          <Field>
            <FieldLabel htmlFor="language">Language</FieldLabel>
            <Input
              id="language"
              value={draft.stt.whisper.language}
              onChange={(e) => edit((c) => (c.stt.whisper.language = e.target.value))}
              placeholder="en"
              className="max-w-40"
            />
            <FieldDescription>Two-letter code such as en, de or cs, or auto to detect.</FieldDescription>
          </Field>
        </>
      ) : (
        <>
          <Field>
            <FieldLabel htmlFor="dg-key">API key</FieldLabel>
            <Input
              id="dg-key"
              type="password"
              value={draft.stt.deepgram.api_key}
              onChange={(e) => edit((c) => (c.stt.deepgram.api_key = e.target.value))}
            />
            <FieldDescription>Create one at console.deepgram.com.</FieldDescription>
          </Field>
          <div className="grid grid-cols-2 gap-4">
            <Field>
              <FieldLabel htmlFor="dg-model">Model</FieldLabel>
              <Input
                id="dg-model"
                value={draft.stt.deepgram.model}
                onChange={(e) => edit((c) => (c.stt.deepgram.model = e.target.value))}
                placeholder="nova-3"
              />
            </Field>
            <Field>
              <FieldLabel htmlFor="dg-lang">Language</FieldLabel>
              <Input
                id="dg-lang"
                value={draft.stt.deepgram.language}
                onChange={(e) => edit((c) => (c.stt.deepgram.language = e.target.value))}
                placeholder="en or multi"
              />
            </Field>
          </div>
        </>
      )}
    </FieldGroup>
  );
}

/* ---------------- Audio ---------------- */

function AudioTab({ draft, edit, devices }: { draft: Config; edit: Edit; devices: DeviceInfo[] }) {
  const inputs = devices.filter((d) => !d.is_monitor);
  const outputs = devices.filter((d) => d.is_monitor);
  const cap = draft.capture;

  return (
    <div className="flex flex-col gap-4">
      <SourceCard
        title="Microphone"
        description="Your voice."
        accent="bg-me"
        enabled={cap.mic}
        onEnabled={(v) => edit((c) => (c.capture.mic = v))}
        label={cap.me_label}
        onLabel={(v) => edit((c) => (c.capture.me_label = v))}
      >
        <DeviceSelect
          id="mic"
          label="Input device"
          value={cap.mic_device}
          devices={inputs}
          defaultLabel="System default"
          onChange={(v) => edit((c) => (c.capture.mic_device = v))}
        />
      </SourceCard>
      <SourceCard
        title="System audio"
        description="Everyone else in the call, captured from what your computer plays."
        accent="bg-them"
        enabled={cap.system}
        onEnabled={(v) => edit((c) => (c.capture.system = v))}
        label={cap.them_label}
        onLabel={(v) => edit((c) => (c.capture.them_label = v))}
      >
        {outputs.length > 0 ? (
          <DeviceSelect
            id="system"
            label="Output to capture"
            value={cap.system_device}
            devices={outputs}
            defaultLabel="Default output"
            onChange={(v) => edit((c) => (c.capture.system_device = v))}
          />
        ) : (
          <FieldDescription>
            macOS captures all system audio. The first recording asks for Screen &amp; System Audio Recording
            permission.
          </FieldDescription>
        )}
      </SourceCard>
      <p className="text-sm text-muted-foreground">
        Use headphones if you can. Without them your microphone also hears the other people, and their words show
        up twice.
      </p>
    </div>
  );
}

function SourceCard({
  title,
  description,
  accent,
  enabled,
  onEnabled,
  label,
  onLabel,
  children,
}: {
  title: string;
  description: string;
  accent: string;
  enabled: boolean;
  onEnabled: (v: boolean) => void;
  label: string;
  onLabel: (v: string) => void;
  children: ReactNode;
}) {
  const id = title.toLowerCase().replace(/\s+/g, "-");
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <span className={`size-2.5 rounded-full ${accent}`} aria-hidden />
          {title}
        </CardTitle>
        <CardDescription>{description}</CardDescription>
        <CardAction>
          <Switch checked={enabled} onCheckedChange={onEnabled} aria-label={`Record ${title.toLowerCase()}`} />
        </CardAction>
      </CardHeader>
      {enabled && (
        <CardContent>
          <FieldGroup className="grid grid-cols-[1fr_12rem] gap-4">
            {children}
            <Field>
              <FieldLabel htmlFor={`${id}-label`}>Label in transcript</FieldLabel>
              <Input id={`${id}-label`} value={label} onChange={(e) => onLabel(e.target.value)} />
            </Field>
          </FieldGroup>
        </CardContent>
      )}
    </Card>
  );
}

function DeviceSelect({
  id,
  label,
  value,
  devices,
  defaultLabel,
  onChange,
}: {
  id: string;
  label: string;
  value: string | null;
  devices: DeviceInfo[];
  defaultLabel: string;
  onChange: (v: string | null) => void;
}) {
  const missing = value && !devices.some((d) => d.id === value);
  return (
    <Field>
      <FieldLabel htmlFor={id}>{label}</FieldLabel>
      <Select value={value ?? DEFAULT_DEVICE} onValueChange={(v) => onChange(v === DEFAULT_DEVICE ? null : v)}>
        <SelectTrigger id={id} className="w-full min-w-0">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectGroup>
            <SelectItem value={DEFAULT_DEVICE}>{defaultLabel}</SelectItem>
            {devices.map((d) => (
              <SelectItem key={d.id} value={d.id}>
                {d.name}
              </SelectItem>
            ))}
            {missing && <SelectItem value={value}>{value} (not connected)</SelectItem>}
          </SelectGroup>
        </SelectContent>
      </Select>
    </Field>
  );
}

/* ---------------- Destinations ---------------- */

function FormatPicker({ value, onChange }: { value: ExportFormat[]; onChange: (v: ExportFormat[]) => void }) {
  return (
    <Field>
      <FieldLabel>File formats</FieldLabel>
      <ToggleGroup
        type="multiple"
        variant="outline"
        size="sm"
        value={value}
        onValueChange={(v) => onChange(v as ExportFormat[])}
        className="justify-start"
      >
        <ToggleGroupItem value="txt">Text (.txt)</ToggleGroupItem>
        <ToggleGroupItem value="md">Markdown (.md)</ToggleGroupItem>
        <ToggleGroupItem value="json">JSON (.json)</ToggleGroupItem>
      </ToggleGroup>
    </Field>
  );
}

function DestinationCard({
  title,
  description,
  enabled,
  onEnabled,
  account,
  children,
}: {
  title: string;
  description: string;
  enabled: boolean;
  onEnabled: (v: boolean) => void;
  account?: { name: string | null; onConnect: () => void; onDisconnect: () => void; busy: boolean };
  children: ReactNode;
}) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>{title}</CardTitle>
        <CardDescription>{description}</CardDescription>
        <CardAction>
          <Switch checked={enabled} onCheckedChange={onEnabled} aria-label={`Send transcripts to ${title}`} />
        </CardAction>
      </CardHeader>
      {enabled && <CardContent>{children}</CardContent>}
      {enabled && account && (
        <CardFooter className="justify-between border-t">
          <span className="text-sm text-muted-foreground">
            {account.name ? (
              <>
                Connected as <span className="font-medium text-foreground">{account.name}</span>
              </>
            ) : (
              "Not connected"
            )}
          </span>
          {account.name ? (
            <Button variant="ghost" size="sm" onClick={account.onDisconnect}>
              Disconnect
            </Button>
          ) : (
            <Button size="sm" onClick={account.onConnect} disabled={account.busy}>
              {account.busy && <Spinner data-icon="inline-start" />}
              Connect {title}
            </Button>
          )}
        </CardFooter>
      )}
    </Card>
  );
}

function DestinationsTab({
  draft,
  edit,
  accounts,
  save,
  refreshAccounts,
}: {
  draft: Config;
  edit: Edit;
  accounts: Accounts;
  save: () => Promise<boolean>;
  refreshAccounts: () => Promise<void>;
}) {
  const c = draft.connectors;
  const [busy, setBusy] = useState<keyof Accounts | null>(null);
  const [device, setDevice] = useState<{ provider: string; login: DeviceLogin } | null>(null);
  const [dropbox, setDropbox] = useState<{ url: string; verifier: string; code: string } | null>(null);

  useEffect(() => {
    const off = onLogin((r) => {
      setDevice(null);
      setBusy(null);
      if (r.ok) toast.success(`Connected as ${r.account}`);
      else toast.error(r.error ?? "Sign-in failed");
      refreshAccounts();
    });
    return () => {
      off.then((f) => f());
    };
  }, [refreshAccounts]);

  async function connectDevice(provider: "onedrive" | "google_drive", name: string) {
    setBusy(provider);
    try {
      if (!(await save())) return setBusy(null);
      const login = provider === "onedrive" ? await api.onedriveLogin() : await api.gdriveLogin();
      setDevice({ provider: name, login });
      openUrl(login.verification_uri).catch(() => {});
    } catch (e) {
      toast.error(errorText(e));
      setBusy(null);
    }
  }

  async function connectDropbox() {
    setBusy("dropbox");
    try {
      if (!(await save())) return;
      const pending = await api.dropboxBeginLogin();
      setDropbox({ ...pending, code: "" });
      openUrl(pending.url).catch(() => {});
    } catch (e) {
      toast.error(errorText(e));
    } finally {
      setBusy(null);
    }
  }

  async function finishDropbox() {
    if (!dropbox) return;
    setBusy("dropbox");
    try {
      const account = await api.dropboxFinishLogin(dropbox.verifier, dropbox.code);
      toast.success(`Connected as ${account}`);
      setDropbox(null);
      refreshAccounts();
    } catch (e) {
      toast.error(errorText(e));
    } finally {
      setBusy(null);
    }
  }

  async function disconnect(fn: () => Promise<void>) {
    try {
      await fn();
      refreshAccounts();
    } catch (e) {
      toast.error(errorText(e));
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <p className="text-sm text-muted-foreground">
        When a recording finishes, the transcript is saved on this computer and sent to every destination you turn on.
      </p>

      <DestinationCard
        title="OneDrive"
        description="Personal or work/school account."
        enabled={c.onedrive.enabled}
        onEnabled={(v) => edit((d) => (d.connectors.onedrive.enabled = v))}
        account={{
          name: accounts.onedrive,
          busy: busy === "onedrive",
          onConnect: () => connectDevice("onedrive", "OneDrive"),
          onDisconnect: () => disconnect(api.onedriveLogout),
        }}
      >
        <FieldGroup>
          <Field>
            <FieldLabel htmlFor="od-client">Application (client) ID</FieldLabel>
            <Input
              id="od-client"
              className="font-mono"
              value={c.onedrive.client_id}
              onChange={(e) => edit((d) => (d.connectors.onedrive.client_id = e.target.value))}
              placeholder="00000000-0000-0000-0000-000000000000"
            />
            <FieldDescription>From your Microsoft Entra app registration. The README walks through it.</FieldDescription>
          </Field>
          <div className="grid grid-cols-2 gap-4">
            <Field>
              <FieldLabel htmlFor="od-folder">Folder</FieldLabel>
              <Input
                id="od-folder"
                value={c.onedrive.folder}
                onChange={(e) => edit((d) => (d.connectors.onedrive.folder = e.target.value))}
              />
            </Field>
            <Field>
              <FieldLabel htmlFor="od-tenant">Tenant</FieldLabel>
              <Input
                id="od-tenant"
                value={c.onedrive.tenant}
                onChange={(e) => edit((d) => (d.connectors.onedrive.tenant = e.target.value))}
                placeholder="common"
              />
            </Field>
          </div>
          <FormatPicker
            value={c.onedrive.formats}
            onChange={(v) => edit((d) => (d.connectors.onedrive.formats = v))}
          />
        </FieldGroup>
      </DestinationCard>

      <DestinationCard
        title="Google Drive"
        description="Saved to a folder in My Drive."
        enabled={c.google_drive.enabled}
        onEnabled={(v) => edit((d) => (d.connectors.google_drive.enabled = v))}
        account={{
          name: accounts.google_drive,
          busy: busy === "google_drive",
          onConnect: () => connectDevice("google_drive", "Google Drive"),
          onDisconnect: () => disconnect(api.gdriveLogout),
        }}
      >
        <FieldGroup>
          <div className="grid grid-cols-2 gap-4">
            <Field>
              <FieldLabel htmlFor="gd-client">Client ID</FieldLabel>
              <Input
                id="gd-client"
                className="font-mono"
                value={c.google_drive.client_id}
                onChange={(e) => edit((d) => (d.connectors.google_drive.client_id = e.target.value))}
              />
            </Field>
            <Field>
              <FieldLabel htmlFor="gd-secret">Client secret</FieldLabel>
              <Input
                id="gd-secret"
                type="password"
                value={c.google_drive.client_secret}
                onChange={(e) => edit((d) => (d.connectors.google_drive.client_secret = e.target.value))}
              />
            </Field>
          </div>
          <Field>
            <FieldLabel htmlFor="gd-folder">Folder</FieldLabel>
            <Input
              id="gd-folder"
              value={c.google_drive.folder}
              onChange={(e) => edit((d) => (d.connectors.google_drive.folder = e.target.value))}
            />
            <FieldDescription>OAuth client of type “TVs and Limited Input devices”. See the README.</FieldDescription>
          </Field>
          <FormatPicker
            value={c.google_drive.formats}
            onChange={(v) => edit((d) => (d.connectors.google_drive.formats = v))}
          />
        </FieldGroup>
      </DestinationCard>

      <DestinationCard
        title="Dropbox"
        description="Saved to a folder in your Dropbox."
        enabled={c.dropbox.enabled}
        onEnabled={(v) => edit((d) => (d.connectors.dropbox.enabled = v))}
        account={{
          name: accounts.dropbox,
          busy: busy === "dropbox",
          onConnect: connectDropbox,
          onDisconnect: () => disconnect(api.dropboxLogout),
        }}
      >
        <FieldGroup>
          <div className="grid grid-cols-2 gap-4">
            <Field>
              <FieldLabel htmlFor="db-key">App key</FieldLabel>
              <Input
                id="db-key"
                className="font-mono"
                value={c.dropbox.app_key}
                onChange={(e) => edit((d) => (d.connectors.dropbox.app_key = e.target.value))}
              />
            </Field>
            <Field>
              <FieldLabel htmlFor="db-folder">Folder</FieldLabel>
              <Input
                id="db-folder"
                value={c.dropbox.folder}
                onChange={(e) => edit((d) => (d.connectors.dropbox.folder = e.target.value))}
              />
            </Field>
          </div>
          <FormatPicker value={c.dropbox.formats} onChange={(v) => edit((d) => (d.connectors.dropbox.formats = v))} />
        </FieldGroup>
      </DestinationCard>

      <DestinationCard
        title="Notion"
        description="Each transcript becomes a page."
        enabled={c.notion.enabled}
        onEnabled={(v) => edit((d) => (d.connectors.notion.enabled = v))}
      >
        <FieldGroup>
          <Field>
            <FieldLabel htmlFor="nt-token">Integration secret</FieldLabel>
            <Input
              id="nt-token"
              type="password"
              value={c.notion.token}
              onChange={(e) => edit((d) => (d.connectors.notion.token = e.target.value))}
            />
          </Field>
          <Field>
            <FieldLabel htmlFor="nt-parent">Parent page</FieldLabel>
            <Input
              id="nt-parent"
              value={c.notion.parent_page_id}
              onChange={(e) => edit((d) => (d.connectors.notion.parent_page_id = e.target.value))}
              placeholder="https://www.notion.so/…"
            />
            <FieldDescription>Paste the page link, and share that page with your integration.</FieldDescription>
          </Field>
        </FieldGroup>
      </DestinationCard>

      <DestinationCard
        title="Folder on this computer"
        description="A synced folder or a notes vault works well."
        enabled={c.folder.enabled}
        onEnabled={(v) => edit((d) => (d.connectors.folder.enabled = v))}
      >
        <FieldGroup>
          <Field>
            <FieldLabel htmlFor="fd-path">Folder path</FieldLabel>
            <Input
              id="fd-path"
              className="font-mono"
              value={c.folder.path}
              onChange={(e) => edit((d) => (d.connectors.folder.path = e.target.value))}
              placeholder="/home/you/Documents/Meetings"
            />
          </Field>
          <FormatPicker value={c.folder.formats} onChange={(v) => edit((d) => (d.connectors.folder.formats = v))} />
        </FieldGroup>
      </DestinationCard>

      <Dialog open={!!device} onOpenChange={(open) => !open && (setDevice(null), setBusy(null))}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Connect {device?.provider}</DialogTitle>
            <DialogDescription>
              Sign in on the page that just opened and enter this code. This window updates when you're done.
            </DialogDescription>
          </DialogHeader>
          <p className="rounded-lg bg-muted py-5 text-center font-mono text-3xl font-semibold tracking-[0.2em] select-all">
            {device?.login.user_code}
          </p>
          <DialogFooter className="items-center sm:justify-between">
            <span className="flex items-center gap-2 text-sm text-muted-foreground">
              <Spinner /> Waiting for approval
            </span>
            <Button variant="outline" onClick={() => device && openUrl(device.login.verification_uri)}>
              <ExternalLinkIcon data-icon="inline-start" />
              Open sign-in page
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={!!dropbox} onOpenChange={(open) => !open && setDropbox(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Connect Dropbox</DialogTitle>
            <DialogDescription>
              Allow access on the Dropbox page that just opened, then paste the code it shows you.
            </DialogDescription>
          </DialogHeader>
          <Field>
            <FieldLabel htmlFor="db-code">Code from Dropbox</FieldLabel>
            <Input
              id="db-code"
              autoFocus
              className="font-mono"
              value={dropbox?.code ?? ""}
              onChange={(e) => setDropbox((d) => d && { ...d, code: e.target.value })}
            />
          </Field>
          <DialogFooter>
            <Button variant="outline" onClick={() => dropbox && openUrl(dropbox.url)}>
              <ExternalLinkIcon data-icon="inline-start" />
              Open Dropbox
            </Button>
            <Button onClick={finishDropbox} disabled={!dropbox?.code.trim() || busy === "dropbox"}>
              {busy === "dropbox" && <Spinner data-icon="inline-start" />}
              Connect
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

/* ---------------- Webhooks ---------------- */

function WebhooksTab({ draft, edit, save }: { draft: Config; edit: Edit; save: () => Promise<boolean> }) {
  const [testing, setTesting] = useState<number | null>(null);

  async function test(index: number) {
    setTesting(index);
    try {
      if (!(await save())) return;
      toast.success(`Test delivered: ${await api.testWebhook(index)}`);
    } catch (e) {
      toast.error(errorText(e));
    } finally {
      setTesting(null);
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <p className="text-sm text-muted-foreground">
        Puplify POSTs JSON to these URLs. With a secret set, each request carries{" "}
        <code className="font-mono text-xs">X-Puplify-Signature: sha256=…</code>, an HMAC of{" "}
        <code className="font-mono text-xs">timestamp.body</code>.
      </p>
      {draft.connectors.webhooks.map((hook, i) => (
        <Card key={i}>
          <CardHeader>
            <CardTitle>{hook.name || hook.url || "New webhook"}</CardTitle>
            <CardDescription className="truncate font-mono">{hook.url || "No URL yet"}</CardDescription>
            <CardAction>
              <Switch
                checked={hook.enabled}
                onCheckedChange={(v) => edit((c) => (c.connectors.webhooks[i].enabled = v))}
                aria-label="Webhook on"
              />
            </CardAction>
          </CardHeader>
          <CardContent>
            <FieldGroup>
              <div className="grid grid-cols-[12rem_1fr] gap-4">
                <Field>
                  <FieldLabel htmlFor={`wh-name-${i}`}>Name</FieldLabel>
                  <Input
                    id={`wh-name-${i}`}
                    value={hook.name}
                    onChange={(e) => edit((c) => (c.connectors.webhooks[i].name = e.target.value))}
                    placeholder="n8n"
                  />
                </Field>
                <Field>
                  <FieldLabel htmlFor={`wh-url-${i}`}>URL</FieldLabel>
                  <Input
                    id={`wh-url-${i}`}
                    className="font-mono"
                    value={hook.url}
                    onChange={(e) => edit((c) => (c.connectors.webhooks[i].url = e.target.value))}
                    placeholder="https://"
                  />
                </Field>
              </div>
              <Field>
                <FieldLabel htmlFor={`wh-secret-${i}`}>Signing secret</FieldLabel>
                <Input
                  id={`wh-secret-${i}`}
                  type="password"
                  value={hook.secret}
                  onChange={(e) => edit((c) => (c.connectors.webhooks[i].secret = e.target.value))}
                  placeholder="Optional"
                />
              </Field>
              <FieldSet>
                <FieldLegend variant="label">Send when</FieldLegend>
                <FieldGroup className="gap-3">
                  {WEBHOOK_EVENTS.map((ev) => (
                    <Field key={ev.id} orientation="horizontal">
                      <Checkbox
                        id={`wh-${i}-${ev.id}`}
                        checked={hook.events.includes(ev.id)}
                        onCheckedChange={(checked) =>
                          edit((c) => {
                            const events = c.connectors.webhooks[i].events.filter((x) => x !== ev.id);
                            c.connectors.webhooks[i].events = checked ? [...events, ev.id] : events;
                          })
                        }
                      />
                      <FieldContent>
                        <FieldLabel htmlFor={`wh-${i}-${ev.id}`}>{ev.label}</FieldLabel>
                        <FieldDescription>
                          <code className="font-mono text-xs">{ev.id}</code> · {ev.hint}
                        </FieldDescription>
                      </FieldContent>
                    </Field>
                  ))}
                </FieldGroup>
              </FieldSet>
            </FieldGroup>
          </CardContent>
          <CardFooter className="justify-end gap-2 border-t">
            <Button variant="ghost" size="sm" onClick={() => edit((c) => c.connectors.webhooks.splice(i, 1))}>
              <Trash2Icon data-icon="inline-start" />
              Remove
            </Button>
            <Button variant="outline" size="sm" onClick={() => test(i)} disabled={!hook.url || testing === i}>
              {testing === i ? <Spinner data-icon="inline-start" /> : <SendIcon data-icon="inline-start" />}
              Send test
            </Button>
          </CardFooter>
        </Card>
      ))}
      <Button
        variant="outline"
        className="self-start"
        onClick={() =>
          edit((c) =>
            c.connectors.webhooks.push({
              name: "",
              url: "",
              secret: "",
              enabled: true,
              events: ["recording.started", "recording.completed"],
            }),
          )
        }
      >
        <PlusIcon data-icon="inline-start" />
        Add webhook
      </Button>
    </div>
  );
}
