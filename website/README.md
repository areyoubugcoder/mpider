# MPider 项目官网

Astro 静态生成网站。首页与博客在构建时输出完整 HTML，JavaScript 仅用于装饰性蛛网。博客入口位于页脚“使用指南”，文章可以公开访问、收录与订阅。

## 开发与检查

在此目录使用 pnpm：

```sh
pnpm install --frozen-lockfile
pnpm dev
pnpm check
pnpm build
pnpm verify
```

预览固定使用 `http://127.0.0.1:3000`。端口被占用时先处理冲突，不自动切换端口。

默认部署地址为 `https://areyoubugcoder.github.io/mpider/`。`SITE_URL` 同时决定资源前缀、canonical、RSS 与站点地图，例如：

```sh
SITE_URL=https://example.com/ pnpm build
SITE_URL=https://example.com/ pnpm verify
```

切换独立域名时，在 GitHub Pages 中配置并验证域名，将仓库 Actions 变量 `SITE_URL` 设为完整 HTTPS 地址（末尾带 `/`），按 GitHub 要求配置 DNS，再手动运行 Pages workflow。确认 DNS 与证书就绪后开启 HTTPS。不要把测试域名的产物发布到正式站点。

## 发布文章

文章存放在 `src/content/blog/`，采用 Markdown 与 frontmatter：

```yaml
---
title: "文章标题"
description: "这篇文章实际解决的问题"
pubDate: 2026-10-29
draft: true
---
```

文件名决定文章地址，发布后尽量保持不变。正文从二级标题开始；页面自动生成主标题、日期、目录、文章元数据与 RSS 条目。

审核完毕后将 `draft` 改为 `false`。草稿和未来日期的文章不会生成公开页面。日期生效后仍需一次构建，可提交文章更新或手动运行 Pages workflow；网站不会自动撰写或发布未经审核的文章。

## 自动部署

`.github/workflows/pages.yml` 在网站 PR 中运行类型检查、构建和产物验证；相关变更合并到 `main` 后自动发布到 GitHub Pages，也可手动触发。PR 不会部署到正式网站。

## Cloudflare Workers 发布

同一份 Astro 源码也支持 Workers Static Assets，独立域名为 `https://mpider.followto.us/`，无需服务端适配器。

在 `website` 目录运行：

```sh
pnpm install --frozen-lockfile
pnpm exec wrangler login
pnpm deploy:cloudflare --dry-run
pnpm deploy:cloudflare
```

Wrangler 会先执行 `build:cloudflare`，固定使用上述域名运行类型检查、构建和产物验证，任何一步失败都会终止发布。也可单独运行 `pnpm build:cloudflare` 检查产物。每次部署都会重新生成 `dist`，不会复用 GitHub Pages 的子路径产物。

`wrangler.jsonc` 管理 Worker 名称、自定义域名与静态文件路由；目录页统一使用末尾 `/`，不存在的地址返回自定义 404 页和 HTTP 404。Cloudflare 会为自定义域名管理 DNS 与 HTTPS 证书。部署账户须拥有 `followto.us` 的管理权限；多账户环境可通过 `CLOUDFLARE_ACCOUNT_ID` 选择部署账户，凭证不写入仓库。

本地发布使用 Wrangler 登录凭证。`.github/workflows/cloudflare.yml` 在网站 PR 中执行发布预检查；相关变更合并到 `main` 后自动发布到 Workers，也支持在 `main` 上手动触发。PR 和其他分支不会发布到正式域名。GitHub Pages 与 Workers 使用各自的工作流构建和发布。

自动发布前，在 GitHub 仓库的 **Settings → Secrets and variables → Actions** 中配置：

- Secret `CLOUDFLARE_API_TOKEN`：专用于 CI 的 Cloudflare API token，允许发布 `mpider-website` 并管理 `followto.us` 的 Workers 路由。权限与作用范围参考 [Cloudflare Workers 权限说明](https://developers.cloudflare.com/workers/authorization/)。
- Variable `CLOUDFLARE_ACCOUNT_ID`：部署账户 ID。

凭证缺失时，正式发布任务会明确失败；PR 的构建校验无需凭证。不要复制本地 OAuth 登录文件，也不要把 token 写入源码或构建产物。

FAQ 存放在 `src/config/faqs.json`，第一条为“为什么开源？”，默认展开。可见答案和 FAQ 结构化数据使用同一份内容。
