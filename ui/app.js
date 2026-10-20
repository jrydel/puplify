"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);

/** Tiny DOM builder; strings become text nodes (never parsed as HTML). */
function h(tag, props = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(props)) {
    if (k === "class") el.className = v;
    else if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else if (v === true) el.setAttribute(k, "");
    else if (v !== false && v != null) el.setAttribute(k, v);
  }
  for (const c of children.flat()) {
    if (c != null) el.append(c instanceof Node ? c : String(c));
  }
  return el;
}

const state = {
  recording: null, // { id, title, startedAt }
  finishing: false,
  liveId: null, // transcript id shown in the record view
  selected: null,
  history: [],
  config: null,
  labels: { me: "Me", them: "Them" },
};

/* ---------- helpers ---------- */

function fmtTs(ms) {
  const s = Math.floor(ms / 1000);
  const pad = (n) => String(n).padStart(2, "0");
  return `${pad(Math.floor(s / 3600))}:${pad(Math.floor(s / 60) % 60)}:${pad(s % 60)}`;
}

function fmtDuration(ms) {
  const m = Math.round(ms / 60000);
  return m < 1 ? "<1 min" : `${m} min`;
}

let toastTimer;
function toast(message, kind = "error") {
  const el = $("toast");
  el.textContent = message;
  el.className = `toast ${kind === "info" ? "info" : ""}`;
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (el.hidden = true), kind === "info" ? 2500 : 8000);
}
$("toast").addEventListener("click", () => ($("toast").hidden = true));

function show(view) {
  for (const v of ["record", "transcript", "settings"]) $(`view-${v}`).hidden = v !== view;
  $("main").scrollTop = 0;
}

function lineEl(segment, labels) {
  return h(
    "div",
    { class: `line ${segment.speaker}` },
    h("span", { class: "ts" }, fmtTs(segment.start_ms)),
    h("div", {}, h("span", { class: "who" }, labels[segment.speaker] ?? segment.speaker), segment.text),
  );
}

function deliveryEl(result) {
  return h("li", { class: result.ok ? "ok" : "fail" }, `${result.connector}: ${result.detail}`);
}

/* ---------- history ---------- */

async function refreshHistory() {
  try {
    state.history = await invoke("list_transcripts");
  } catch (e) {
    toast(e);
  }
  renderHistory();
}

function renderHistory() {
  const q = $("search").value.trim().toLowerCase();
  const nav = $("history");
  nav.replaceChildren();
  let lastDay = null;
  for (const t of state.history) {
    if (q && !t.title.toLowerCase().includes(q)) continue;
    const started = new Date(t.started_at);
    const day = started.toLocaleDateString(undefined, { weekday: "short", month: "short", day: "numeric" });
    if (day !== lastDay) {
      nav.append(h("div", { class: "h-day" }, day));
      lastDay = day;
    }
    const live = state.recording?.id === t.id;
    const meta = live
      ? "Recording…"
      : `${started.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })} · ${fmtDuration(t.duration_ms)}`;
    nav.append(
      h(
        "button",
        { class: state.selected === t.id ? "active" : "", onclick: () => (live ? openRecorder() : openTranscript(t.id)) },
        h("span", { class: "h-title" }, t.title),
        h("span", { class: "h-meta" }, meta),
      ),
    );
  }
  if (!nav.children.length) nav.append(h("div", { class: "empty" }, q ? "No matches" : "No transcripts yet"));
}

$("search").addEventListener("input", renderHistory);

/* ---------- recording ---------- */

let timer;

function openRecorder() {
  state.selected = null;
  renderHistory();
  show("record");
  renderRecordControls();
}

function renderRecordControls() {
  const btn = $("record-toggle");
  btn.disabled = state.finishing;
  btn.textContent = state.recording ? "Stop" : state.finishing ? "Finishing…" : "Start recording";
  btn.classList.toggle("recording", !!state.recording);
  $("record-title").disabled = !!state.recording || state.finishing;
  $("record-timer").classList.toggle("live", !!state.recording);
  $("new-recording").textContent = state.recording ? "● Recording…" : "● New recording";
}

function tick() {
  if (state.recording) $("record-timer").textContent = fmtTs(Date.now() - state.recording.startedAt);
}

async function startRecording() {
  $("record-toggle").disabled = true;
  $("record-status").textContent = "Starting…";
  $("live").replaceChildren();
  $("partials").replaceChildren();
  $("record-deliveries").replaceChildren();
  try {
    const cfg = await invoke("get_config");
    state.labels = { me: cfg.capture.me_label, them: cfg.capture.them_label };
    const info = await invoke("start_recording", { title: $("record-title").value });
    state.recording = { id: info.transcript_id, title: info.title, startedAt: Date.now() };
    state.liveId = info.transcript_id;
    $("record-title").value = info.title;
    $("record-status").textContent = "Listening. Transcript appears as people pause.";
    timer = setInterval(tick, 500);
    tick();
    refreshHistory();
  } catch (e) {
    $("record-status").textContent = "";
    toast(e);
  }
  renderRecordControls();
}

async function stopRecording() {
  if (!state.recording) return;
  state.recording = null;
  state.finishing = true;
  clearInterval(timer);
  $("record-status").textContent = "Finishing transcription…";
  renderRecordControls();
  try {
    await invoke("stop_recording");
  } catch (e) {
    toast(e);
  }
}

$("record-toggle").addEventListener("click", () => (state.recording ? stopRecording() : startRecording()));
$("new-recording").addEventListener("click", () => {
  if (!state.recording && !state.finishing) {
    // Fresh session: clear the previous recording's output.
    $("record-title").value = "";
    $("record-timer").textContent = fmtTs(0);
    $("record-status").textContent = "";
    for (const id of ["live", "partials", "record-deliveries"]) $(id).replaceChildren();
  }
  openRecorder();
});

const partials = new Map();

function renderPartials() {
  $("partials").replaceChildren(
    ...[...partials].map(([speaker, text]) => h("div", {}, `${state.labels[speaker] ?? speaker}: ${text}`)),
  );
}

listen("session", ({ payload: ev }) => {
  switch (ev.type) {
    case "partial":
      partials.set(ev.speaker, ev.text);
      renderPartials();
      break;
    case "segment":
      partials.delete(ev.segment.speaker);
      renderPartials();
      insertLive(ev.segment);
      break;
    case "error":
      toast(ev.message);
      break;
    case "finished":
      if (state.recording) {
        // Transcription ended on its own (e.g. the cloud stream dropped).
        state.recording = null;
        clearInterval(timer);
        invoke("stop_recording").catch(toast);
      }
      state.finishing = true;
      partials.clear();
      renderPartials();
      $("record-status").textContent = "Saved. Sending to connectors…";
      renderRecordControls();
      refreshHistory();
      break;
    case "delivery":
      $("record-deliveries").append(deliveryEl(ev.result));
      break;
    case "done":
      state.finishing = false;
      $("record-status").textContent = "Done.";
      renderRecordControls();
      refreshHistory();
      break;
  }
});

/** Segments from the two sources arrive out of order; keep the view sorted. */
function insertLive(segment) {
  const live = $("live");
  const el = lineEl(segment, state.labels);
  el.dataset.start = segment.start_ms;
  const after = [...live.children].find((c) => Number(c.dataset.start) > segment.start_ms);
  live.insertBefore(el, after ?? null);
  if (!$("view-record").hidden) $("main").scrollTop = $("main").scrollHeight;
}

/* ---------- transcript view ---------- */

let currentText = "";

async function openTranscript(id) {
  try {
    const { transcript: t, text } = await invoke("get_transcript", { id });
    state.selected = id;
    currentText = text;
    renderHistory();
    show("transcript");
    $("t-title").value = t.title;
    const started = new Date(t.started_at);
    const duration = t.ended_at ? new Date(t.ended_at) - started : 0;
    $("t-meta").textContent = `${started.toLocaleString()} · ${fmtDuration(duration)} · ${t.engine}`;
    $("t-deliveries").replaceChildren();
    const body = $("t-body");
    body.replaceChildren();
    if (!t.segments.length) body.append(h("div", { class: "empty" }, "Nothing was transcribed."));
    for (const s of t.segments) body.append(lineEl(s, t.labels));
    resetDelete();
  } catch (e) {
    toast(e);
  }
}

$("t-title").addEventListener("change", async () => {
  try {
    await invoke("rename_transcript", { id: state.selected, title: $("t-title").value });
    await refreshHistory();
    const { text } = await invoke("get_transcript", { id: state.selected });
    currentText = text;
  } catch (e) {
    toast(e);
  }
});

$("t-copy").addEventListener("click", async () => {
  try {
    await navigator.clipboard.writeText(currentText);
    toast("Copied", "info");
  } catch (e) {
    toast(`Copy failed: ${e}`);
  }
});

$("t-deliver").addEventListener("click", async () => {
  const btn = $("t-deliver");
  btn.disabled = true;
  $("t-deliveries").replaceChildren(h("li", { class: "muted" }, "Sending…"));
  try {
    const results = await invoke("deliver_transcript", { id: state.selected });
    $("t-deliveries").replaceChildren(...results.map(deliveryEl));
  } catch (e) {
    $("t-deliveries").replaceChildren();
    toast(e);
  }
  btn.disabled = false;
});

// Two-step delete: native confirm() dialogs are unavailable in some webviews.
let deleteArmed = null;
function resetDelete() {
  clearTimeout(deleteArmed);
  deleteArmed = null;
  $("t-delete").textContent = "Delete";
}
$("t-delete").addEventListener("click", async () => {
  if (!deleteArmed) {
    $("t-delete").textContent = "Click again to delete";
    deleteArmed = setTimeout(resetDelete, 3000);
    return;
  }
  resetDelete();
  try {
    await invoke("delete_transcript", { id: state.selected });
    state.selected = null;
    await refreshHistory();
    openRecorder();
  } catch (e) {
    toast(e);
  }
});

/* ---------- settings ---------- */

const FORMATS = ["txt", "md", "json"];
const EVENTS = ["recording.started", "transcript.segment", "recording.completed"];

const getPath = (obj, path) => path.split(".").reduce((o, k) => o?.[k], obj);
function setPath(obj, path, value) {
  const keys = path.split(".");
  const last = keys.pop();
  keys.reduce((o, k) => (o[k] ??= {}), obj)[last] = value;
}

const form = $("settings");

function fillSelect(select, options, value) {
  // Keep a configured-but-unplugged device selectable so saving doesn't drop it.
  if (value && !options.some(([v]) => v === value)) options = [...options, [value, `${value} (not available)`]];
  select.replaceChildren(...options.map(([v, label]) => h("option", { value: v }, label)));
  select.value = value ?? "";
}

async function loadSettings() {
  show("settings");
  state.selected = null;
  renderHistory();
  $("settings-status").textContent = "";
  try {
    state.config = await invoke("get_config");
  } catch (e) {
    toast(e);
    return;
  }
  const cfg = state.config;

  for (const el of form.querySelectorAll("[name]")) {
    if (el.closest("#webhooks")) continue;
    const value = getPath(cfg, el.name);
    if (el.type === "checkbox") el.checked = !!value;
    else if (el.tagName !== "SELECT" || el.options.length) el.value = value ?? "";
  }

  await loadModels();

  let devices = [];
  try {
    devices = await invoke("list_devices");
  } catch (e) {
    toast(`Could not list audio devices: ${e}`);
  }
  const inputs = devices.filter((d) => !d.is_monitor);
  const monitors = devices.filter((d) => d.is_monitor);
  fillSelect($("mic-select"), [["", "System default"], ...inputs.map((d) => [d.id, d.name])], cfg.capture.mic_device);
  fillSelect(
    $("system-select"),
    [["", "Default output (what you hear)"], ...monitors.map((d) => [d.id, d.name])],
    cfg.capture.system_device,
  );
  // macOS captures all system audio via ScreenCaptureKit; no source to pick.
  $("system-device-row").hidden = monitors.length === 0;
  $("model-select").value = cfg.stt.whisper.model;
  form.querySelector('[name="stt.provider"]').value = cfg.stt.provider;

  for (const box of form.querySelectorAll("[data-formats]")) {
    const selected = getPath(cfg, box.dataset.formats) ?? [];
    box.replaceChildren(
      ...FORMATS.map((f) =>
        h("label", { class: "check" }, h("input", { type: "checkbox", value: f, checked: selected.includes(f) }), `.${f}`),
      ),
    );
  }
  renderWebhooks(cfg.connectors.webhooks);
  updateConditional();
  refreshOneDrive();
}

async function loadModels() {
  try {
    const models = await invoke("list_models");
    const current = state.config.stt.whisper.model;
    const options = models.map((m) => [m.name, `${m.name} — ${m.size_mb} MB${m.downloaded ? " ✓ downloaded" : ""}`]);
    if (current && !models.some((m) => m.name === current)) options.push([current, current]);
    fillSelect($("model-select"), options, $("model-select").value || current);
  } catch (e) {
    toast(e);
  }
}

function updateConditional() {
  for (const el of form.querySelectorAll("[data-show]")) {
    const [path, value] = el.dataset.show.split("=");
    el.hidden = form.querySelector(`[name="${path}"]`).value !== value;
  }
}
form.addEventListener("change", updateConditional);

function renderWebhooks(hooks) {
  const list = $("webhooks");
  list.replaceChildren(
    ...hooks.map((hook, i) =>
      h(
        "div",
        { class: "webhook", "data-index": i },
        h("label", {}, "Name", h("input", { "data-field": "name", value: hook.name })),
        h("label", {}, "URL", h("input", { "data-field": "url", value: hook.url, placeholder: "https://…" })),
        h("label", {}, "Secret (optional)", h("input", { "data-field": "secret", type: "password", value: hook.secret })),
        h("label", { class: "check" }, h("input", { type: "checkbox", "data-field": "enabled", checked: hook.enabled }), "Enabled"),
        h(
          "div",
          { class: "formats" },
          EVENTS.map((ev) =>
            h(
              "label",
              { class: "check" },
              h("input", { type: "checkbox", "data-event": ev, checked: hook.events.includes(ev) }),
              ev,
            ),
          ),
        ),
        h(
          "div",
          { class: "row" },
          h("button", { type: "button", class: "ghost", onclick: () => testWebhook(i) }, "Send test"),
          h("button", { type: "button", class: "ghost danger", onclick: () => removeWebhook(i) }, "Remove"),
          h("span", { class: "muted", "data-status": "" }),
        ),
      ),
    ),
  );
}

function readWebhooks() {
  return [...$("webhooks").children].map((el) => ({
    name: el.querySelector('[data-field="name"]').value.trim(),
    url: el.querySelector('[data-field="url"]').value.trim(),
    secret: el.querySelector('[data-field="secret"]').value,
    enabled: el.querySelector('[data-field="enabled"]').checked,
    events: EVENTS.filter((ev) => el.querySelector(`[data-event="${ev}"]`).checked),
  }));
}

$("webhook-add").addEventListener("click", () => {
  renderWebhooks([
    ...readWebhooks(),
    { name: "", url: "", secret: "", enabled: true, events: ["recording.started", "recording.completed"] },
  ]);
});

function removeWebhook(i) {
  const hooks = readWebhooks();
  hooks.splice(i, 1);
  renderWebhooks(hooks);
}

async function testWebhook(i) {
  const status = $("webhooks").children[i].querySelector("[data-status]");
  status.textContent = "Saving and sending…";
  try {
    await saveSettings();
    status.textContent = `✓ ${await invoke("test_webhook", { index: i })}`;
  } catch (e) {
    status.textContent = `✗ ${e}`;
  }
}

function readForm() {
  const cfg = structuredClone(state.config);
  for (const el of form.querySelectorAll("[name]")) {
    if (el.closest("#webhooks")) continue;
    let value = el.type === "checkbox" ? el.checked : el.value.trim();
    if ((el.name === "capture.mic_device" || el.name === "capture.system_device") && value === "") value = null;
    setPath(cfg, el.name, value);
  }
  for (const box of form.querySelectorAll("[data-formats]")) {
    setPath(cfg, box.dataset.formats, [...box.querySelectorAll("input:checked")].map((i) => i.value));
  }
  cfg.connectors.webhooks = readWebhooks();
  return cfg;
}

async function saveSettings() {
  const cfg = readForm();
  await invoke("save_config", { config: cfg });
  state.config = cfg;
}

form.addEventListener("submit", async (e) => {
  e.preventDefault();
  try {
    await saveSettings();
    $("settings-status").textContent = "Saved";
    toast("Settings saved", "info");
  } catch (err) {
    toast(err);
  }
});

$("open-settings").addEventListener("click", loadSettings);

/* models */

$("model-download").addEventListener("click", async () => {
  const name = $("model-select").value;
  const btn = $("model-download");
  btn.disabled = true;
  $("model-progress").textContent = "Starting download…";
  try {
    await invoke("download_model", { name });
    $("model-progress").textContent = `${name} downloaded`;
    await loadModels();
  } catch (e) {
    $("model-progress").textContent = "";
    toast(e);
  }
  btn.disabled = false;
});

listen("model-progress", ({ payload: p }) => {
  const mb = (n) => Math.round(n / 1e6);
  $("model-progress").textContent = p.total ? `${p.name}: ${mb(p.done)} / ${mb(p.total)} MB` : `${p.name}: ${mb(p.done)} MB`;
});

/* OneDrive */

async function refreshOneDrive() {
  try {
    const account = await invoke("onedrive_status");
    $("onedrive-account").textContent = account ? `Signed in as ${account}` : "Not signed in";
    $("onedrive-logout").hidden = !account;
  } catch (e) {
    $("onedrive-account").textContent = `${e}`;
  }
}

$("onedrive-login").addEventListener("click", async () => {
  const box = $("onedrive-code");
  try {
    await saveSettings();
    const code = await invoke("onedrive_login");
    box.replaceChildren(
      h("div", {}, "Open ", h("a", { href: "#", onclick: (e) => { e.preventDefault(); openUrl(code.verification_uri); } }, code.verification_uri), " and enter:"),
      h("code", {}, code.user_code),
      h("div", { class: "muted" }, "Waiting for approval…"),
    );
    box.hidden = false;
    openUrl(code.verification_uri);
  } catch (e) {
    toast(e);
  }
});

function openUrl(url) {
  window.__TAURI__.opener.openUrl(url).catch(() => {});
}

listen("onedrive-login", ({ payload }) => {
  $("onedrive-code").hidden = true;
  if (payload.ok) toast(`OneDrive connected: ${payload.account}`, "info");
  else toast(payload.error);
  refreshOneDrive();
});

$("onedrive-logout").addEventListener("click", async () => {
  try {
    await invoke("onedrive_logout");
  } catch (e) {
    toast(e);
  }
  refreshOneDrive();
});

/* ---------- boot ---------- */

(async function boot() {
  await refreshHistory();
  const active = await invoke("recording_status").catch(() => null);
  if (active) {
    // Window reloaded mid-recording.
    state.recording = { id: active.transcript_id, title: active.title, startedAt: Date.now() };
    state.liveId = active.transcript_id;
    $("record-title").value = active.title;
    timer = setInterval(tick, 500);
  }
  openRecorder();
})();
