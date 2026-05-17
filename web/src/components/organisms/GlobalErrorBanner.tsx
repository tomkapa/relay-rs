import { AlertTriangle } from "lucide-react";
import { useAuthStore } from "../../stores/authStore";
import { useActiveOrg } from "../../hooks/useMe";
import { Banner } from "../molecules/Banner";

export function GlobalErrorBanner() {
  const error = useAuthStore((s) => s.error);
  const clearError = useAuthStore((s) => s.clearError);
  const activeOrg = useActiveOrg();

  if (!error) return null;

  if (error.kind === "forbidden") {
    const orgLabel = activeOrg?.name ?? "this org";
    return (
      <div className="border-b border-[var(--color-line)] bg-[var(--color-paper-2)] px-3 py-2">
        <Banner
          variant="rose"
          icon={<AlertTriangle className="h-3.5 w-3.5" />}
          onDismiss={clearError}
        >
          No access to this resource in {orgLabel}.
        </Banner>
      </div>
    );
  }

  return null;
}
