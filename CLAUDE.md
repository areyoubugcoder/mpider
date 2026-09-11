# CLAUDE.md

本文给 Claude Code（claude.ai/code）在本仓库工作时看，中文，与代码 / 文档一致。

## 这个仓库是什么

采集微信公众号文章（列表 + 正文）的桌面工具，Rust 核心 + Tauri v2 壳。原理见 `docs/how-it-works.md`，用户视角见 `README.md`。

- `crates/core`（`mpider-core`）：核心逻辑。`store`（唯一 DB 出入口）/ `capture`（MITM + 按 SNI 直通）/ `proxy_addon`（抠凭证）/
  `seedserver`（本机种子入口）/ `relay`（接力脚本与队列）/ `rpa`（点种子、框选标定）/ `orchestrator`（编排 + 看门狗）/
  `collector` + `wechat`（`getmsg` 回放）/ `credrefresh`（集中续期）/ `detail`（正文补采）/ `sweep`（定时巡检）/
  `runstate`（运行互斥、请求闸门、预算、退避）/ `ratelimit`（留档统计）/ `report`（HTTP 上报）/ `notify`（飞书）/
  `addlink`（批量添加：短链匿名直连解析公众号建号）/
  `applog` + `phases`（环节日志、阶段耗时）/ `manualcap`（调试用手动抓凭证）。不依赖 Tauri，可独立 `cargo test`。
- `crates/article-md`：文章 HTML → Markdown 纯解析库，不含网络。
- `src-tauri`：`#[command]` 薄封装 `mpider-core` + 事件推送。
- `frontend`：Vite + React 18 + TS + Tailwind + shadcn/Radix。运行配置存前端 localStorage，通过 `RealRunConfig` 传给后端。视觉基线（沉静 · 工具感：shadcn 细淡线条不动，暖白表面 + 品牌黄只做 CTA / 激活态 + 语义状态色，Inter / JetBrains Mono）见 `docs/ui-baseline.md`，改样式前先读。

## 常用命令

```bash
cargo test -p mpider-core            # 核心库单测（无网络）
cargo clippy --all-targets       # CI 门禁，零 warning
cargo fmt
cargo run -p mpider-core --example mitm_smoke   # 本地 TLS origin 冒烟，证明按 SNI 直通
cargo run -p mpider-core --example manualcap_probe -- --db <app_data_dir>/mpider.db --ca-dir <app_data_dir> --service Wi-Fi --minutes 10
                                 # 脱离 GUI 起「手动抓凭证代理」验证微信内置浏览器走代理 / 信任证书（真改系统代理，需有人值守）
pnpm install && pnpm tauri dev   # 桌面壳联调（自动起 Vite:5173）
pnpm build                       # 前端产物到 frontend/dist
pnpm exec tsc --noEmit -p frontend/tsconfig.json   # 前端类型检查
pnpm tauri build
```

包管理器只用 `pnpm`。Tauri CLI 走本地依赖 `pnpm tauri …`。

## 硬约束

- **注释、文档、日志、UI 文案一律中文**；每个模块顶部有文档注释说明职责。
- **`store` 是唯一 DB 出入口**，其它模块不得直接开 SQLite；解析函数要能脱网单测。
- **TLS 直通不可省**：固定证书的微信主机必须按 SNI 直通（`capture.rs` 的 `should_intercept_tls`）。拦错主机会弄断微信。
- **rustls / rcgen 版本耦合**：rustls、pki_types、crypto provider 统一从 `hudsucker` 再导出，不要在 `mpider-core` 直接依赖 rustls；`rcgen` 版本与 hudsucker 内部一致。
- **系统代理用 RAII 守卫** `SystemProxyGuard`，任何开系统代理的路径都靠它兜底复位；守卫构造即登记到进程级表，Tauri 退出事件调 `sysproxy::force_restore_all` 复位（直接退出时守卫来不及析构），启动时 `sysproxy_reset_stale` 清理上次残留（回环地址 + 端口无人听）。
- **凡是发 `getmsg` 的路径都必须走 `collect_list`**（自带闸门 `runstate::list_gate_wait`、计数、`list_call_log` 留档），不要绕过它另起请求。
- **日志不得出现接口 URL / key / pass_ticket / uin**（`applog::redact` 兜底），文案只写「在做什么、结果如何」；账号用 `store.account_label(biz)` 显示。
- **请求 / 响应关联存在每请求的 handler clone 字段里**，不要改回按 `client_addr` 共享 map（hudsucker 默认 HTTP/2，会串）。
- 新增执行单元类型要补 `run_log`；新增环节在入口 `phases::mark`；新增预警点一行 `notify::notify`，只报异常不报状态。
- 改了配置字段、上报载荷、schema，同一次改动里更新 `README.md` / `docs/`。
- 库内错误统一 `anyhow::Result`。

## 行为要点（改动前先知道）

- 主循环 `orchestrator::run_forever` 是唯一流水线，两种模式互斥：巡检模式（GUI「开始巡检」，`sweep` 为 `Some`）和历史模式（开始 / 继续历史抓取时由 GUI 拉起，`sweep` 为 `None`，没有活动历史任务自行退出）。顺序：退避中不动 → 预算检查 → 历史抓取到点的下一页（`history::due` → `tick` 一页）→ 巡检批次 → 休眠。每个单元持 `UnitGuard`，批次之间释放。没有手动任务：「批量添加」走 `addlink`，短链匿名直连解析公众号名称 / biz 当场建号并记 `accounts.seed_url`；库里没文章时 `latest_article_urls` 回退到种子链接做接力取样。
- 巡检 ↔ 历史互斥在两端硬拦：`history::start` / `resume` 见 `runstate::sweep_on()`（含「停止巡检」后批次收尾中）即拒绝；`sweep::start_check` / `sweep_retry_account` 见 `history_active()`（running 或 paused）即拒绝。巡检开关不跨重启，应用启动时 `history::pause_on_restart` 把在跑的历史任务转 `paused(restart)`。
- 历史抓取（`history.rs`，`history_jobs` 表）**一次只有一个号、一页一个单元**，页间隔 `history_gap_seconds`，预算叠加 `history_budget_reserve` 为巡检留额度；预算 / 封号 / 退避 / 凭证不可用都转 `paused(原因)`，额度腾出自动恢复；上报只在结束 / 暂停时一次（`job_kind: "history"`）。全局提醒走 `runstate::alert_push`。
- 微信号由抓到的凭证按 `uin` 自动登记到 `wx_accounts`（没有手动新增），单激活；抓到的 uin 不属于激活号即中止本批。
- 长链能解析出 `__biz` 且凭证新鲜（未过 TTL、未实测失效、uin 属于当前激活微信号）时直接采，不开微信；只有短链和凭证不可用的号才接力。
- 翻页到底不信 `can_msg_continue`，看 `next_offset` 是否前进 / 下一页是否空；`ret=-3` 记 `invalidated_at` 并带回 offset，本轮采完后集中续期再续采。
- `ret=-6` 是微信号级封禁（按 uin 累计约 200 次 / 24h），不重试、不换 key，按微信号记退避 6h 起翻倍；429 / 5xx / 接口回网页是 IP 级，整机退避 5min 起翻倍。退避状态持久化，重启不清零。
- 命中验证页先抓对照链接，对照也验证页才判限流；无效 `sn` 的坏链稳定返回验证页，是假阳性。
- **静默空页 / 列表接口现状**（`wechat::is_silent_empty_list`）：`ret=0`/`errmsg=ok` 但无列表容器且 `msg_count=0`。**2026-09-10 起这是常态，列表采集实际不可用**：当天中午还正常（149 篇），下午起三个微信号（含全新号首次请求）× 两台机器 × 7 个公众号响应一致，排除账号限制，指向服务端调整。只在首采首页判定，记 `suspect_blocked` + `list_call_log.outcome="empty_suspect"` + warn + 预警，**不要**当成「没有更多历史」。结论维护在 `Faq.tsx` 第一条。
- 种子页请求不经代理（Chromium 对回环地址绕过系统代理），入口逻辑在 `seedserver` 里；`capture.rs::is_bootstrap` 只是 seed_host 配成局域网 IP 时的兜底。
- 巡检单号「重新巡检」：凭证新鲜直采，不可用就立即单链接接力；有执行单元在跑或有历史任务时按钮禁用。
- **平台差异**：只有 Windows 能自动点种子（`rpa::WindowsWeChatController`）。mac / 关闭 RPA 是**人工模式**（`WeChatController::manual()`）：编排与续期把等待抬到 `rpa::MANUAL_WAIT_SECS` 下限并推 `manual_open` 全局提醒，用户在微信里打开任意一篇文章即接力；本批末条后接力脚本把内置浏览器送回种子页当**待命页**（`seedserver` 长轮询 `/wait`，`CaptureConfig.resident_home`），`seedserver::resident_alive()` 为真时后续批次不提醒、不抬等待，由待命页自动接力（仅 `set_resident_mode` 打开时，Windows 自动点击不开）。前端按 `system_status.rpa_auto` / `platform` **整体隐藏**不适用的 UI（种子链接、框选、自检、测试点击、seed_* / relay 重点相关字段、mac 之外的「网络服务名」），不显示「不支持」文案。
- 上报每号采完当场 POST，未启用返回 `skipped` 不算失败；没正常采到列表的号不报。

## 不要真机跑抓取

`capture` / `runner::run_loop_real` / GUI「开始巡检」「抓取历史」「重新巡检」会安装根 CA、切系统代理、驱动微信内置浏览器，需要有人值守的桌面会话，agent 不要自动执行。「批量添加」只是匿名 GET 公开文章页，可以跑。验证逻辑用 `cargo test`、`mitm_smoke` example，或 `tools/mock_report_server.py` 联调上报。`key` 约 30 分钟过期。

## 仓库约定

`main` 单分支。`target/`、`node_modules/`、`frontend/dist/`、`data/`、`*.log`、`trace/` 已 gitignore。不要提交任何真实抓包、凭证、内网地址、个人账号；测试夹具一律假值。
