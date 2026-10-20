import { CheckIcon, XIcon } from "lucide-react";

import type { DeliveryResult } from "@/lib/api";

/** Human names for connector ids reported by the backend. */
const NAMES: Record<string, string> = {
  folder: "Folder",
  onedrive: "OneDrive",
  "google-drive": "Google Drive",
  dropbox: "Dropbox",
  notion: "Notion",
};

export function DeliveryList({ results }: { results: DeliveryResult[] }) {
  if (results.length === 0) return null;
  return (
    <ul className="flex flex-col gap-1.5" aria-label="Where the transcript was sent">
      {results.map((d) => (
        <li key={d.connector} className="flex items-start gap-2 text-sm">
          {d.ok ? (
            <CheckIcon className="mt-0.5 size-4 shrink-0 text-me" aria-label="Sent" />
          ) : (
            <XIcon className="mt-0.5 size-4 shrink-0 text-destructive" aria-label="Failed" />
          )}
          <span className="shrink-0 font-medium">
            {NAMES[d.connector] ?? d.connector.replace(/^webhook:/, "Webhook · ")}
          </span>
          <span className="min-w-0 break-all text-muted-foreground">{d.detail}</span>
        </li>
      ))}
    </ul>
  );
}
