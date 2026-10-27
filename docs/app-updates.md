# GitHub 版本更新

## 使用

目前只发布 Windows x64，暂不发布 macOS；其他平台显示当前通道不支持更新。在「系统设置 → 版本更新」检查 GitHub Releases 的最新正式版本。正式构建启动时检查一次；检查不自动下载、安装。发现新版后底栏显示入口。

点击「预下载安装包」显示下载进度，期间可以继续使用应用。下载完成并通过签名校验后保存到应用本地数据目录的 `updates/`，切页或正常退出不会丢失。下次启动从缓存恢复，无需联网即可安装。

点击「安装并重启」，确认后替换应用。应用会退出，正在进行的请求会中断，建议先暂停任务。公众号、文章、配置及已持久化进度保留；原有重启恢复规则继续生效。Windows 启动原生安装器，可能需要系统授权。开发模式禁止执行安装。便携版请通过 Release 安装包迁移到正式安装版本。

清除更新缓存后可重新检查或下载；已有预下载包时保留该版本，不会被后续检查覆盖。失败会显示原因并允许重试，下载不完整或签名不匹配不能进入可安装状态。

## 版本与接口

- 当前版本取 Tauri 项目配置（当前为 `0.2.0`），`package.json`、根 `Cargo.toml` 的工作区版本、`src-tauri/tauri.conf.json` 必须一致。发布标签为 `v<当前项目版本>`，不根据历史 GitHub Release 自动推算或递增。
- 更新地址固定为 `https://github.com/areyoubugcoder/mpider/releases/latest/download/latest.json`，公开仓库无需在客户端放 GitHub Token。仅更高的稳定语义版本有效，同版、旧版、预发布版均不安装。草稿未公开时不会被客户端下载。
- 静态清单包含 `version`、`pub_date`、`notes` 和 `platforms`；目前仅含 `windows-x86_64`。平台项含 `url` 和 `.sig` 文件的完整内容 `signature`。
- Tauri updater 负责 HTTPS 请求、平台选择与签名验证，版本清单和安装包通过 GitHub Release 附件分发，不增加业务 HTTP 服务或数据库表。
- 更新签名强制绑定版本（`requireSignedVersion`）。下载时、缓存恢复时、安装时均校验签名，防止缓存修改、旧包冒充新版。使用 Tauri CLI 2.12.4 或之后的兼容 v2 版本生成版本绑定签名。
- `update_status` 返回当前版本、平台支持及配置状态、阶段、新版版本与说明、已下载/总字节数、错误及发布页链接。`update_check`、`update_download`、`update_clear` 返回相同快照；`update_install` 仅允许已下载状态，成功后退出/重启。阶段为 `idle/checking/available/downloading/ready/installing/up_to_date/error`。
- 后端操作互斥，轮询不持有操作锁；用户切换页面不会终止下载。检查超时 30 秒，安装包下载超时 30 分钟；不实现断点续传，失败后重新下载。

## 一次性签名配置

更新签名与 Windows 开发者代码签名相互独立，更新验签不代表通过操作系统的开发者验证。

在安全位置生成并备份正式签名密钥（不要放进仓库）：

```sh
pnpm tauri signer generate -w ~/.tauri/mpider-updater.key
```

在 GitHub 仓库 Settings → Secrets and variables → Actions 中配置：

| 类型 | 名称 | 内容 |
| --- | --- | --- |
| Variable | `MPIDER_UPDATER_PUBLIC_KEY` | `.key.pub` 文件的完整内容 |
| Secret | `TAURI_SIGNING_PRIVATE_KEY` | 私钥文件完整内容 |
| Secret | `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | 生成密钥时设置的密码；无密码时可留空 |

所有后续版本必须沿用同一私钥。丢失私钥后已安装客户端无法验证新密钥签的更新。仓库里的 `src-tauri/fixtures/updater/` 只是测试公钥、签名和假数据，生成它的一次性私钥已销毁，不能用于正式发布。

默认本地构建没有公钥，显示未启用应用内更新，普通打包不要求签名密钥。需要本地正式签名构建时，在环境中配置上述三个值，执行 `node tools/release-updater.mjs prepare` 后 `pnpm tauri build`。`prepare` 会在当前构建工作区的 Tauri 配置中写入公钥并启用更新产物；私钥只从环境读取，不写入配置。不要把本地修改误提交。

## GitHub 发布

1. 核对发布提交与标签；本次项目版本为 `0.2.0`，标签应为 `v0.2.0`。保留历史 Release 与标签，不覆盖仍在服务用户的版本。
2. 按项目主线/开源发布流程准备发布提交。需要升级版本时，同时修改三个版本文件及锁文件；本功能不会自行修改项目版本。
3. 将匹配当前项目版本的 `v*` 标签推送到 GitHub。工作流先核对版本与签名配置，再为 Windows x64 构建；缺少签名配置时标签发布立即失败。
4. Windows 构建成功后整理附件并生成 `latest.json`，创建 Release **预发布草稿**；保留完整便携 ZIP、自检与摘要，同时提供携带相同 Python 运行环境和完整许可证的 NSIS/MSI。Windows 更新默认使用 NSIS `-setup.exe`；MSI 仍提供手动安装。工作流不构建或上传 macOS 产物。
5. 检查产物、签名、Windows 清单并补充发布说明，再公开草稿。验收通过后发布为正式版本（取消预发布标记），客户端才会通过 latest 地址检测到该版本；保持预发布标记时不会被稳定更新通道选中。缺包、缺签名或缺平台时不生成可发布清单。

手动触发工作流仍只构建与留存 Actions artifacts，不创建 Release；未配签名时可验证普通安装包链路。CI 不创建或更新签名密钥。

## 本地验证

```sh
pnpm exec tsc --noEmit -p frontend/tsconfig.json
pnpm build
node --test tools/test_release_updater.mjs
cargo test -p mpider-tauri updater::tests --lib
```

Windows 本地运行 Rust 测试前，在仓库根目录配置测试 runner（需已有 Windows SDK）：

```powershell
$runner = (Resolve-Path tools/run-windows-test.ps1).Path.Replace('\', '/')
$runnerConfig = 'target.x86_64-pc-windows-msvc.runner=["powershell","-NoProfile","-ExecutionPolicy","Bypass","-File",' + (ConvertTo-Json $runner -Compress) + ']'
cargo --config $runnerConfig test --locked -p mpider-core -p article-md -p mpider-tauri --lib
```

runner 仅在测试 EXE 中嵌入 Common Controls v6 清单，保留测试的参数与退出码；不改正式应用或系统组件。Tauri 当前对库测试清单的限制见[上游说明](https://github.com/tauri-apps/tauri/pull/16320)。CI 和 Windows 发行回归已配置相同 runner。

HTTP 集成验证使用 Postman 本地模拟（没有登录 Postman 时跳过第三方拥有者 mock 搜索；模拟依据 Tauri 官方静态清单契约编写）。它运行真实 updater HTTP 客户端，下载带有效测试签名的假包，验证缓存离线恢复、损坏包拒绝、404/503/非 JSON 处理与重试。模拟不安装测试包，也不改真实采集数据。

```sh
pnpm dlx postman-cli@1.77.0 simulation run postman/simulations/updater-download.sim.yaml --port 4918 --output ndjson
# 另一个终端中；如模拟启动在其他端口，请使用 listening 事件里的实际地址。
MPIDER_UPDATER_TEST_ENDPOINT=http://127.0.0.1:4918/github cargo test -p mpider-tauri network_simulation --lib -- --ignored
```

测试只能证明本地逻辑和模拟下载链路；正式发布前仍需用两个真实签名版本验证 Windows 安装、系统权限和重启。不会在开发期间自动替换用户正在使用的应用。
