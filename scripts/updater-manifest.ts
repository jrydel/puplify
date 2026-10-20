// Builds the in-app updater manifest (`latest.json`) for a release from the
// signed bundles every platform job uploaded, and uploads it to the release.
// Run once after all builds finish, so parallel jobs never race on the file.
//
//   bun scripts/updater-manifest.ts v0.5.0 [owner/repo]
//
// Keys follow tauri-plugin-updater: `{os}-{arch}-{installer}`, plus a plain
// `{os}-{arch}` fallback for builds that can't tell how they were installed.
import { mkdtemp, readdir, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { $ } from "bun";

const tag = process.argv[2];
const repo = process.argv[3] ?? process.env.GITHUB_REPOSITORY;
if (!tag || !repo) throw new Error("usage: bun scripts/updater-manifest.ts <tag> [owner/repo]");

/** Updater bundle suffix → OS, installer name and default architecture. */
const KINDS: { suffix: string; os: string; installer: string; arch: string; fallback: boolean }[] = [
  { suffix: ".AppImage", os: "linux", installer: "appimage", arch: "x86_64", fallback: true },
  { suffix: ".deb", os: "linux", installer: "deb", arch: "x86_64", fallback: false },
  { suffix: ".rpm", os: "linux", installer: "rpm", arch: "x86_64", fallback: false },
  { suffix: ".app.tar.gz", os: "darwin", installer: "app", arch: "aarch64", fallback: true },
  { suffix: "-setup.exe", os: "windows", installer: "nsis", arch: "x86_64", fallback: true },
  { suffix: ".msi", os: "windows", installer: "msi", arch: "x86_64", fallback: false },
];

const ARCH_IN_NAME: [RegExp, string][] = [
  [/aarch64|arm64/, "aarch64"],
  [/x86_64|x64|amd64/, "x86_64"],
];

const { assets } = (await $`gh release view ${tag} --repo ${repo} --json assets`.json()) as {
  assets: { name: string }[];
};
const names = new Set(assets.map((a) => a.name));

const dir = await mkdtemp(join(tmpdir(), "puplify-sigs-"));
await $`gh release download ${tag} --repo ${repo} --pattern ${"*.sig"} --dir ${dir}`;

const platforms: Record<string, { url: string; signature: string }> = {};
for (const sigName of await readdir(dir)) {
  const bundle = sigName.slice(0, -".sig".length);
  const kind = KINDS.find((k) => bundle.endsWith(k.suffix));
  if (!kind || !names.has(bundle)) {
    console.warn(`skipping ${sigName}: ${kind ? "bundle not in release" : "unknown bundle type"}`);
    continue;
  }
  // Draft releases report `untagged-…` asset URLs; this is the address once published.
  const url = `https://github.com/${repo}/releases/download/${tag}/${encodeURIComponent(bundle)}`;
  const arch = ARCH_IN_NAME.find(([re]) => re.test(bundle))?.[1] ?? kind.arch;
  const entry = { url, signature: (await readFile(join(dir, sigName), "utf8")).trim() };
  platforms[`${kind.os}-${arch}-${kind.installer}`] = entry;
  if (kind.fallback) platforms[`${kind.os}-${arch}`] = entry;
}
if (Object.keys(platforms).length === 0) throw new Error(`no signed bundles found in ${tag}`);

const manifest = {
  version: tag.replace(/^v/, ""),
  notes: `https://github.com/${repo}/releases/tag/${tag}`,
  pub_date: new Date().toISOString().replace(/\.\d{3}Z$/, "Z"),
  platforms,
};
const out = join(dir, "latest.json");
await writeFile(out, `${JSON.stringify(manifest, null, 2)}\n`);
await $`gh release upload ${tag} ${out} --repo ${repo} --clobber`;
console.log(`latest.json: ${Object.keys(platforms).sort().join(", ")}`);
