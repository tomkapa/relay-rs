import { cn, initials, toneById } from "../../lib/utils";

type Tone = "moss" | "amber" | "rose" | "ink" | "neutral" | "user";

const toneStyles: Record<Tone, string> = {
  moss: "bg-[var(--color-moss-soft)] text-[var(--color-moss-deep)] border-[var(--color-moss-soft)]",
  amber:
    "bg-[var(--color-amber-soft)] text-[var(--color-amber)] border-[var(--color-amber-soft)]",
  rose: "bg-[var(--color-rose-soft)] text-[var(--color-rose)] border-[var(--color-rose-soft)]",
  ink: "bg-[var(--color-rail)] text-[var(--color-paper)] border-[var(--color-rail)]",
  neutral:
    "bg-[var(--color-paper-2)] text-[var(--color-ink)] border-[var(--color-line)]",
  user: "bg-[var(--color-moss)] text-white border-[var(--color-moss)]",
};

export function Monogram({
  name,
  id,
  size = 28,
  tone,
  className,
}: {
  name: string;
  id?: string;
  size?: number;
  tone?: Tone;
  className?: string;
}) {
  const t = tone ?? toneById(id);
  return (
    <span
      className={cn(
        "inline-flex shrink-0 items-center justify-center border font-[var(--font-mono)] font-semibold uppercase tracking-tight select-none",
        toneStyles[t],
        className,
      )}
      style={{
        width: size,
        height: size,
        fontSize: Math.round(size * 0.4),
      }}
      aria-label={name}
    >
      {initials(name)}
    </span>
  );
}
