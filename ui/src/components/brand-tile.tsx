import { FolderIcon, LinkIcon, type LucideIcon } from "lucide-react";

import { LOGOS, type LogoBrand } from "@/components/brand-logos";
import { cn } from "@/lib/utils";

/** Generic glyphs for sources that aren't a product. */
const GLYPHS: Record<"folder" | "link", LucideIcon> = { folder: FolderIcon, link: LinkIcon };

export type Brand = LogoBrand | keyof typeof GLYPHS;

/** Product logo on a white tile, as in app connector galleries; the tile stays white in dark mode so logos keep their colours. */
export function BrandTile({ brand, className }: { brand: Brand; className?: string }) {
  const Glyph = brand === "folder" || brand === "link" ? GLYPHS[brand] : null;
  return (
    <div className={cn("grid size-10 shrink-0 place-items-center rounded-xl bg-white shadow-xs ring-1 ring-black/10", className)}>
      {Glyph ? (
        <Glyph className="size-5 text-[#5B6573]" aria-hidden />
      ) : (
        // Static logo markup from brand-logos.ts, not user input.
        <svg
          viewBox={LOGOS[brand as LogoBrand].viewBox}
          className="size-6"
          aria-hidden
          dangerouslySetInnerHTML={{ __html: LOGOS[brand as LogoBrand].body }}
        />
      )}
    </div>
  );
}
