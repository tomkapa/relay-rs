import { cn } from "../../lib/utils";

export function CountBadge({
  children,
  tone = "ink",
  className,
}: {
  children: React.ReactNode;
  tone?: "ink" | "moss";
  className?: string;
}) {
  const palette =
    tone === "moss"
      ? "bg-[var(--color-moss)] text-white"
      : "bg-[var(--color-ink)] text-[var(--color-paper)]";
  return (
    <span
      className={cn(
        "inline-flex items-center px-2 py-0.5 font-[var(--font-mono)] text-[11px] font-semibold leading-none",
        palette,
        className,
      )}
    >
      {children}
    </span>
  );
}
