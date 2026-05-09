import type { ReactNode } from "react";
import { cn } from "../../lib/utils";

/**
 * ASCII-style boxed mono label. Used for date dividers and "NEW MESSAGES"
 * markers — the bordered hairline + uppercase letterspaced mono is the
 * spec's "field journal" tell.
 */
export function BoxedLabel({
  children,
  variant = "default",
  className,
}: {
  children: ReactNode;
  variant?: "default" | "moss" | "rose";
  className?: string;
}) {
  const tone =
    variant === "moss"
      ? "border-[var(--color-moss)] text-[var(--color-moss-deep)] bg-[var(--color-moss-tint)]"
      : variant === "rose"
        ? "border-[var(--color-rose)] text-[var(--color-rose)] bg-[var(--color-rose-soft)]"
        : "border-[var(--color-line-strong)] text-[var(--color-ink)] bg-[var(--color-paper)]";
  return (
    <div className="my-3 flex items-center gap-3">
      <span className="h-px flex-1 bg-[var(--color-line)]" />
      <span
        className={cn(
          "inline-flex h-[22px] items-center px-2.5 border font-[var(--font-mono)] text-[10.5px] font-medium uppercase tracking-[0.16em]",
          tone,
          className,
        )}
      >
        {children}
      </span>
      <span className="h-px flex-1 bg-[var(--color-line)]" />
    </div>
  );
}
