import { FolderIcon } from "lucide-react";

import { LOGOS, type LogoBrand } from "@/components/brand-logos";
import { cn } from "@/lib/utils";

export type Brand = LogoBrand | "folder";

/** Product logo on a white tile, as in app connector galleries; the tile stays white in dark mode so logos keep their colours. */
export function BrandTile({ brand, className }: { brand: Brand; className?: string }) {
  return (
    <div className={cn("grid size-10 shrink-0 place-items-center rounded-xl bg-white shadow-xs ring-1 ring-black/10", className)}>
      {brand === "folder" ? (
        <FolderIcon className="size-5 text-[#5B6573]" aria-hidden />
      ) : (
        // Static logo markup from brand-logos.ts, not user input.
        <svg viewBox={LOGOS[brand].viewBox} className="size-6" aria-hidden dangerouslySetInnerHTML={{ __html: LOGOS[brand].body }} />
      )}
    </div>
  );
}
