import { useState, type ReactNode } from "react";
import { ChevronRight } from "lucide-react";
import { cn } from "../../lib/utils";

export function Collapsible({
  trigger,
  children,
  defaultOpen = false,
  className,
}: {
  trigger: ReactNode;
  children: ReactNode;
  defaultOpen?: boolean;
  className?: string;
}) {
  const [open, setOpen] = useState(defaultOpen);
  return (
    <div className={cn("w-full", className)}>
      <button
        onClick={() => setOpen((v) => !v)}
        className="group flex w-full items-center gap-1.5 text-left text-[12px] font-medium tracking-tight text-[var(--color-muted)] hover:text-[var(--color-ink)] transition-colors"
        aria-expanded={open}
      >
        <ChevronRight
          className={cn(
            "h-3.5 w-3.5 transition-transform duration-150",
            open && "rotate-90",
          )}
        />
        <span className="flex-1">{trigger}</span>
      </button>
      {open && <div className="mt-1.5">{children}</div>}
    </div>
  );
}
