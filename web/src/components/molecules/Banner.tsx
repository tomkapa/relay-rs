import { X } from "lucide-react";
import type { ReactNode } from "react";
import { cn } from "../../lib/utils";

type Variant = "rose" | "amber" | "moss";

const VARIANT: Record<Variant, string> = {
  rose: "border-[var(--color-rose)] bg-[var(--color-rose-soft)] text-[var(--color-rose)]",
  amber:
    "border-[var(--color-amber)] bg-[var(--color-amber-soft)] text-[var(--color-amber)]",
  moss: "border-[var(--color-moss)] bg-[var(--color-moss-tint)] text-[var(--color-moss-deep)]",
};

export function Banner({
  variant = "rose",
  icon,
  children,
  onDismiss,
  className,
}: {
  variant?: Variant;
  icon?: ReactNode;
  children: ReactNode;
  onDismiss?: () => void;
  className?: string;
}) {
  return (
    <div
      role="alert"
      className={cn(
        "flex items-center gap-2 border px-3 py-2 font-[var(--font-mono)] text-[11.5px] leading-[1.4]",
        VARIANT[variant],
        className,
      )}
    >
      {icon ? <span className="shrink-0">{icon}</span> : null}
      <div className="min-w-0 flex-1">{children}</div>
      {onDismiss ? (
        <button
          type="button"
          aria-label="Dismiss"
          onClick={onDismiss}
          className="shrink-0 opacity-70 hover:opacity-100"
        >
          <X className="h-3.5 w-3.5" />
        </button>
      ) : null}
    </div>
  );
}
