import { useCallback, useRef, useState, type ReactNode } from "react";
import { useNavigate } from "react-router-dom";
import { BarChart3, BookText, Check, ChevronsUpDown, Cpu, Shield } from "lucide-react";
import { MenuRail } from "../organisms/MenuRail";
import { GlobalErrorBanner } from "../organisms/GlobalErrorBanner";
import { Monogram } from "../atoms/Monogram";
import { useT } from "../../i18n";
import { cn } from "../../lib/utils";
import { useAgents } from "../../hooks/useAgents";
import { useDismissable } from "../../hooks/useDismissable";
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
          <AgentSwitcher current={agent} />
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

function AgentSwitcher({ current }: { current: Agent | null }) {
  const { t } = useT();
  const nav = useNavigate();
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const close = useCallback(() => setOpen(false), []);
  useDismissable(rootRef, open, close);
  const list = useAgents();
  const agents = list.data ?? [];

  const pick = (id: string) => {
    setOpen(false);
    if (id !== current?.id) nav(`/agents/${id}`);
  };

  return (
    <div ref={rootRef} className="relative mt-2">
      <button
        type="button"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={t("agent.detail.switcher.aria")}
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-2.5 text-left outline-none focus-visible:ring-1 focus-visible:ring-[var(--color-ink)]"
      >
        <Monogram
          name={current?.name ?? "—"}
          id={current?.id}
          size={32}
          tone="moss"
        />
        <div className="min-w-0 flex-1">
          <div className="truncate font-[var(--font-display)] text-[18px] leading-tight font-bold text-[var(--color-ink)]">
            {current?.name ?? "…"}
          </div>
        </div>
        <ChevronsUpDown
          className="h-4 w-4 shrink-0 text-[var(--color-muted)]"
          strokeWidth={1.75}
        />
      </button>

      {open ? (
        <ul
          role="listbox"
          aria-label={t("agent.detail.switcher.aria")}
          className="absolute left-0 right-0 top-full z-20 mt-1 max-h-[60vh] overflow-y-auto border border-[var(--color-line)] bg-[var(--color-card)] py-1 shadow-md scroll-thin"
        >
          {agents.length === 0 ? (
            <li className="px-3 py-2 text-[12.5px] text-[var(--color-muted)]">
              {t("agent.detail.switcher.empty")}
            </li>
          ) : (
            agents.map((a) => {
              const isActive = a.id === current?.id;
              return (
                <li key={a.id}>
                  <button
                    type="button"
                    role="option"
                    aria-selected={isActive}
                    onClick={() => pick(a.id)}
                    className="flex w-full items-center gap-2.5 px-3 py-2 text-left hover:bg-[var(--color-paper-2)]"
                  >
                    <Monogram name={a.name} id={a.id} size={24} tone="moss" />
                    <div className="min-w-0 flex-1 truncate text-[13px] font-semibold text-[var(--color-ink)]">
                      {a.name}
                    </div>
                    {isActive ? (
                      <Check className="h-3.5 w-3.5 shrink-0 text-[var(--color-moss)]" />
                    ) : null}
                  </button>
                </li>
              );
            })
          )}
        </ul>
      ) : null}
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
