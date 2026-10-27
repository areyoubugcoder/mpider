import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "tailwindcss";
import autoprefixer from "autoprefixer";
import path from "node:path";
import { fileURLToPath } from "node:url";

// 布局见 RUST.md：前端源码在 frontend/，src-tauri/ 与之平级。
// Node 工程根放仓库根：单一 pnpm 工程 + Tauri CLI 从仓库根即可找到 ./src-tauri。
// `root: "frontend"` 相对 pnpm 脚本的 cwd（= 仓库根）解析；outDir 相对 root → frontend/dist。
//
// 前端已迁移到 React 18 + TS + Tailwind + shadcn/ui：
// - @vitejs/plugin-react 提供 JSX / Fast Refresh；
// - PostCSS 内联声明（tailwindcss + autoprefixer），显式指向仓库根 tailwind.config.cjs，
//   避免 root=frontend 时 PostCSS 配置发现的歧义；
// - `@` 别名指向 frontend/src（与 tsconfig paths 对齐）。
const repoRoot = path.dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  root: "frontend",
  plugins: [react()],
  // Tauri：固定端口、失败即停，且不清屏（方便同屏看 Rust 侧日志）。
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  resolve: {
    alias: {
      "@": path.resolve(repoRoot, "frontend/src"),
    },
  },
  css: {
    postcss: {
      plugins: [
        tailwindcss({ config: path.resolve(repoRoot, "tailwind.config.cjs") }),
        autoprefixer(),
      ],
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "es2021",
  },
});
