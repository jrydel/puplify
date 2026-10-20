# Verbatim

Desktop meeting transcriber for **Linux and macOS**. It records your microphone and the meeting's
system audio, transcribes both live, and sends the full text transcript wherever you want:
**OneDrive**, a **local folder**, or any **webhook** (Zapier, n8n, Make, your own server).

There are no bots joining calls and no AI notes. You get a transcript with a clear "who said what":

```text
Weekly sync
2026-10-20 14:05 – 14:47 (42 min)

[00:00:03] Me: Morning. Shall we start with the budget?
[00:00:07] Them: Yes. We're about ten percent over on contractors.
```

Speakers are told apart by audio source: the microphone is **Me** and system audio is **Them**.
You can rename both labels in Settings.

## Features

- **Transcription engines** (choose in Settings)
  - **Local whisper.cpp**: private and offline. A Silero VAD filters out background noise, so it
    doesn't invent text from silence. Text appears a few seconds after each pause.
  - **Deepgram** (Nova-3): cloud streaming with live partial results. Mic and system audio are sent
    as two channels, so speaker labels stay exact.
- **Connectors** run when a recording finishes. You can re-send any stored transcript.
  - OneDrive (personal and work/school accounts): uploads `.txt`, `.md` and/or `.json`.
  - Local folder: same formats, e.g. a synced folder or an Obsidian vault.
  - Webhooks: `recording.started`, live `transcript.segment`, and `recording.completed` with the
    full text. HMAC-signed.
- **Local history**: every transcript is saved as JSON in the data directory and autosaved during
  recording.
- **CLI** (`verbatim`): works without the GUI and is scriptable.

## Requirements

| | Linux | macOS |
|---|---|---|
| OS | Any distro with PipeWire (`pipewire-pulse`) or PulseAudio | macOS 13 Ventura or newer |
| Mic | default or chosen PulseAudio source | CoreAudio input |
| System audio | monitor of the default output (or any monitor source) | ScreenCaptureKit (needs **Screen & System Audio Recording** permission) |

## Install

Download from [Releases](https://github.com/jrydel/verbatim/releases/latest):

| Platform | File |
|---|---|
| Fedora / RHEL / openSUSE | `Verbatim-<version>-1.x86_64.rpm` → `sudo dnf install ./Verbatim-*.rpm` |
| Debian / Ubuntu | `Verbatim_<version>_amd64.deb` → `sudo apt install ./Verbatim_*.deb` |
| Other Linux | `Verbatim_<version>_amd64.AppImage` → `chmod +x` and run |
| macOS (Apple Silicon) | `Verbatim_<version>_aarch64.dmg` |
| CLI only | `verbatim-cli-linux-x86_64.tar.gz`, `verbatim-cli-macos-arm64.tar.gz` |

The macOS build is ad-hoc signed, not notarized. After dragging it to Applications, run
`xattr -dr com.apple.quarantine /Applications/Verbatim.app` once. Intel Macs: build from source.

## Build dependencies

Rust ≥ 1.90, Bun or Node (for the Tauri CLI), CMake and a C++ compiler (for whisper.cpp).

Fedora:

```sh
sudo dnf install webkit2gtk4.1-devel libsoup3-devel javascriptcoregtk4.1-devel \
  pulseaudio-libs-devel clang clang-devel cmake gcc-c++ libappindicator-gtk3-devel librsvg2-devel
```

Debian/Ubuntu:

```sh
sudo apt install libwebkit2gtk-4.1-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev \
  libpulse-dev clang libclang-dev cmake g++ libayatana-appindicator3-dev librsvg2-dev
```

macOS: Xcode Command Line Tools (`xcode-select --install`) and CMake (`brew install cmake`).

## Build and run

```sh
bun install                 # or: npm install
bun run dev                 # run the desktop app in development
bun run build               # bundles: .deb/.rpm/.AppImage on Linux, .app/.dmg on macOS
cargo build --release -p verbatim-cli   # CLI only → target/release/verbatim
```

GPU acceleration for whisper.cpp is opt-in. Use `--features metal` on Apple Silicon, or
`--features vulkan` / `--features cuda` on Linux, for example:
`bun run build -- --features metal`.

## First run

1. Open **Settings → Transcription**.
   - **Local**: pick a model and click **Download model**. `base.en` (142 MB) is fast on any CPU.
     `small.en` or `large-v3-turbo-q5_0` are more accurate. For other languages, pick a model
     without `.en` and set the language (or `auto`).
   - **Deepgram**: paste an API key from <https://console.deepgram.com>.
2. **Audio**: keep both sources on. Wear headphones if you can; otherwise the mic also picks up the
   other people from your speakers, and their words show up twice (once as Me).
3. Click **New recording → Start recording**. Click **Stop** when the meeting ends. The transcript
   is saved and then sent to every enabled connector.

On macOS the first recording triggers two permission prompts: Microphone, and Screen & System Audio
Recording. After granting the second one, restart the app.

## OneDrive setup

OneDrive access needs a Microsoft Entra app registration. It's free, takes about 3 minutes, and the
app's client ID stays yours:

1. Go to <https://entra.microsoft.com> → **App registrations** → **New registration**.
   - Name: `Verbatim`
   - Supported account types: **Accounts in any organizational directory and personal Microsoft
     accounts**. This allows both personal and work/school accounts.
   - Redirect URI: leave empty.
2. Open the new app → **Authentication** → **Advanced settings** → set **Allow public client
   flows** to **Yes** → Save. This is required for device-code sign-in.
3. **API permissions** → Add → Microsoft Graph → Delegated: `Files.ReadWrite`, `User.Read`,
   `offline_access`. In work tenants that need it, an admin must grant consent.
4. Copy the **Application (client) ID** from **Overview**.
5. In the app: **Settings → OneDrive** → paste the client ID → **Sign in**. A browser page opens.
   Enter the code shown in the app and approve.
6. Tick **Upload finished transcripts**, choose a folder (default `Transcripts`) and formats, then
   **Save settings**.

Files are named `YYYY-MM-DD HHMM Title.txt`. Re-sending replaces the existing file. Tokens are
stored in `onedrive-token.json` in the config directory (mode 0600) and refresh automatically.

Single-tenant only? Put your tenant ID or domain in **Tenant** instead of `common`.

## Webhooks

Every request is a `POST` with a JSON body and these headers:

```text
Content-Type: application/json
X-Verbatim-Event: recording.completed
X-Verbatim-Delivery: 4f1c…            (unique per delivery)
X-Verbatim-Timestamp: 1792505100      (unix seconds)
X-Verbatim-Signature: sha256=<hex>    (only when a secret is set)
```

The signature is `HMAC-SHA256(secret, "<timestamp>.<raw body>")`. Check it and reject old
timestamps:

```python
import hmac, hashlib
expected = "sha256=" + hmac.new(secret, f"{ts}.".encode() + body, hashlib.sha256).hexdigest()
ok = hmac.compare_digest(expected, signature_header)
```

Payloads:

```jsonc
// recording.started
{ "event": "recording.started", "delivery_id": "…", "sent_at": "…",
  "transcript": { "id": "…", "title": "…", "started_at": "…", "engine": "whisper:base.en" } }

// transcript.segment  (live, one per finalized utterance; opt-in)
{ "event": "transcript.segment", "transcript_id": "…", "title": "…", "speaker_label": "Me",
  "segment": { "speaker": "me", "start_ms": 3200, "end_ms": 5100, "text": "…" } }

// recording.completed
{ "event": "recording.completed", "duration_ms": 2520000,
  "text": "Weekly sync\n2026-10-20 14:05 – 14:47 (42 min)\n\n[00:00:03] Me: …",
  "transcript": { "id": "…", "title": "…", "started_at": "…", "ended_at": "…", "engine": "…",
                  "labels": { "me": "Me", "them": "Them" }, "segments": [ … ] } }
```

`recording.completed` is retried with backoff (2 s, 8 s) on network errors, 5xx and 429. Live
events are sent once.

## CLI

```text
verbatim record [--title T] [--duration SECS]   # Ctrl-C stops
verbatim transcribe meeting.wav [--title T]     # any WAV file
verbatim list | show <id> [--format txt|md|json] | deliver <id> | delete <id>
verbatim devices                                # mic + monitor sources
verbatim models list | models download small.en
verbatim onedrive login | status | logout
verbatim webhook-test [name]
verbatim config                                 # path + current settings
```

## Files

| | Linux | macOS |
|---|---|---|
| Settings (`config.json`, OneDrive token) | `~/.config/verbatim/` | `~/Library/Application Support/verbatim/` |
| Transcripts, models | `~/.local/share/verbatim/` | `~/Library/Application Support/verbatim/` |

Set `VERBATIM_HOME=/some/dir` to keep everything in one directory (for portable installs or tests).
`config.json` is plain JSON with mode 0600; the GUI and CLI share it.

## Layout

```text
crates/core   capture (audio/), engines (stt/), transcript + store, connectors/, session pipeline
crates/cli    `verbatim` command
src-tauri     desktop app (commands in src/main.rs)
ui            plain HTML/CSS/JS front end, no build step
```

## Development

```sh
cargo test -p verbatim-core
```
