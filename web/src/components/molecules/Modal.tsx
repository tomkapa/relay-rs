import { useEffect, useRef, type ReactNode } from "react";
import { X } from "lucide-react";
import { cn } from "../../lib/utils";
import { useT } from "../../i18n";

// Scrim is `--color-rail` at 80% alpha (matches the design frames).
export function Modal({
  open,
  onClose,
  children,
  width = 460,
  ariaLabel,
}: {
  open: boolean;
  onClose: () => void;
  children: ReactNode;
  width?: number;
  ariaLabel?: string;
}) {
  const dialogRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  useEffect(() => {
    if (!open) return;
    const prevOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      document.body.style.overflow = prevOverflow;
    };
  }, [open]);

  if (!open) return null;

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label={ariaLabel}
      className="fixed inset-0 z-50 flex items-center justify-center bg-[#1E3322CC] p-8"
      onMouseDown={(e) => {
        // Close on backdrop click only (not when dragging from inside).
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        ref={dialogRef}
        className="max-h-[calc(100vh-64px)] w-full overflow-auto border border-[var(--color-line)] bg-[var(--color-card)] shadow-xl"
        style={{ maxWidth: width }}
      >
        {children}
      </div>
    </div>
  );
}

export function ModalHeader({
  eyebrow,
  title,
  icon,
  onClose,
}: {
  eyebrow: ReactNode;
  title: ReactNode;
  icon?: ReactNode;
  onClose: () => void;
}) {
  const { t } = useT();
  return (
    <div className="flex items-start gap-3 border-b border-[var(--color-line)] px-5 pt-5 pb-4">
      {icon ? <div className="shrink-0 pt-0.5">{icon}</div> : null}
      <div className="min-w-0 flex-1">
        <div className="font-[var(--font-mono)] text-[10px] tracking-[0.14em] text-[var(--color-muted)] uppercase">
          {eyebrow}
        </div>
        <div className="mt-1 font-[var(--font-display)] text-[18px] leading-tight font-semibold text-[var(--color-ink)]">
          {title}
        </div>
      </div>
      <button
        type="button"
        aria-label={t("connections.modal.close")}
        onClick={onClose}
        className="-mt-1 -mr-1 shrink-0 p-1 text-[var(--color-muted)] hover:text-[var(--color-ink)]"
      >
        <X className="h-4 w-4" />
      </button>
    </div>
  );
}

export function ModalFooter({
  left,
  children,
}: {
  left?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-3 border-t border-[var(--color-line)] bg-[var(--color-paper-2)] px-5 py-3">
      <div className="flex min-w-0 items-center gap-2">{left}</div>
      <div className="flex shrink-0 items-center gap-2">{children}</div>
    </div>
  );
}
