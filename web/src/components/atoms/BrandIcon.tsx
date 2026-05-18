import { BRAND_ICONS } from "../../data/brandIcons";
import { cn } from "../../lib/utils";

/** Renders an inline SVG brand icon by slug (e.g. "notion", "slack").
 *  Falls back to rendering nothing if the slug is unknown. The icon is
 *  always displayed in white so callers control the surrounding tile
 *  background for branded colour. */
export function BrandIcon({
  slug,
  size = 20,
  className,
}: {
  slug: string;
  size?: number;
  className?: string;
}) {
  const icon = BRAND_ICONS[slug];
  if (!icon) return null;
  return (
    <svg
      aria-hidden
      viewBox="0 0 24 24"
      width={size}
      height={size}
      fill="currentColor"
      className={cn("shrink-0", className)}
    >
      <path d={icon.path} />
    </svg>
  );
}

/** Returns the brand hex colour for a given slug, or undefined if unknown. */
export function brandHex(slug: string): string | undefined {
  return BRAND_ICONS[slug]?.hex;
}
