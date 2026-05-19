import { SectionCard } from "../molecules/SectionCard";
import { SectionHeader } from "../atoms/SectionHeader";
import { Spinner } from "../atoms/Spinner";
import { useT } from "../../i18n";
import type { McpServer } from "../../types/api";
import { AllowlistRow } from "./AllowlistRow";
import { type Allowlist } from "./allowlistState";

export function AllowlistCard({
  servers,
  list,
  onChange,
  loading,
}: {
  servers: McpServer[];
  list: Allowlist;
  onChange: (next: Allowlist) => void;
  loading: boolean;
}) {
  const { t } = useT();
  const enabledCount = Object.keys(list).length;

  return (
    <SectionCard
      header={
        <SectionHeader
          eyebrow={
            <>
              {t("agent.detail.tools.sectionEyebrow")}
              <span className="font-[var(--font-mono)] text-[10px] font-normal normal-case tracking-normal text-[var(--color-muted)]">
                {t("agent.detail.tools.sectionCount", {
                  enabled: String(enabledCount),
                  total: String(servers.length),
                })}
              </span>
            </>
          }
        />
      }
    >
      {loading ? (
        <div className="flex items-center justify-center px-5 py-10">
          <Spinner size={16} />
        </div>
      ) : servers.length === 0 ? (
        <div className="px-5 py-10 text-center text-[13px] text-[var(--color-muted)]">
          {t("agent.detail.tools.empty")}
        </div>
      ) : (
        servers.map((server) => (
          <AllowlistRow
            key={server.id}
            server={server}
            list={list}
            onChange={onChange}
          />
        ))
      )}
    </SectionCard>
  );
}
