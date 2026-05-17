import { useMemo } from "react";
import type { ReactNode } from "react";
import { Bot, ChevronDown, Hash, Plus, Search } from "lucide-react";
import { Button } from "../atoms/Button";
import { Kbd } from "../atoms/Kbd";
import { Monogram } from "../atoms/Monogram";
import { StatusSquare } from "../atoms/StatusSquare";
import { cn } from "../../lib/utils";
import type { Agent, ThreadSummary } from "../../types/api";

export function Sidebar({
  workspace = "Acme Robotics",
  threads,
  agents,
  selectedChannel,
  selectedAgentId,
  onSelectChannel,
  onSelectAgent,
  userName,
  orgSwitcher,
  userMenu,
}: {
  workspace?: string;
  threads: ThreadSummary[];
  agents: Agent[];
  selectedChannel: string;
  selectedAgentId: string | null;
  onSelectChannel: (channel: string) => void;
  onSelectAgent: (agentId: string) => void;
  userName: string;
  orgSwitcher?: ReactNode;
  userMenu?: ReactNode;
}) {
  const channels = [{ name: "general", icon: Hash, count: threads.length }];
  const threadCountByAgent = useMemo(() => {
    const m = new Map<string, number>();
    for (const t of threads) {
      m.set(t.first_agent.id, (m.get(t.first_agent.id) ?? 0) + 1);
    }
    return m;
  }, [threads]);

  return (
    <aside
      className="flex h-full w-[300px] shrink-0 flex-col border-r border-[var(--color-line)] bg-[var(--color-paper)]"
      aria-label="Channels and threads"
    >
      {/* Workspace picker — either the OrgSwitcher slot or a static fallback. */}
      <header className="flex items-center justify-between gap-2 border-b border-[var(--color-line)] px-4 py-3">
        {orgSwitcher ?? (
          <>
            <div className="min-w-0">
              <div className="font-[var(--font-mono)] text-[10px] uppercase tracking-[0.18em] text-[var(--color-muted)]">
                Relay
              </div>
              <div className="mt-0.5 truncate font-[var(--font-display)] text-[18px] font-bold tracking-tight text-[var(--color-ink)]">
                {workspace}
              </div>
            </div>
            <ChevronDown className="h-4 w-4 shrink-0 text-[var(--color-muted)]" />
          </>
        )}
      </header>

      {/* Search */}
      <div className="border-b border-[var(--color-line)] px-3 py-2.5">
        <div className="flex h-[34px] items-center gap-2 border border-[var(--color-line)] bg-[var(--color-card)] px-2.5">
          <Search className="h-3.5 w-3.5 text-[var(--color-muted)]" />
          <input
            placeholder="Search workspace"
            className="w-full bg-transparent font-[var(--font-mono)] text-[12px] outline-none placeholder:text-[var(--color-muted-2)]"
          />
          <Kbd>⌘K</Kbd>
        </div>
      </div>

      <div className="flex-1 overflow-y-auto scroll-thin px-2 py-2">
        {/* CHANNELS */}
        <Section
          title="Channels"
          expandable
          action={<AddBtn label="Add channel" />}
        />
        <div className="mb-2 flex flex-col gap-0.5">
          {channels.map((c) => (
            <SidebarRow
              key={c.name}
              icon={<c.icon className="h-3 w-3 text-[var(--color-muted)]" />}
              label={c.name}
              prefix="#"
              trailing={
                c.count != null ? (
                  <span className="bg-[var(--color-paper-3)] px-1 font-[var(--font-mono)] text-[10px] text-[var(--color-muted)]">
                    {c.count}
                  </span>
                ) : null
              }
              active={c.name === selectedChannel}
              onClick={() => onSelectChannel(c.name)}
              mono
            />
          ))}
        </div>

        {/* DIRECT MESSAGES — agents list, opens agent-scoped feed on click. */}
        <Section
          title="Direct Messages"
          expandable
          action={<AddBtn label="New DM" />}
        />
        <div className="mb-2 flex flex-col gap-0.5">
          {agents.map((a) => {
            const count = threadCountByAgent.get(a.id) ?? 0;
            return (
              <SidebarRow
                key={a.id}
                icon={
                  <Monogram name={a.name} id={a.id} size={20} tone="moss" />
                }
                label={a.name}
                trailing={
                  <span className="inline-flex items-center gap-1.5">
                    <Bot className="h-3.5 w-3.5 text-[var(--color-moss)]" />
                    {count > 0 && (
                      <span className="bg-[var(--color-paper-3)] px-1 font-[var(--font-mono)] text-[10px] text-[var(--color-muted)]">
                        {count}
                      </span>
                    )}
                  </span>
                }
                active={selectedAgentId === a.id}
                onClick={() => onSelectAgent(a.id)}
                mono
              />
            );
          })}
          {agents.length === 0 && (
            <p className="px-2 py-1 font-[var(--font-mono)] text-[11px] text-[var(--color-muted-2)]">
              No agents registered.
            </p>
          )}
        </div>
      </div>

      {/* User bar */}
      <footer className="flex items-center gap-2.5 border-t border-[var(--color-line)] bg-[var(--color-card)] px-3 py-2.5">
        <Monogram name={userName} id="user" tone="user" size={32} />
        <div className="min-w-0 flex-1">
          <div className="truncate text-[13px] font-semibold text-[var(--color-ink)]">
            {userName}
          </div>
          <div className="flex items-center gap-1.5 font-[var(--font-mono)] text-[10.5px] text-[var(--color-muted)]">
            <StatusSquare status="live" size={6} />
            online · operator
          </div>
        </div>
        {userMenu}
      </footer>
    </aside>
  );
}

function Section({
  title,
  expandable,
  action,
}: {
  title: string;
  expandable?: boolean;
  action?: React.ReactNode;
}) {
  return (
    <div className="mt-2 mb-1 flex items-center gap-1.5 px-2 h-[24px]">
      {expandable && (
        <ChevronDown className="h-3 w-3 text-[var(--color-muted-2)]" />
      )}
      <span className="font-[var(--font-mono)] text-[10px] uppercase tracking-[0.16em] text-[var(--color-muted)]">
        {title}
      </span>
      <span className="ml-auto">{action}</span>
    </div>
  );
}

function AddBtn({ label }: { label: string }) {
  return (
    <Button variant="ghost" size="xxs" iconOnly aria-label={label}>
      <Plus className="h-3.5 w-3.5" />
    </Button>
  );
}

function SidebarRow({
  icon,
  label,
  prefix,
  trailing,
  active,
  muted,
  mono,
  title,
  onClick,
}: {
  icon?: React.ReactNode;
  label: React.ReactNode;
  prefix?: string;
  trailing?: React.ReactNode;
  active?: boolean;
  muted?: boolean;
  mono?: boolean;
  title?: string;
  onClick?: () => void;
}) {
  return (
    <button
      onClick={onClick}
      title={title}
      className={cn(
        "group flex h-[28px] w-full items-center gap-2 px-2 text-left text-[13px] transition-colors",
        mono && "font-[var(--font-mono)] text-[12.5px]",
        active
          ? "bg-[var(--color-rail)] text-[var(--color-paper)] font-medium"
          : muted
            ? "text-[var(--color-muted)] hover:bg-[var(--color-paper-2)] hover:text-[var(--color-ink)]"
            : "text-[var(--color-ink)] hover:bg-[var(--color-paper-2)]",
      )}
    >
      {icon && <span className="shrink-0">{icon}</span>}
      <span className="flex-1 truncate">
        {prefix && (
          <span
            className={cn(
              "mr-0.5",
              active
                ? "text-[var(--color-paper)]"
                : "text-[var(--color-muted)]",
            )}
          >
            {prefix}
          </span>
        )}
        {label}
      </span>
      {trailing && <span className="shrink-0">{trailing}</span>}
    </button>
  );
}
