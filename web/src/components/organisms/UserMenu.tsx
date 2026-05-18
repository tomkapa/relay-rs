import { useCallback, useRef, useState } from "react";
import { LogOut, MoreHorizontal } from "lucide-react";
import { useAuthStore } from "../../stores/authStore";
import { useLogout } from "../../hooks/useLogout";
import { useDismissable } from "../../hooks/useDismissable";
import { Button } from "../atoms/Button";
import { useT } from "../../i18n";
import type { TranslationKey } from "../../i18n/en";
import { api } from "../../lib/api";
import type { Language } from "../../types/api";

// Exhaustive list of selectable languages. Adding a `Language` variant
// without extending this array is a TypeScript error at the `label` key
// (it's typed `TranslationKey`, and `usermenu.language.<new>` won't
// exist in the en/vi tables until the translation is added).
const LANGUAGE_OPTIONS: { value: Language; label: TranslationKey }[] = [
  { value: "en", label: "usermenu.language.en" },
  { value: "vi", label: "usermenu.language.vi" },
];

export function UserMenu() {
  const me = useAuthStore((s) => s.me);
  const logout = useLogout();
  const { t, language } = useT();
  const [open, setOpen] = useState(false);
  const [langError, setLangError] = useState<string | null>(null);
  const [langPending, setLangPending] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const close = useCallback(() => setOpen(false), []);
  useDismissable(rootRef, open, close);

  if (!me) return null;

  // Backend authority: only owner/admin can switch. The select is hidden
  // for plain members. Server enforces the same check; this is UX only.
  const canSwitchLanguage = me.role === "owner" || me.role === "admin";

  const onChangeLanguage = async (next: Language) => {
    if (langPending || next === language) return;
    setLangPending(true);
    setLangError(null);
    try {
      const { default_language } = await api.setOrgLanguage(next);
      // Read the latest store snapshot rather than the closed-over
      // `me`: an org switch or /me re-poll that landed while we were
      // awaiting the PATCH would otherwise be clobbered by the stale
      // copy here.
      const latest = useAuthStore.getState().me;
      if (!latest) return;
      useAuthStore.getState().setMe({
        ...latest,
        orgs: latest.orgs.map((o) =>
          o.id === latest.active_org_id ? { ...o, default_language } : o,
        ),
      });
    } catch {
      setLangError(t("usermenu.language.error"));
    } finally {
      setLangPending(false);
    }
  };

  return (
    <div ref={rootRef} className="relative">
      <Button
        variant="ghost"
        size="sm"
        iconOnly
        aria-label="User menu"
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        <MoreHorizontal className="h-4 w-4" />
      </Button>

      {open ? (
        <div
          role="menu"
          className="absolute bottom-full right-0 z-20 mb-1 w-[240px] border border-[var(--color-line)] bg-[var(--color-card)] py-1 shadow-md"
        >
          <div className="border-b border-[var(--color-line)] px-3 py-2">
            <div className="truncate text-[12px] font-semibold text-[var(--color-ink)]">
              {me.user.display_name ?? me.user.email}
            </div>
            <div className="truncate font-[var(--font-mono)] text-[10.5px] text-[var(--color-muted)]">
              {me.user.email}
            </div>
          </div>

          {canSwitchLanguage ? (
            <div className="border-b border-[var(--color-line)] px-3 py-2">
              <label
                htmlFor="usermenu-language"
                className="block font-[var(--font-mono)] text-[10.5px] uppercase tracking-[0.18em] text-[var(--color-muted)]"
              >
                {t("usermenu.language.label")}
              </label>
              <select
                id="usermenu-language"
                value={language}
                disabled={langPending}
                onChange={(e) => onChangeLanguage(e.target.value as Language)}
                className="mt-1 w-full border border-[var(--color-line)] bg-[var(--color-paper)] px-2 py-1 text-[12px] text-[var(--color-ink)] focus:outline-none focus:ring-1 focus:ring-[var(--color-ink)] disabled:opacity-50"
              >
                {LANGUAGE_OPTIONS.map(({ value, label }) => (
                  <option key={value} value={value}>
                    {t(label)}
                  </option>
                ))}
              </select>
              {langError ? (
                <p className="mt-1 text-[10.5px] text-[var(--color-rose)]">
                  {langError}
                </p>
              ) : null}
            </div>
          ) : null}

          <button
            type="button"
            role="menuitem"
            onClick={() => logout.mutate()}
            disabled={logout.isPending}
            className="flex w-full items-center gap-2 px-3 py-2 text-left text-[12px] text-[var(--color-rose)] hover:bg-[var(--color-rose-soft)] disabled:opacity-50"
          >
            <LogOut className="h-3.5 w-3.5" />
            {t("usermenu.signout")}
          </button>
        </div>
      ) : null}
    </div>
  );
}
