import { useCallback, useRef, useState } from "react";
import { LogOut, MoreHorizontal } from "lucide-react";
import { useAuthStore } from "../../stores/authStore";
import { useLogout } from "../../hooks/useLogout";
import { useDismissable } from "../../hooks/useDismissable";
import { Button } from "../atoms/Button";

export function UserMenu() {
  const me = useAuthStore((s) => s.me);
  const logout = useLogout();
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const close = useCallback(() => setOpen(false), []);
  useDismissable(rootRef, open, close);

  if (!me) return null;

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
          className="absolute bottom-full right-0 z-20 mb-1 w-[220px] border border-[var(--color-line)] bg-[var(--color-card)] py-1 shadow-md"
        >
          <div className="border-b border-[var(--color-line)] px-3 py-2">
            <div className="truncate text-[12px] font-semibold text-[var(--color-ink)]">
              {me.user.display_name ?? me.user.email}
            </div>
            <div className="truncate font-[var(--font-mono)] text-[10.5px] text-[var(--color-muted)]">
              {me.user.email}
            </div>
          </div>
          <button
            type="button"
            role="menuitem"
            onClick={() => logout.mutate()}
            disabled={logout.isPending}
            className="flex w-full items-center gap-2 px-3 py-2 text-left text-[12px] text-[var(--color-rose)] hover:bg-[var(--color-rose-soft)] disabled:opacity-50"
          >
            <LogOut className="h-3.5 w-3.5" />
            Sign out
          </button>
        </div>
      ) : null}
    </div>
  );
}
