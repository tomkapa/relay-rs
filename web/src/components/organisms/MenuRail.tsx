import { Bot, House, Plug } from "lucide-react";
import { useLocation, useNavigate } from "react-router-dom";
import { cn } from "../../lib/utils";
import { useT } from "../../i18n";
import { useAuthStore } from "../../stores/authStore";
import { UserMenu } from "./UserMenu";

type MenuItem = {
  id: string;
  label: string;
  icon: typeof House;
  /** Match against the current pathname prefix to highlight as active. */
  match: (pathname: string) => boolean;
  to: string;
};

export function MenuRail() {
  const nav = useNavigate();
  const { pathname } = useLocation();
  const { t } = useT();
  const me = useAuthStore((s) => s.me);

  const items: MenuItem[] = [
    {
      id: "home",
      label: "Home",
      icon: House,
      match: (p) => p === "/" || p.startsWith("/threads") || p.startsWith("/c/"),
      to: "/",
    },
    {
      id: "agent",
      label: "Agent",
      icon: Bot,
      match: (p) => p.startsWith("/agents"),
      to: "/agents",
    },
    {
      id: "connections",
      label: t("menu.connections"),
      icon: Plug,
      match: (p) => p.startsWith("/connections"),
      to: "/connections",
    },
  ];

  return (
    <aside
      className="flex h-full w-[72px] shrink-0 flex-col items-center gap-1.5 bg-[#1E3322] p-2"
      aria-label="Menu rail"
    >
      <div
        className="flex h-9 w-9 items-center justify-center border border-white bg-[var(--color-moss)]"
        aria-hidden
      >
        <span className="font-mono text-[11px] font-bold tracking-tight text-white">
          NX
        </span>
      </div>

      <div className="my-0.5 h-px w-6 bg-white/20" aria-hidden />

      {items.map((item) => {
        const Icon = item.icon;
        const active = item.match(pathname);
        return (
          <button
            key={item.id}
            type="button"
            onClick={() => nav(item.to)}
            aria-current={active ? "page" : undefined}
            className={cn(
              "flex w-full flex-col items-center gap-1 px-2 py-1 transition-colors",
              active
                ? "bg-white/10 text-white"
                : "text-white/80 hover:bg-white/5 hover:text-white",
            )}
          >
            <Icon className="h-5 w-5" strokeWidth={1.75} />
            <span className="font-sans text-[11px] font-medium leading-none">
              {item.label}
            </span>
          </button>
        );
      })}

      {me ? (
        <div className="mt-auto flex w-full justify-center pt-2">
          <UserMenu />
        </div>
      ) : null}
    </aside>
  );
}
