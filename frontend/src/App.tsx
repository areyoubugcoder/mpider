import { useState } from "react";
import { AppProvider } from "@/store";
import { Sidebar } from "@/components/Sidebar";
import { StatusBar } from "@/components/StatusBar";
import { AlertBar } from "@/components/AlertBar";
import { Toaster } from "@/components/ui/sonner";
import { Dashboard } from "@/views/Dashboard";
import { RateLimit } from "@/views/RateLimit";
import { Jobs } from "@/views/Jobs";
import { Accounts } from "@/views/Accounts";
import { Articles } from "@/views/Articles";
import { Settings } from "@/views/Settings";
import { WxAccounts } from "@/views/WxAccounts";
import { Logs } from "@/views/Logs";
import { Faq } from "@/views/Faq";
import type { View } from "@/views/types";

/**
 * 应用外壳（布局根治）：
 * - 根 `h-screen overflow-hidden`，纵向 flex-col；
 * - 中间行（侧栏 + 主内容）`flex-1 min-h-0`，把可用高度框死；
 * - 侧栏固定宽度、底部状态栏固定高度、都 `shrink-0` 不动；
 * - 只有右侧主内容 `overflow-y-auto min-h-0` 滚动 —— 内容再高也不撑开整页。
 */
function Shell() {
  const [view, setView] = useState<View>("dashboard");

  return (
    <div className="flex h-screen flex-col overflow-hidden bg-background text-foreground">
      <div className="flex min-h-0 flex-1">
        <Sidebar view={view} onNav={setView} />
        <main className="min-h-0 min-w-0 flex-1 overflow-y-auto overflow-x-hidden bg-background px-4 py-4">
          {/* 全局提醒横幅：预算达上限 / 微信号受限 / 历史抓取暂停，任何页面可见 */}
          <AlertBar onNav={setView} />
          {view === "dashboard" && <Dashboard onNav={setView} />}
          {view === "jobs" && <Jobs />}
          {view === "ratelimit" && <RateLimit onNav={setView} />}
          {view === "accounts" && <Accounts onNav={setView} />}
          {view === "articles" && <Articles />}
          {view === "wxaccounts" && <WxAccounts onNav={setView} />}
          {view === "settings" && <Settings />}
          {view === "logs" && <Logs />}
          {view === "faq" && <Faq onNav={setView} />}
        </main>
      </div>
      <StatusBar onNav={setView} />
      {/* 异步操作结果的统一回显（成功 / 失败 / 提示） */}
      <Toaster />
    </div>
  );
}

export function App() {
  return (
    <AppProvider>
      <Shell />
    </AppProvider>
  );
}
