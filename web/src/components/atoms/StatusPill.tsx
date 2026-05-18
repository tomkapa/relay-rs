import { cn } from "../../lib/utils";
import {
  STATUS_BG,
  STATUS_COLOR,
  type StatusTone,
} from "../../data/connectionStatus";

export function StatusPill({
  tone,
  label,
  className,
}: {
  tone: StatusTone;
  label: string;
  className?: string;
}) {
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 px-2.5 py-1 font-[var(--font-mono)] text-[11px] font-semibold tracking-[0.02em]",
        className,
      )}
      style={{
        background: STATUS_BG[tone],
        color: STATUS_COLOR[tone],
        border: `1px solid ${STATUS_COLOR[tone]}`,
      }}
    >
      <span
        aria-hidden
        className="block h-2 w-2 rounded-full"
        style={{ background: STATUS_COLOR[tone] }}
      />
      <span>{label}</span>
    </span>
  );
}
