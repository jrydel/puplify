// Downloads the models shipped inside the desktop bundle (see `bundle.resources`
// in src-tauri/tauri.conf.json). Kept out of git; fetched once and cached.
import { mkdir, rename, stat } from "node:fs/promises";
import path from "node:path";

const DEST = path.join(import.meta.dir, "..", "src-tauri", "resources", "models");

const MODELS = [
  // Multilingual Whisper base, 5-bit quantized: the default transcription model.
  { file: "ggml-base-q5_1.bin", url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base-q5_1.bin" },
  // Silero voice-activity model used to skip non-speech audio.
  { file: "ggml-silero-v5.1.2.bin", url: "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin" },
];

await mkdir(DEST, { recursive: true });

for (const { file, url } of MODELS) {
  const target = path.join(DEST, file);
  const existing = await stat(target).catch(() => null);
  if (existing?.size) {
    console.log(`✓ ${file} (cached)`);
    continue;
  }
  console.log(`↓ ${file}`);
  const res = await fetch(url);
  if (!res.ok || !res.body) throw new Error(`downloading ${url}: HTTP ${res.status}`);
  const expected = Number(res.headers.get("content-length") ?? 0);
  const part = `${target}.part`;
  // Stream to disk; Bun.write(path, response) stalls on large Hugging Face downloads.
  const sink = Bun.file(part).writer();
  let written = 0;
  for await (const chunk of res.body) {
    sink.write(chunk);
    written += chunk.byteLength;
  }
  await sink.end();
  if (expected && written !== expected) throw new Error(`${file}: got ${written} of ${expected} bytes`);
  await rename(part, target);
  console.log(`✓ ${file} (${(written / 1e6).toFixed(1)} MB)`);
}
