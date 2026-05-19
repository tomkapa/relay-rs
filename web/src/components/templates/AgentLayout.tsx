import type { ReactNode } from "react";
import { BarChart3, BookText, Cpu, Shield } from "lucide-react";
import { MenuRail } from "../organisms/MenuRail";
import { GlobalErrorBanner } from "../organisms/GlobalErrorBanner";
import { Monogram } from "../atoms/Monogram";
import { useT } from "../../i18n";
import { cn } from "../../lib/utils";
import type { Agent } from "../../types/api";

/** Per-agent settings sub-navigation. Today only `tools` has a real page;
 *  the other three sit in the design as placeholders and render as
 *  `aria-disabled` items so the sidebar matches the design without
 *  inventing routes. */
type AgentNavId = "prompt" | "model" | "tools" | "logs";

export function AgentLayout({
  agent,
  active,
  children,
}: {
  agent: Agent | null;
  active: AgentNavId;
  children: ReactNode;
}) {
  const { t } = useT();

  const navItems: {
    id: AgentNavId;
    label: string;
    icon: typeof Shield;
    disabled?: boolean;
  }[] = [
    {
      id: "prompt",
      label: t("agent.detail.nav.prompt"),
      icon: BookText,
      disabled: true,
    },
    {
      id: "model",
      label: t("agent.detail.nav.model"),
      icon: Cpu,
      disabled: true,
    },
    { id: "tools", label: t("agent.detail.nav.tools"), icon: Shield },
    {
      id: "logs",
      label: t("agent.detail.nav.logs"),
      icon: BarChart3,
      disabled: true,
    },
  ];

  return (
    <div className="flex h-screen w-screen overflow-hidden bg-[var(--color-paper)]">
      <MenuRail />
      <aside
        className="flex h-full w-[260px] shrink-0 flex-col border-r border-[var(--color-line)] bg-[var(--color-paper-2)]"
        aria-label={t("agent.detail.nav.aria")}
      >
        <div className="border-b border-[var(--color-line)] px-5 pt-5 pb-4">
          <div className="font-[var(--font-mono)] text-[10px] tracking-[0.15em] text-[var(--color-muted)] uppercase">
            {t("agent.detail.nav.eyebrow")}
          </div>
          <div className="mt-2 flex items-center gap-2.5">
            <Monogram
              name={agent?.name ?? "—"}
              id={agent?.id}
              size={32}
              tone="moss"
            />
            <div className="min-w-0 flex-1">
              <div className="truncate font-[var(--font-display)] text-[18px] leading-tight font-bold text-[var(--color-ink)]">
                {agent?.name ?? "…"}
              </div>
            </div>
          </div>
        </div>
        <nav className="flex flex-col gap-0.5 p-2">
          <div className="px-3 pt-2 pb-1 font-[var(--font-mono)] text-[10px] tracking-[0.15em] text-[var(--color-muted)] uppercase">
            {t("agent.detail.nav.section")}
          </div>
          {navItems.map((it) => {
            const Icon = it.icon;
            const isActive = active === it.id;
            return (
              <button
                key={it.id}
                type="button"
                aria-current={isActive ? "page" : undefined}
                aria-disabled={it.disabled ? "true" : undefined}
                disabled={it.disabled}
                className={cn(
                  "group flex items-center gap-2.5 px-3 py-2 text-left transition-colors",
                  isActive
                    ? "border-l-2 border-[var(--color-moss)] bg-[var(--color-moss-tint)] text-[var(--color-moss-deep)]"
                    : it.disabled
                      ? "cursor-not-allowed border-l-2 border-transparent text-[var(--color-muted-2)] opacity-60"
                      : "cursor-pointer border-l-2 border-transparent text-[var(--color-muted)] hover:text-[var(--color-ink)]",
                )}
              >
                <Icon
                  className={cn(
                    "h-4 w-4 shrink-0",
                    isActive
                      ? "text-[var(--color-moss)]"
                      : "text-[var(--color-muted-2)]",
                  )}
                  strokeWidth={1.75}
                />
                <span
                  className={cn(
                    "min-w-0 flex-1 truncate text-[13px]",
                    isActive ? "font-medium" : "font-normal",
                  )}
                >
                  {it.label}
                </span>
              </button>
            );
          })}
        </nav>
      </aside>
      <main className="flex min-w-0 flex-1 flex-col bg-[var(--color-card)]">
        <GlobalErrorBanner />
        {children}
      </main>
    </div>
  );
}

export function AgentBreadcrumb({
  trail,
}: {
  trail: { label: string; current?: boolean }[];
}) {
  return (
    <div className="flex items-center gap-2 px-8 pt-4 pb-3 font-[var(--font-mono)] text-[11px] text-[var(--color-muted)]">
      {trail.map((step, i) => (
        <span key={`${step.label}-${i}`} className="flex items-center gap-2">
          <span
            className={cn(
              step.current && "font-semibold text-[var(--color-ink)]",
            )}
          >
            {step.label}
          </span>
          {i < trail.length - 1 ? <span aria-hidden>/</span> : null}
        </span>
      ))}
    </div>
  );
}
