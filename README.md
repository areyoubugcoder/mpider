# MPider · 公众号采集工作台

在自己的 Windows 电脑上采集微信公众号文章，保存正文与图片，离线阅读，并同时导出 **Markdown 和 HTML**。

使用已登录的微信会话，通过当前 Windows 原生列表与会话协议采集。公众号监测、列表采集和正文下载分别控制，文章与任务进度保存在本机。

[下载客户端](https://github.com/areyoubugcoder/mpider/releases/latest) · [全部版本](https://github.com/areyoubugcoder/mpider/releases) · [使用说明](docs/windows-standalone.md) · [常见问题](docs/faq.md) · [反馈问题](https://github.com/areyoubugcoder/mpider/issues/new/choose)

> **v0.2.1 · Windows x64 更新验收版**。当前仅验证微信 **4.1.15.56 x64** 的模块与函数指纹；微信升级后需要重新适配。macOS、Linux 不支持本次原生采集。

## 软件截图

以下截图来自本版本前端，使用虚构公众号与演示文章展示操作，不代表真实账号或实际采集结果。

**公众号：先监测登记，再按公众号同步首页、采集历史、下载正文。**

![公众号监测与采集工作台](docs/screenshots/workbench-sources.png)

**文章库：按公众号筛选，下载后阅读，同时导出 Markdown 和 HTML。**

![文章库与下载、阅读、导出操作](docs/screenshots/workbench-articles.png)

**任务：分别管理首页同步、历史采集与正文下载，暂停后保留进度。**

![历史采集任务与检查点进度](docs/screenshots/workbench-tasks.png)

## 可以做什么

| 功能 | 使用方式 |
| --- | --- |
| 公众号监测 | 开启后在微信手动打开公众号主页，登记公众号与会话；监测本身不入库文章 |
| 同步最新文章 | 每个公众号单独获取首页，去重入库，不改变历史检查点 |
| 采集历史文章 | 按追加篇数或截止日期创建任务，支持暂停和检查点续采 |
| 更新会话后续采 | 按提示手动打开目标主页，完整新会话到齐后继续原任务 |
| 正文与图片下载 | 单篇、勾选或整号加入统一持久化队列，跳过已有正文 |
| 离线阅读与导出 | 同时生成 Markdown、HTML、双格式索引、已保存图片和缺图清单 |
| 请求与任务管理 | 分别配置列表、正文和图片的间隔、并发与预算，查看请求记录和系统日志 |
| 客户端更新 | Windows 安装版检查 GitHub 正式版本，预下载并验签，确认后安装与重启 |

列表默认每页 10 条，可切换 20 / 50 / 100 条。底栏显示监测状态与活动任务进度，点击进度进入对应任务标签。游标实验室默认隐藏，可在系统设置中开启。

暂不采集视频、音频、评论、阅读量和互动数据。不提供云端托管、无人登录的 Windows 服务或自动唤醒。

## 下载与安装

环境要求：**Windows 10/11 x64**、[WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/) 和已登录的 **微信 4.1.15.56 x64**。首次登记公众号与更新会话时，桌面需要解锁且可交互。

在 [GitHub Releases](https://github.com/areyoubugcoder/mpider/releases/latest) 选择：

| 下载文件 | 用途 |
| --- | --- |
| `MPider_0.2.1_x64-setup.exe` | 推荐，NSIS 安装版，支持应用内更新 |
| `MPider_0.2.1_x64_zh-CN.msi` | 中文 MSI 安装版 |
| `MPider-v0.2.1-windows-x64.zip` | 便携版，完整解压后运行 `MPider.exe`，保留旁边的 `runtime/` |
| `SHA256SUMS` | 核对下载文件的 SHA-256 |

三个客户端包均携带 Python **3.12.10**、Frida **17.23.3**、cryptography **50.0.2** 及所需许可，无需另外安装 Python、Node.js、pnpm 或 Rust。微信和 WebView2 需要自行安装。不要只复制便携包内的 EXE。

安装包附带应用更新签名，用于 MPider 验证更新文件；它不等同于 Windows 开发者代码签名，系统仍可能提示发布者未知。可先核对附件摘要：

```powershell
Get-FileHash .\MPider_0.2.1_x64-setup.exe -Algorithm SHA256
```

## 开始采集

1. **登记公众号**：打开 MPider，在「公众号」页开启「公众号监测」，再到微信手动打开目标公众号主页，等待名称与会话登记。此时不会自动采集文章。
2. **获取文章列表**：在对应公众号行点击「同步最新文章」获取首页；需要历史内容时，点击「采集历史文章」，选择追加篇数或截止日期。
3. **处理会话更新**：任务提示需要新会话时，按提示在微信重新打开或刷新该公众号主页。完整会话到齐后会沿原任务继续，程序不会替你点击微信窗口。
4. **下载正文**：在公众号行点击「下载全部正文」，或在文章库下载单篇、勾选文章。下载包括正文、封面与正文图片，已保存正文自动跳过。
5. **阅读与导出**：已下载文章显示「阅读 / 导出」。单篇、批量和整号均同时导出 Markdown 与 HTML；批量或整号导出前，先完成全部目标正文下载。

首页同步、历史采集、正文下载在「任务」的三个标签中分别管理。关闭窗口后应用留在托盘，任务继续运行；从托盘退出才停止程序。重启后未完成任务保留检查点并暂停，由你选择继续。

升级前备份「系统设置」显示的完整数据目录。便携版的数据也存放在应用数据目录，通常不在 EXE 旁；只备份数据库会遗漏正文、图片和受保护会话文件。便携版迁移到安装版及更新操作见[客户端更新说明](docs/app-updates.md)。

## 当前采集方案

```text
开启监测 → 在微信手动打开公众号主页 → 登记公众号、会话与首页游标
                                            ↓
                     手动发起：首页同步 / 历史采集
                                            ↓
                     原生会话协议请求 → 文章列表与检查点入库
                                            ↓
                     单独下载正文与图片 → 离线阅读 → Markdown + HTML
```

Tauri / React 提供界面，Rust 管理 SQLite、调度、请求策略和本地归档。Python / Frida 在本机观察已登录微信的原生列表调用；获取会话后，工作进程构造与验证协议请求，Rust 原子保存文章、游标和任务进度。首页同步和历史采集分别维护进度，会话更新后可沿检查点继续。

**使用当前工作台不需要安装根证书、修改系统代理、标定点击坐标或配置种子链接。** 详细流程见[实现原理](docs/how-it-works.md)、[会话恢复](docs/home-sync-recovery.md)和[协议设计](docs/wechat-session-protocol.md)。

## 请求控制与适配范围

初始列表间隔为 **90–120 秒**，滚动 24 小时预算 **60 次**；正文与图片初始间隔各 **1 秒**，并发各 **4**。这些是本地默认配置，**不是已证明安全的平台配额**，可在「限流配置」自行调整，见[频率与预算说明](docs/request-policy.md)。

微信可能要求验证、使会话失效、限制请求或账号。Frida 附加已有导致微信退出的记录，长期稳定性仍需验证。程序遇到不匹配的模块或函数指纹会停止，不能仅改微信版本号强行适配。出现验证或频率限制时应停止任务并查看日志。

会话有效期与游标可复用时间分别判断，不设统一寿命承诺。已有[旧游标约 33 小时后配合新会话复用的单样本观察](docs/cursor-lifetime-validation-20261023.md)，该样本不能推广到所有公众号。CI 的离线回归与发行包自检不代表真实采集、长期运行或两个正式版本间升级已通过；验证范围见[记录](docs/windows-workbench-validation.md)。

## 数据与隐私

文章、设置、请求账本、日志与任务进度保存在本机，无需注册 MPider 账号，不自动向维护者上传文章库或诊断材料。会话密钥、原始游标与协议证据由当前 Windows 用户的 DPAPI 保护；普通文章导出不读取受保护会话文件。这些文件不能直接跨 Windows 用户使用，正文和导出文件本身不加密。

采集会访问微信服务及文章图片来源。底栏广告在启动及每 30 分钟从 GitHub 读取公开配置；启用应用更新的正式构建启动时检查公开版本清单，不自动下载或安装。上述检查不附带本地账号、Cookie、会话或文章库，GitHub 仍会收到 IP 等常规网络信息。外部产品、原文和反馈链接在点击后通过系统浏览器访问，详见[广告说明](docs/advertisements.md)。

反馈前请检查并脱敏日志；不要上传数据库、`workbench/private/`、会话文件或原始抓包。安全问题见[安全政策](SECURITY.md)。只获取和保存你有权处理的内容，文章版权归原作者及权利人所有。

## 开发与贡献

开发需要 Node.js **24+**、**pnpm**、Rust stable；Windows 构建还需 Visual Studio C++ Build Tools 与 Windows SDK。改动在独立 Git worktree 中进行：

```sh
pnpm install --frozen-lockfile
pnpm exec tsc --noEmit -p frontend/tsconfig.json
pnpm build
pnpm test
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked -p mpider-core -p article-md -p mpider-tauri
pnpm tauri dev
```

Windows 测试 runner、Python 离线测试和完整客户端构建见[Windows 构建说明](docs/windows-standalone.md)。CI 检查前端与 Windows Rust；客户端发行仅面向 Windows x64，标签必须与项目版本一致，完成构建、离线测试、便携包解压自检与签名清单生成后创建 Release 草稿。

[贡献指南](CONTRIBUTING.md) · [工作台设计](docs/windows-workbench.md) · [更新记录](CHANGELOG.md) · [第三方声明](THIRD-PARTY-NOTICES.md)

本项目以 [MIT](LICENSE) 开源。

希望持续订阅公众号更新，可了解 [mp2rss](https://mp2rss.bugcode.dev/)；需要历史文章下载服务，可了解[公众号历史文章下载](https://mphistory.followto.us/)。它们是独立外部服务，点击入口不会附带 MPider 本地会话或文章库。
