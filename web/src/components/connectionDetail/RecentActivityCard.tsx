import { ArrowRight, Check, X } from "lucide-react";
import { SectionHeader } from "../atoms/SectionHeader";
import { SectionCard } from "../molecules/SectionCard";
import { useT } from "../../i18n";

type MockRow = {
  time: string;
  tool: string;
  agent: string;
  agentColor?: string;
  latency: string;
  ok: boolean;
  outcome: string;
};

const MOCK_ROWS: MockRow[] = [
  {
    time: "2m ago",
    tool: "pages.search",
    agent: "Atlas",
    latency: "142ms",
    ok: true,
    outcome: "200 OK",
  },
  {
    time: "14m ago",
    tool: "databases.query",
    agent: "Atlas",
    latency: "618ms",
    ok: true,
    outcome: "200 OK",
  },
  {
    time: "22m ago",
    tool: "comments.create",
    agent: "Beacon",
    latency: "203ms",
    ok: true,
    outcome: "200 OK",
  },
  {
    time: "1h ago",
    tool: "pages.create",
    agent: "Atlas",
    latency: "—",
    ok: false,
    outcome: "403 forbidden_page",
  },
  {
    time: "3h ago",
    tool: "pages.search",
    agent: "Atlas",
    latency: "96ms",
    ok: true,
    outcome: "200 OK",
  },
];

const COL_WIDTHS = "minmax(76px,90px) minmax(0,1fr) minmax(96px,140px) 72px minmax(120px,180px)";

export function RecentActivityCard() {
  const { t } = useT();

  return (
    <SectionCard
      className="relative"
      header={
        <SectionHeader
          eyebrow={
            <>
              {t("connections.detail.activity.eyebrow")}
              <span className="font-[var(--font-mono)] text-[10px] font-normal normal-case tracking-normal text-[var(--color-muted)]">
                {t("connections.detail.activity.last50")}
              </span>
            </>
          }
          right={
            <span
              aria-disabled="true"
              className="inline-flex items-center gap-1 text-[12px] text-[var(--color-muted)]"
            >
              {t("connections.detail.activity.audit")}
              <ArrowRight className="h-3 w-3" strokeWidth={1.75} aria-hidden />
            </span>
          }
        />
      }
    >
      <div className="relative">
        <div className="pointer-events-none select-none opacity-55 grayscale">
          <div
            className="grid items-center gap-3 border-b border-[var(--color-line)] bg-[var(--color-paper-2)] px-5 py-2.5 font-[var(--font-mono)] text-[9px] tracking-[0.15em] uppercase text-[var(--color-muted)]"
            style={{ gridTemplateColumns: COL_WIDTHS }}
          >
            <span>{t("connections.detail.activity.col.time")}</span>
            <span>{t("connections.detail.activity.col.tool")}</span>
            <span>{t("connections.detail.activity.col.agent")}</span>
            <span>{t("connections.detail.activity.col.latency")}</span>
            <span className="text-right">
              {t("connections.detail.activity.col.outcome")}
            </span>
          </div>
          {MOCK_ROWS.map((row, i) => (
            <div
              key={`${row.time}-${row.tool}-${i}`}
              className={`grid items-center gap-3 border-b border-[var(--color-line)] px-5 py-2.5 last:border-b-0 ${
                row.ok ? "" : "bg-[var(--color-rose-soft)]"
              }`}
              style={{ gridTemplateColumns: COL_WIDTHS }}
            >
              <span className="font-[var(--font-mono)] text-[12px] text-[var(--color-muted)]">
                {row.time}
              </span>
              <span className="truncate font-[var(--font-mono)] text-[12px] text-[var(--color-ink)]">
                {row.tool}
              </span>
              <span className="truncate text-[12px] text-[var(--color-moss-deep)]">
                {row.agent}
              </span>
              <span className="font-[var(--font-mono)] text-[12px] text-[var(--color-ink)]">
                {row.latency}
              </span>
              <span
                className={`flex items-center justify-end gap-1.5 font-[var(--font-mono)] text-[12px] ${
                  row.ok
                    ? "text-[var(--color-ink)]"
                    : "font-semibold text-[var(--color-rose)]"
                }`}
              >
                {row.ok ? (
                  <Check
                    className="h-3 w-3 text-[var(--color-moss)]"
                    strokeWidth={2}
                    aria-hidden
                  />
                ) : (
                  <X
                    className="h-3 w-3 text-[var(--color-rose)]"
                    strokeWidth={2}
                    aria-hidden
                  />
                )}
                {row.outcome}
              </span>
            </div>
          ))}
        </div>
        <span className="absolute right-3 top-3 z-10 inline-flex items-center gap-1.5 border border-[var(--color-line)] bg-[var(--color-card)] px-2.5 py-1 font-[var(--font-mono)] text-[10px] font-semibold uppercase tracking-[0.12em] text-[var(--color-ink)] shadow-sm">
          <span
            aria-hidden
            className="h-1.5 w-1.5 rounded-full bg-[var(--color-amber)]"
          />
          {t("connections.detail.activity.comingSoon")}
        </span>
      </div>
    </SectionCard>
  );
}
