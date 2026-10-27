# 仓库布局

- `crates/core`       —— `mpider-core`：store / config / model、MITM 抓凭证 + 接力、回放采集、RPA、巡检、限流、上报、通知、编排（核心逻辑，不依赖 Tauri）
- `crates/article-md` —— 文章 HTML → Markdown 纯解析库（移植自 Python `wechat-article-parser` 0.0.6，MIT；见 `THIRD-PARTY-NOTICES.md`）
- `src-tauri`         —— Tauri v2 外壳：`#[command]` 薄封装 core + 事件推送
- `frontend`          —— WebView 前端（Vite / React 18 / TS / Tailwind / shadcn）
- `docs/`             —— 实现原理
- `tools/`            —— 本地联调脚本（mock 上报接收端）

开发指南见 `CLAUDE.md`，原理见 `docs/how-it-works.md`。
