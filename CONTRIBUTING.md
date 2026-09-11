# 贡献指南

感谢你对 MPider 的兴趣。提交 issue 或 PR 前请先读完本页。

## 环境

- Rust stable（工作区 edition 2021）、Node 20+、pnpm 9+。
- **包管理器只用 pnpm**，仓库锁文件是 `pnpm-lock.yaml`。
- Tauri CLI 走本地依赖：`pnpm tauri …`。

```bash
pnpm install
cargo test -p mpider-core            # 核心库单测（纯逻辑、不联网）
cargo clippy --all-targets       # CI 门禁，零 warning
cargo fmt
pnpm build                       # 前端
pnpm tauri dev                   # 起桌面壳联调
```

## 约定

- 注释、文档、日志、UI 文案一律中文。每个模块顶部有文档注释说明职责，新增模块照做。
- `crates/core/src/store.rs` 是唯一的数据库出入口，其它模块不得直接开 SQLite。
- 解析函数要能脱离网络单测；`crates/article-md` 是纯解析库，不含网络。
- 对固定证书的微信主机必须按 SNI 直通（`capture.rs`），拦错主机会弄断微信。
- rustls / pki_types / crypto provider 统一从 `hudsucker` 再导出，不要在 `mpider-core` 直接依赖 rustls。
- 任何开系统代理的路径都要经 `SystemProxyGuard`（Drop 复位）。
- 凡是发 `getmsg` 的路径都必须走 `collect_list`（自带闸门、计数、留档），不要绕过它另起请求。
- 日志里不得出现微信接口 URL / key / pass_ticket / uin 等参数，只写「在做什么、结果如何」。
- 改了配置字段、上报载荷、数据库 schema，要在同一个 PR 里更新 README 与 `docs/`。

## 仓库里的调试工具（examples）

都要显式 `cargo run` 才会执行，平时只被 `cargo clippy --all-targets` 编译：

| 命令 | 用途 | 是否碰真机 |
|---|---|---|
| `cargo run -p mpider-core --example mitm_smoke` | 起本地 TLS origin，证明按 SNI 直通时裸 TCP 隧道透传原始证书 | 否，全本地 |
| `cargo run -p article-md --example parse_file -- <url> <html文件>` | 用本地 HTML 验证文章解析与错误分类 | 否，不联网 |
| `cargo run -p mpider-core --example detail_probe -- --db <库> --staircase 10000,5000 --csv out.csv` | 间隔阶梯测量公开文章页的限流阈值，结果落 CSV、不写库 | 是，会真实请求 |
| `cargo run -p mpider-core --example manualcap_probe -- --db <库> --ca-dir <目录> --service Wi-Fi` | 脱离 GUI 起「手动抓凭证代理」，环节日志打到终端 | 是，改系统代理 |
| `cargo run -p mpider-core --example rpa_smoke -- check\|mark\|click` | 仅 Windows：RPA 自检 / 命令行标定 / 测试点击 | 是，驱动微信窗口 |

## 不要在 CI 或无人值守环境跑真机链路

`capture` / `runner::run_loop_real` / GUI 里的「开始巡检」会安装根 CA、切换系统代理、驱动微信内置浏览器，
需要有人值守的桌面会话。验证逻辑请用 `cargo test` 与上表中「不碰真机」的两个 example。

## 提交 PR

- 一个 PR 只做一件事；描述里写清动机与验证方式。
- 提交前 `cargo fmt`、`cargo clippy --all-targets`、`cargo test`、`pnpm build` 全过。
- 不要提交任何真实抓包数据、凭证、内网地址、个人账号信息。测试夹具一律用假值。
