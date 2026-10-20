// Builds the client bundle and copies self-hosted fonts and the app icon into public/.
import { copyFile, mkdir } from "node:fs/promises";
import { join } from "node:path";

const root = join(import.meta.dir, "..");
const fonts: Array<[pkg: string, file: string, out: string]> = [
  ["bricolage-grotesque", "bricolage-grotesque-latin-opsz-normal.woff2", "bricolage-grotesque.woff2"],
  ["hanken-grotesk", "hanken-grotesk-latin-wght-normal.woff2", "hanken-grotesk.woff2"],
  ["jetbrains-mono", "jetbrains-mono-latin-wght-normal.woff2", "jetbrains-mono.woff2"],
];

await mkdir(join(root, "public/fonts"), { recursive: true });
for (const [pkg, file, out] of fonts) {
  await copyFile(
    join(root, `node_modules/@fontsource-variable/${pkg}/files/${file}`),
    join(root, "public/fonts", out),
  );
}

// The icon is owned by the desktop app; the site always ships its current version.
await copyFile(join(root, "../src-tauri/icons/icon.svg"), join(root, "public/icon.svg"));
await copyFile(join(root, "../src-tauri/icons/128x128@2x.png"), join(root, "public/apple-touch-icon.png"));

const result = await Bun.build({
  entrypoints: [join(root, "src/client/app.ts")],
  outdir: join(root, "public"),
  target: "browser",
  minify: true,
  naming: "app.js",
});
if (!result.success) {
  for (const log of result.logs) console.error(log);
  process.exit(1);
}
console.log("site built");
