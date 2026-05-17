import type { ReactNode } from "react";
import { GlobalErrorBanner } from "../organisms/GlobalErrorBanner";

export function ChatLayout({
  rail,
  sidebar,
  main,
  panel,
}: {
  rail: ReactNode;
  sidebar: ReactNode;
  main: ReactNode;
  panel: ReactNode;
}) {
  return (
    <div className="flex h-screen w-screen overflow-hidden bg-[var(--color-surface)]">
      {rail}
      {sidebar}
      <main className="flex min-w-0 flex-1 flex-col">
        <GlobalErrorBanner />
        {main}
      </main>
      {panel}
    </div>
  );
}
