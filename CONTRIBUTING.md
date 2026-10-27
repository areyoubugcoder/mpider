# 贡献指南

提交问题先查看 [README](README.md)、[更新记录](CHANGELOG.md) 和[已知验证边界](docs/windows-workbench-validation.md)。功能问题使用 [Issue 模板](https://github.com/areyoubugcoder/mpider/issues/new/choose)；漏洞请走[安全政策](SECURITY.md)中的私密渠道。

## 开发环境

- Rust stable、Node.js 24+、pnpm 9+；只使用 pnpm，保留 `pnpm-lock.yaml`。
- 原生桥离线测试使用 Python 3.12，依赖固定在 `native/requirements.txt`。
- Windows 桌面构建还需要 MSVC C++ Build Tools、Windows SDK 与 WebView2，见[独立运行](docs/windows-standalone.md)。
- 当前原生采集只验证微信 4.1.15.56 x64。开发构建或跨平台测试通过不代表该平台可采集。

新增功能与修复均在独立 Git worktree 开发验证，不在主检出目录直接改动。示例：

```sh
git worktree add ../mpider-my-change -b feat/my-change
cd ../mpider-my-change
pnpm install --frozen-lockfile
pnpm tauri dev
```

Tauri CLI 使用本地依赖 `pnpm tauri …`。前端开发服务配置见 `vite.config.ts`。开始采集之前需另行确认真机、登录用户和操作范围；普通开发不要求运行微信或触发真实请求。

## 提交前验证

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p mpider-core -p article-md
pnpm exec tsc --noEmit -p frontend/tsconfig.json
pnpm build
python -m pip install -r native/requirements.txt
python -m unittest discover -s native -p 'test_*.py'
pnpm test
```

按改动范围补充有意义的回归。说明实际运行的平台、命令与结果，区分离线测试、桌面启动、真实请求和长期观察；没有验证的部分明确写出。

## 代码与数据约定

- 文档、注释、日志和 UI 文案使用中文；模块顶部说明职责。
- Rust 核心保持不依赖 Tauri，界面 IPC 只做薄封装；解析逻辑应能脱网测试。
- `store` 是唯一数据库入口。**禁止添加 FOREIGN KEY（包括 ORM 外键声明）**；普通 ID 关联由业务事务、原子更新与并发控制保证。
- 工作台列表请求共用锁、策略闸门和持久化账本；正文、图片、重试和实验请求也需记账，不旁路预算与冷却。
- 普通 IPC、日志、截图及导出不得包含会话密钥、原始游标或完整协议载荷；真实数据、抓包、个人账号和内网地址不得提交。测试夹具使用虚构值。
- 微信 profile 必须核对模块摘要、函数指纹与桥协议，不能只替换版本号或复制旧 RVA。
- 变更配置、接口、数据库 schema 或行为时，同步更新文档。界面样式遵循[视觉基线](docs/ui-baseline.md)。

历史代理链路另需保留 TLS/SNI 直通、`SystemProxyGuard` 复位及 `collect_list` 的计账规则；不要把旧 getmsg 错误码语义套到当前原生链路。架构见 [CLAUDE.md](CLAUDE.md) 与[工作台设计](docs/windows-workbench.md)。

## 真机验证

CI 和无人值守测试不得附加用户微信、安装根证书、切换系统代理、模拟点击或发送真实采集请求。使用虚构协议夹具和本机 HTTP 服务验证逻辑。需要真实验证时，先明确授权、数据目录和停止条件，保护现有数据库与会话；不要用已有真实数据充当可公开的测试夹具。

## Pull Request

说明触发场景、改动后的行为、验证证据和未覆盖范围。保持改动聚焦，遵循 PR 模板。提交前检查差异与新增文件，确认没有日志、数据库、私有材料或本地环境配置。不要自动附加完整诊断导出。

维护者确认合并后再合并；本地 worktree 与分支的保留或清理按协作约定处理，避免影响他人的检出目录。
