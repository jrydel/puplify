const REPO = "jrydel/puplify";
const RELEASES_URL = `https://github.com/${REPO}/releases`;
const CANONICAL_HOST = "puplify.com";
const CACHE_TTL_SECONDS = 300;
const CACHE_ERROR_TTL_SECONDS = 30;

/** Ordered matchers per platform: the first pattern that hits an asset wins. */
const PLATFORMS = {
  mac: [/_aarch64\.dmg$/],
  windows: [/_x64-setup\.exe$/, /_x64_en-US\.msi$/],
  deb: [/_amd64\.deb$/],
  rpm: [/\.x86_64\.rpm$/],
  appimage: [/_amd64\.AppImage$/],
  "cli-linux": [/^puplify-cli-linux-x86_64\.tar\.gz$/],
  "cli-mac": [/^puplify-cli-macos-arm64\.tar\.gz$/],
  "cli-windows": [/^puplify-cli-windows-x86_64\.zip$/],
} as const satisfies Record<string, readonly RegExp[]>;

type Platform = keyof typeof PLATFORMS;

interface GitHubAsset {
  name: string;
  size: number;
  browser_download_url: string;
}

interface GitHubRelease {
  tag_name: string;
  published_at: string | null;
  assets: GitHubAsset[];
}

/** What we keep of a release: `null` means "GitHub answered, there is no release". */
type ReleaseLookup = { release: GitHubRelease | null } | { error: true };

function isPlatform(value: string): value is Platform {
  return Object.hasOwn(PLATFORMS, value);
}

function findAsset(release: GitHubRelease, platform: Platform): GitHubAsset | undefined {
  for (const matcher of PLATFORMS[platform]) {
    const asset = release.assets.find((a) => matcher.test(a.name));
    if (asset) return asset;
  }
  return undefined;
}

async function fetchLatestRelease(): Promise<ReleaseLookup> {
  try {
    const res = await fetch(`https://api.github.com/repos/${REPO}/releases/latest`, {
      headers: {
        Accept: "application/vnd.github+json",
        "User-Agent": "puplify-site (+https://puplify.com)",
        "X-GitHub-Api-Version": "2022-11-28",
      },
    });
    if (res.status === 404) return { release: null };
    if (!res.ok) return { error: true };
    const json = (await res.json()) as GitHubRelease;
    return { release: { tag_name: json.tag_name, published_at: json.published_at, assets: json.assets ?? [] } };
  } catch {
    return { error: true };
  }
}

/** Latest release via the edge cache (~5 min; failures are remembered for 30 s). */
async function latestRelease(ctx: ExecutionContext): Promise<ReleaseLookup> {
  const cache = caches.default;
  const key = new Request(`https://${CANONICAL_HOST}/__cache/latest-release`);
  const hit = await cache.match(key);
  if (hit) return (await hit.json()) as ReleaseLookup;

  const lookup = await fetchLatestRelease();
  const ttl = "error" in lookup ? CACHE_ERROR_TTL_SECONDS : CACHE_TTL_SECONDS;
  ctx.waitUntil(
    cache.put(
      key,
      new Response(JSON.stringify(lookup), {
        headers: { "Content-Type": "application/json", "Cache-Control": `public, max-age=${ttl}` },
      }),
    ),
  );
  return lookup;
}

function json(body: unknown, status: number, cacheControl: string): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "Content-Type": "application/json; charset=utf-8",
      "Cache-Control": cacheControl,
      "X-Content-Type-Options": "nosniff",
    },
  });
}

async function handleDownload(platform: string, ctx: ExecutionContext): Promise<Response | null> {
  if (!isPlatform(platform)) return null;
  const lookup = await latestRelease(ctx);
  const asset = "release" in lookup && lookup.release ? findAsset(lookup.release, platform) : undefined;
  return new Response(null, {
    status: 302,
    headers: {
      Location: asset?.browser_download_url ?? RELEASES_URL,
      "Cache-Control": "no-store",
    },
  });
}

async function handleLatest(ctx: ExecutionContext): Promise<Response> {
  const lookup = await latestRelease(ctx);
  if ("error" in lookup) {
    return json({ error: "github_unavailable" }, 502, `public, max-age=${CACHE_ERROR_TTL_SECONDS}`);
  }
  const { release } = lookup;
  const assets: Record<string, { name: string; size: number; url: string }> = {};
  if (release) {
    for (const platform of Object.keys(PLATFORMS) as Platform[]) {
      const asset = findAsset(release, platform);
      if (asset) assets[platform] = { name: asset.name, size: asset.size, url: `/download/${platform}` };
    }
  }
  return json(
    {
      version: release ? release.tag_name.replace(/^v/, "") : null,
      published_at: release?.published_at ?? null,
      assets,
    },
    200,
    `public, max-age=${CACHE_TTL_SECONDS}`,
  );
}

export default {
  async fetch(request, env, ctx): Promise<Response> {
    const url = new URL(request.url);

    if (url.hostname === `www.${CANONICAL_HOST}`) {
      return Response.redirect(`https://${CANONICAL_HOST}${url.pathname}${url.search}`, 301);
    }

    if (request.method !== "GET" && request.method !== "HEAD") {
      return new Response("Method not allowed", { status: 405, headers: { Allow: "GET, HEAD" } });
    }

    if (url.pathname === "/api/latest") return handleLatest(ctx);

    if (url.pathname.startsWith("/download/")) {
      const response = await handleDownload(url.pathname.slice("/download/".length).replace(/\/$/, ""), ctx);
      if (response) return response;
    }

    if (url.pathname.startsWith("/api/")) {
      return json({ error: "not_found" }, 404, "no-store");
    }

    return env.ASSETS.fetch(request);
  },
} satisfies ExportedHandler<Env>;
