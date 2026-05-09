import { cn } from "../../lib/utils";

type Status = "live" | "idle" | "error" | "muted" | "outline";

const map: Record<Status, string> = {
  live: "bg-[var(--color-moss)] border-[var(--color-moss)]",
  idle: "bg-[var(--color-amber)] border-[var(--color-amber)]",
  error: "bg-[var(--color-rose)] border-[var(--color-rose)]",
  muted: "bg-[var(--color-line-2)] border-[var(--color-line-2)]",
  outline: "border-[var(--color-line-strong)] bg-transparent",
};

export function StatusSquare({
  status,
  size = 8,
  className,
}: {
  status: Status;
  size?: number;
  className?: string;
}) {
  return (
    <span
      aria-hidden="true"
      className={cn("inline-block border", map[status], className)}
      style={{ width: size, height: size }}
    />
  );
}
