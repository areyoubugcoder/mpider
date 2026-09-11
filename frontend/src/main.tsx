import React from "react";
import ReactDOM from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { App } from "@/App";
import { SeedPickOverlay } from "@/components/SeedPickOverlay";
// 字体随产物打包（桌面应用离线可用）：Inter 可变字体 + JetBrains Mono 可变字体
import "@fontsource-variable/inter";
import "@fontsource-variable/jetbrains-mono";
import "@/index.css";

// react-query：视图层的数据获取 / 变更统一走它（loading、错误、失效重取）。
// 应用级长驻状态（轮询、事件流、运行配置）仍在 store.tsx。
const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // Tauri 本地 IPC：失败基本不是瞬时网络问题，不自动重试；窗口聚焦不狂刷。
      retry: false,
      refetchOnWindowFocus: false,
    },
  },
});

const rootEl = document.getElementById("root");
if (!rootEl) throw new Error("missing #root");

// 同一份前端产物承载两个窗口：主窗口（默认）与种子框选层（`?view=pick`，由后端 rpa_pick_begin
// 建的全屏透明置顶窗口加载）。框选层要让底下的桌面透出来，故把 html/body 背景置透明。
const view = new URLSearchParams(window.location.search).get("view");
if (view === "pick") {
  document.documentElement.style.background = "transparent";
  document.body.style.background = "transparent";
}

ReactDOM.createRoot(rootEl).render(
  <React.StrictMode>
    {view === "pick" ? (
      <SeedPickOverlay />
    ) : (
      <QueryClientProvider client={queryClient}>
        <App />
      </QueryClientProvider>
    )}
  </React.StrictMode>,
);
