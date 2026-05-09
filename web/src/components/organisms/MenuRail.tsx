import { Bot, House } from "lucide-react";
import { cn } from "../../lib/utils";

type MenuItem = {
  id: string;
  label: string;
  icon: typeof House;
  active?: boolean;
};

const items: MenuItem[] = [
  { id: "home", label: "Home", icon: House, active: true },
  { id: "agent", label: "Agent", icon: Bot },
];

export function MenuRail() {
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
        return (
          <button
            key={item.id}
            aria-current={item.active ? "page" : undefined}
            className={cn(
              "flex w-full flex-col items-center gap-1 px-2 py-1 transition-colors",
              item.active
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
    </aside>
  );
}
