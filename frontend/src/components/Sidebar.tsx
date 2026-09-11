import {
  CircleHelp,
  Gauge,
  LayoutDashboard,
  List,
  ListChecks,
  Newspaper,
  ScrollText,
  Settings2,
  UserRound,
  type LucideIcon,
} from "lucide-react";
import { cn } from "@/lib/utils";
import type { View } from "@/views/types";
import appIcon from "@/assets/app-icon.png";

interface NavItem {
  key: View;
  label: string;
  icon: LucideIcon;
}

/** 分组导航（对齐 shadcn sidebar-07 block 的 SidebarGroup 结构）。 */
const GROUPS: { label: string; items: NavItem[] }[] = [
  {
    label: "概览",
    items: [
      { key: "dashboard", label: "控制面板", icon: LayoutDashboard },
      { key: "jobs", label: "任务列表", icon: ListChecks },
      { key: "ratelimit", label: "限流分析", icon: Gauge },
    ],
  },
  {
    label: "采集",
    items: [
      { key: "accounts", label: "公众号列表", icon: List },
      { key: "articles", label: "公众号文章", icon: Newspaper },
    ],
  },
  {
    label: "系统",
    items: [
      { key: "wxaccounts", label: "微信号管理", icon: UserRound },
      { key: "settings", label: "系统设置", icon: Settings2 },
      { key: "logs", label: "日志", icon: ScrollText },
      { key: "faq", label: "常见问题", icon: CircleHelp },
    ],
  },
];

export function Sidebar({
  view,
  onNav,
}: {
  view: View;
  onNav: (v: View) => void;
}) {
  return (
    <aside className="flex w-60 shrink-0 flex-col overflow-y-auto border-r border-sidebar-border bg-sidebar text-sidebar-foreground">
      {/* 品牌头（SidebarHeader） */}
      <div className="flex items-center gap-2.5 px-4 pb-2 pt-4">
        <img src={appIcon} alt="" aria-hidden className="size-9 shrink-0 select-none" draggable={false} />
        <div className="leading-tight">
          <div className="text-sm font-semibold text-foreground">MPider</div>
          <div className="text-[11px] text-muted-foreground">公众号采集控制台</div>
        </div>
      </div>

      {/* 分组导航（SidebarContent / SidebarGroup / SidebarMenu） */}
      <nav className="flex flex-1 flex-col gap-1 px-3 py-2">
        {GROUPS.map((g) => (
          <div key={g.label} className="mb-1.5">
            <div className="px-2 pb-1 pt-2 text-[11px] font-medium text-sidebar-foreground/60">
              {g.label}
            </div>
            <div className="flex flex-col gap-0.5">
              {g.items.map(({ key, label, icon: Icon }) => {
                const active = view === key;
                return (
                  <button
                    key={key}
                    type="button"
                    onClick={() => onNav(key)}
                    className={cn(
                      // 选中项：中性浅灰圆角块 + 墨色字；黄只留给主 CTA，不进导航
                      "flex items-center gap-2.5 rounded-lg px-2.5 py-2 text-left text-sm transition-colors",
                      "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sidebar-ring focus-visible:ring-offset-1 focus-visible:ring-offset-sidebar",
                      active
                        ? "bg-sidebar-accent font-medium text-sidebar-accent-foreground"
                        : "text-sidebar-foreground hover:bg-sidebar-accent/50 hover:text-sidebar-accent-foreground",
                    )}
                  >
                    <Icon
                      className={cn("size-4 shrink-0", !active && "opacity-80")}
                    />
                    <span>{label}</span>
                  </button>
                );
              })}
            </div>
          </div>
        ))}
      </nav>

      {/* SidebarFooter */}
      <div className="px-4 py-3 text-[11px] text-sidebar-foreground/60">
        Rust / Tauri 版 · 开发中
      </div>
    </aside>
  );
}
