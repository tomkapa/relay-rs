import type { ReactNode } from "react";
import { cn } from "../../lib/utils";

export function SectionCard({
  header,
  footer,
  children,
  tone = "neutral",
  className,
  bodyClassName,
}: {
  header?: ReactNode;
  footer?: ReactNode;
  children?: ReactNode;
  tone?: "neutral" | "danger";
  className?: string;
  bodyClassName?: string;
}) {
  return (
    <section
      className={cn(
        "relative flex flex-col bg-[var(--color-card)] border",
        tone === "danger"
          ? "border-[var(--color-rose)]"
          : "border-[var(--color-line)]",
        className,
      )}
    >
      {header}
      <div className={cn("min-w-0", bodyClassName)}>{children}</div>
      {footer}
    </section>
  );
}
