// Puplify landing page: live tape demo, OS-aware download button, release info.
// Everything degrades: without this script the page shows the finished transcript and plain links.

const animate = !matchMedia("(prefers-reduced-motion: reduce)").matches;
if (animate) document.documentElement.classList.add("anim");

type Os = "mac" | "windows" | "linux" | "mobile" | "other";

interface LatestAsset {
  name: string;
  size: number;
  url: string;
}
interface Latest {
  version: string | null;
  published_at: string | null;
  assets: Record<string, LatestAsset>;
}
interface UserAgentData {
  platform?: string;
  mobile?: boolean;
}

const $ = <T extends HTMLElement>(selector: string, root: ParentNode = document) => root.querySelector<T>(selector);

// ---------- tape demo ----------

/** Real seconds per meeting second: the 53 s meeting plays in about 16 s. */
const SECONDS_PER_SECOND = 0.3;
const MEETING_END = 53;

interface Utterance {
  block: HTMLElement;
  line: HTMLElement;
  words: HTMLElement[];
  start: number;
  duration: number;
  shown: number;
}

function initTape(): void {
  const deck = $("#deck");
  const tape = $("#tape");
  const head = $("#head");
  const rec = $("#rec");
  const recText = $("#rec-text");
  const saved = $("#saved");
  const replay = $<HTMLButtonElement>("#replay");
  if (!deck || !tape || !head || !rec || !recText || !saved || !replay) return;

  const total = parseFloat(tape.style.getPropertyValue("--T"));
  const utterances: Utterance[] = [];
  for (const block of deck.querySelectorAll<HTMLElement>(".blk")) {
    const line = $(`.lines li[data-i="${block.dataset.i}"]`, deck);
    const say = line && $(".say", line);
    if (!line || !say) continue;
    const words = (say.textContent ?? "").trim().split(/\s+/);
    say.textContent = "";
    const spans = words.map((word, index) => {
      const span = document.createElement("span");
      span.className = "w";
      span.textContent = word;
      say.append(span, index < words.length - 1 ? " " : "");
      return span;
    });
    utterances.push({
      block,
      line,
      words: spans,
      start: parseFloat(block.style.getPropertyValue("--s")),
      duration: parseFloat(block.style.getPropertyValue("--d")),
      shown: 0,
    });
  }

  const clock = (seconds: number) => {
    const s = Math.floor(seconds);
    return `${String(Math.floor(s / 60)).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}`;
  };

  const render = (t: number) => {
    for (const u of utterances) {
      const progress = Math.min(Math.max((t - u.start) / u.duration, 0), 1);
      u.block.style.setProperty("--p", progress.toFixed(4));
      u.block.classList.toggle("on", progress > 0);
      u.line.classList.toggle("on", progress > 0);
      const shown = progress > 0 ? Math.max(1, Math.ceil(progress * u.words.length)) : 0;
      if (shown !== u.shown) {
        u.words.forEach((word, index) => word.classList.toggle("on", index < shown));
        u.shown = shown;
      }
    }
    head.style.setProperty("--x", ((Math.min(t, MEETING_END) / total) * 100).toFixed(3));
    recText.textContent = `REC ${clock(Math.min(t, MEETING_END))}`;
  };

  let frameId = 0;
  const play = () => {
    cancelAnimationFrame(frameId);
    saved.classList.remove("on");
    replay.hidden = true;
    rec.dataset.state = "live";
    head.classList.add("on");
    let t = 0;
    let last = performance.now();
    render(0);
    const tick = (now: number) => {
      // Clamp the step so a background tab doesn't skip the whole meeting.
      t += Math.min(now - last, 100) / 1000 / SECONDS_PER_SECOND;
      last = now;
      render(t);
      if (t < MEETING_END) {
        frameId = requestAnimationFrame(tick);
        return;
      }
      rec.dataset.state = "done";
      recText.textContent = `SAVED ${clock(MEETING_END)}`;
      head.classList.remove("on");
      saved.classList.add("on");
      replay.hidden = false;
    };
    frameId = requestAnimationFrame(tick);
  };

  replay.addEventListener("click", () => {
    play();
    replay.blur();
  });

  render(0);
  let started = false;
  const start = () => {
    if (started) return;
    started = true;
    setTimeout(play, 400);
  };
  void document.fonts.ready.then(start);
  setTimeout(start, 1200);
}

// ---------- downloads ----------

function detectOs(): Os {
  const uad = (navigator as Navigator & { userAgentData?: UserAgentData }).userAgentData;
  const ua = navigator.userAgent;
  if (uad?.mobile || /Android|iPhone|iPad|iPod/i.test(ua)) return "mobile";
  const platform = uad?.platform ?? "";
  if (/mac/i.test(platform) || (!platform && /Macintosh|Mac OS X/.test(ua))) return "mac";
  if (/windows/i.test(platform) || (!platform && /Windows/.test(ua))) return "windows";
  if (/CrOS/.test(ua)) return "other";
  if (/linux/i.test(platform) || (!platform && /Linux|X11/.test(ua))) return "linux";
  return "other";
}

function formatSize(bytes: number): string {
  const mb = bytes / 1024 ** 2;
  return `${mb >= 100 ? mb.toFixed(0) : mb.toFixed(1)} MB`;
}

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function downloadButton(platform: string, label: string, latest: Latest | null): HTMLAnchorElement {
  const a = el("a", "btn btn-primary", label);
  a.href = `/download/${platform}`;
  const asset = latest?.assets[platform];
  if (asset) a.append(el("span", "btn-meta", `${latest?.version ? `v${latest.version} · ` : ""}${formatSize(asset.size)}`));
  return a;
}

function renderCta(os: Os, latest: Latest | null): void {
  const cta = $("#cta");
  const note = $("#cta-note");
  if (!cta || !note) return;
  const all = el("a", "btn btn-quiet", os === "mobile" ? "See all downloads" : "Other platforms");
  all.href = "#download";
  note.replaceChildren();

  switch (os) {
    case "mac":
      cta.replaceChildren(downloadButton("mac", "Download for macOS", latest), all);
      note.append("Apple Silicon only. Not notarized: one command after install, ", noteLink("see how"), ".");
      break;
    case "windows":
      cta.replaceChildren(downloadButton("windows", "Download for Windows", latest), all);
      note.textContent = "64-bit installer. Free and open source.";
      break;
    case "linux": {
      const label = el("p", "cta-label", "Download for Linux");
      cta.replaceChildren(
        label,
        downloadButton("deb", ".deb", latest),
        downloadButton("rpm", ".rpm", latest),
        downloadButton("appimage", ".AppImage", latest),
        all,
      );
      note.textContent = "Debian and Ubuntu, Fedora and openSUSE, or any distribution.";
      break;
    }
    case "mobile":
      cta.replaceChildren(all);
      note.textContent = "Puplify is a desktop app. Open this page on your Mac, PC or Linux machine.";
      break;
    default:
      note.textContent = "Free and open source.";
  }
}

function noteLink(text: string): HTMLAnchorElement {
  const a = el("a", "", text);
  a.href = "#download";
  return a;
}

function renderDownloads(os: Os, latest: Latest): void {
  const version = $("#version");
  if (version) {
    if (latest.version) {
      const when = latest.published_at
        ? new Intl.DateTimeFormat("en", { dateStyle: "medium" }).format(new Date(latest.published_at))
        : "";
      version.textContent = `Version ${latest.version}${when ? ` · ${when}` : ""}`;
    } else {
      version.textContent = "No release published yet. Downloads open the GitHub releases page.";
    }
  }

  for (const item of document.querySelectorAll<HTMLElement>(".dl-list li[data-platform]")) {
    const platform = item.dataset.platform ?? "";
    const asset = latest.assets[platform];
    const size = $("[data-size]", item);
    if (asset && size) {
      size.textContent = formatSize(asset.size);
      const link = $("a", item);
      if (link) link.title = asset.name;
    } else if (latest.version && size) {
      item.classList.add("missing");
      size.textContent = "not in this release";
    }
    if (platform === os) item.classList.add("match");
  }
}

async function loadLatest(): Promise<Latest | null> {
  try {
    const res = await fetch("/api/latest", { headers: { Accept: "application/json" } });
    if (!res.ok) return null;
    return (await res.json()) as Latest;
  } catch {
    return null;
  }
}

function initCopy(): void {
  const button = $<HTMLButtonElement>("#copy-xattr");
  const source = $("#xattr");
  if (!button || !source) return;
  button.addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(source.textContent ?? "");
      button.textContent = "Copied";
    } catch {
      const range = document.createRange();
      range.selectNodeContents(source);
      const selection = getSelection();
      selection?.removeAllRanges();
      selection?.addRange(range);
      button.textContent = "Press Ctrl/⌘ C";
    }
    setTimeout(() => (button.textContent = "Copy"), 2000);
  });
}

function init(): void {
  const os = detectOs();
  renderCta(os, null);
  initCopy();
  if (animate) initTape();
  void loadLatest().then((latest) => {
    if (!latest) return;
    renderCta(os, latest);
    renderDownloads(os, latest);
  });
}

if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
else init();
