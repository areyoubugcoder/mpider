# MPider 开发说明

MPider 是 Windows x64 微信公众号采集与离线阅读工作台。桌面壳为 Tauri v2，前端为 React/TypeScript，Rust 管理 SQLite、任务、请求策略和归档，Python/Frida 提供原生会话观察与协议工作进程。

## 当前工作流

1. 用户开启公众号监测，在已登录的微信中手动打开目标公众号主页。
2. 监测登记公众号、会话和首页游标；文章入库由用户单独发起首页同步或历史采集。
3. 历史采集按追加篇数或截止日期运行，每页原子保存文章与检查点，支持暂停和续采。
4. 会话需要更新时，提示用户手动打开目标主页，完整会话到齐后继续原任务。
5. 正文与图片进入独立持久化下载队列，下载后离线阅读，同时导出 Markdown 与 HTML。

当前适配微信 4.1.15.56 x64 的精确指纹；使用方式与边界以 README、Windows 独立运行说明和 FAQ 为准。

## 目录与职责

- `frontend/`：工作台视图、任务与状态组件、Tauri IPC 契约。
- `src-tauri/`：桌面窗口、托盘、IPC 与签名更新逻辑。
- `crates/core/src/workbench/`：调度、恢复、归档、导出与游标实验。
- `crates/core/src/store/` 及 `store.rs`：数据库读写与事务。
- `crates/article-md/`：正文解析与 Markdown 转换。
- `native/`：原生观察、会话保护、协议工作进程与离线测试。
- `tools/`：验证、许可收集与 Windows 打包。

数据库禁止新增外键声明。普通 ID 关联由业务校验、原子更新和并发控制维护。前端不接收会话密钥或原始游标；会话与原始证据由当前 Windows 用户 DPAPI 保护。

## 开发与验证

修改使用独立 Git worktree，安装依赖使用 `pnpm install --frozen-lockfile`。提交前按改动范围运行类型、构建、Node/Python 离线回归、Rust 格式、Clippy 与测试，命令见 CONTRIBUTING.md。

CI 检查前端与 Windows Rust，发行仅构建 Windows x64。离线测试不附加用户微信，不读取真实会话，不安装根证书或修改系统代理。需要真机采集时，先明确授权、数据目录和停止条件。

## 维护者公开发布

项目版本取 `package.json`、根 `Cargo.toml` 与 `src-tauri/tauri.conf.json`，三处保持一致。匹配的 `v*` 标签触发 Windows 完整构建、许可与离线回归，生成便携包、NSIS/MSI、版本绑定签名、更新清单和摘要。检查草稿附件及验证范围后公开，正式签名密钥保持在仓库外。

不把数据库、会话、日志、私有材料或本机环境文件加入公开提交。详细发布操作见 `docs/app-updates.md` 和 `docs/windows-standalone.md`。
