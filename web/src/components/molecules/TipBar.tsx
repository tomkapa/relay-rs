import { AtSign } from "lucide-react";
import type { ReactNode } from "react";

export function TipBar({ children }: { children: ReactNode }) {
  return (
    <div className="flex items-center gap-2 border border-[var(--color-moss)] bg-[var(--color-moss-tint)] px-3 py-1.5 font-[var(--font-mono)] text-[11.5px] leading-[1.4] text-[var(--color-moss-deep)]">
      <AtSign className="h-3 w-3 shrink-0 text-[var(--color-moss)]" />
      {children}
    </div>
  );
}
