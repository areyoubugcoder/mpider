# Windows 独立运行

当前发行目标为 Windows 10/11 x64 原生工作台。首次登记公众号和更新会话需要已登录、解锁且可交互的微信桌面；保存有效会话后的历史翻页、正文和图片任务仍需电脑与应用运行，但不依赖持续点击微信窗口。不提供无人登录的 Windows 服务或自动唤醒。

v1.0.2 包含独立监测、按公众号任务、Markdown/HTML 双格式导出和 GitHub 签名更新。下载时请核对 Release 的源码版本、文件摘要与验证范围；离线 CI 不代替真实采集与安装升级验证。

## 使用发行包

推荐安装 `MPider_1.0.2_x64-setup.exe`，也可选择 MSI；安装版携带与便携包相同的 Python 环境和许可，并支持应用内签名更新。以下步骤介绍便携 ZIP 的使用。

1. 从 [GitHub Releases](https://github.com/areyoubugcoder/mpider/releases) 下载 `MPider-v版本-windows-x64.zip` 和 `SHA256SUMS`，核对来源及摘要：

   ```powershell
   Get-FileHash .\MPider-v1.0.2-windows-x64.zip -Algorithm SHA256
   ```

2. **完整解压**到可写目录，保留 `MPider.exe` 旁的 `runtime`。不要只复制 EXE。发行包附带 Python 3.12.10、Frida 17.23.3、cryptography 50.0.2 及其依赖，无需安装 Node.js、pnpm、Rust 或系统 Python。
3. 确认已安装 [WebView2 Evergreen Runtime](https://developer.microsoft.com/microsoft-edge/webview2/)。包内不包含微信和 WebView2；微信适配须匹配精确模块指纹，当前验证版本为 4.1.15.56。
4. 双击 `MPider.exe`。设置中的 Python 路径保留默认 `python`，程序优先使用自身旁的 `runtime/python.exe`。已有用户曾填写绝对路径时，需恢复默认或填写本发行包 Python 的完整路径。
5. 在「公众号」开启「公众号监测」，再在微信手动打开目标公众号主页，等待登记名称与完整会话。监测不自动入库文章；在该公众号行点击「同步最新文章（首页）」或「采集历史文章」，后者单独选择追加篇数或截止日期。
6. 文章入库后再单独下载正文。未下载文章显示下载，已下载文章显示阅读和导出；整号或批量导出要求目标正文全部下载。任务按首页同步、历史采集、正文下载三个标签管理，列表默认每页10条。

程序未代码签名，系统可能阻止运行；请自行核对下载来源与 SHA-256 后决定是否运行。当前工作台不要求安装根证书或修改系统代理。Frida 可能引发微信崩溃，长时间稳定性和请求风控仍有限制，详见 README。

## 从源码构建

构建环境：Windows x64、Visual Studio 2022 Build Tools（C++ 桌面开发及 Windows SDK）、Rust stable MSVC、Node.js 24+、pnpm 9+；组装完整发行包还需构建机 Python 3.9+。包管理器只使用 pnpm。

在独立 Git worktree 中执行：

```powershell
pnpm install --frozen-lockfile
pnpm exec tsc --noEmit -p frontend/tsconfig.json
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked -p mpider-core -p article-md
python -m pip install -r native/requirements.txt
python -B -m unittest discover -s native -p 'test_*.py'
pnpm test
powershell -NoProfile -ExecutionPolicy Bypass -File tools\build-windows.ps1 -Release -Portable -PythonPath python -SourceRevision (git rev-parse HEAD)
```

`Bypass` 仅作用于当前 PowerShell 进程，不修改机器策略。脚本遵循已有 `CARGO_TARGET_DIR`，运行 `pnpm tauri build --no-bundle`，把前端与已内嵌的 native worker、JS 和 profiles 编译到 EXE，再附带隔离 Python/Frida 环境，产物位于：

- `dist-windows/MPider-v1.0.2-windows-x64.zip`：完整便携包。
- `dist-windows/SHA256SUMS`：ZIP 的 SHA-256。
- `dist-windows/verification.json`：离线验收记录。

不加 `-Portable` 时只生成开发用 `dist-windows/MPider.exe`，需要自行准备完整 Python/Frida 环境。`tools/setup-native-runtime.ps1` 可为已有 Python 安装隔离依赖，不能替代完整发行包的固定依赖校验。

## 发行包与验收边界

`tools/windows-runtime.lock.json` 固定 Python 和 Windows wheels 的下载地址、版本与 SHA-256；构建时校验后原样保留 Python `LICENSE.txt`、wheel 的 `dist-info` 完整许可证，并补充 Frida wxWindows 许可引用的 GNU LGPL v2 全文。下载缓存只在忽略目录 `dist-windows/.downloads`。

构建机还会从 Cargo/pnpm 依赖复制完整许可与字体 OFL，补回上游遗漏的许可文件；MPL 组件和部分缺声明组件的原版源码一并附带。详见包内 `THIRD-PARTY-NOTICES.md` 与 `licenses/dependencies/index.json`，该过程只在构建阶段依赖 Cargo/pnpm。

`release-manifest.json` 列出包内文件摘要、源码 revision 与依赖版本。独立验证命令为：

```powershell
python tools/verify-windows-release.py --directory dist-windows\MPider-v1.0.2-windows-x64
```

该检查只核对文件完整性、x64 PE、实际 Python/依赖版本以及虚构材料 AES-GCM 和当前用户 DPAPI 往返，不启动微信、不读取真实会话、不发送采集请求。**离线检查成功不代表真实采集或长期稳定性通过。**页面交互验收需在交互桌面另行进行，真实数据不得进入公开包。

`.github/workflows/release.yml` 在 Windows 执行类型、Rust、Python、Node 离线检查后构建，并重新解压 ZIP 验证。手动触发只留存 Actions artifact；`v*` 标签必须与应用版本一致，只创建预发布草稿，核验后再公开。当前流程不发布 macOS 安装包，避免将旧代理工作流支持误认为新原生工作台支持。

## 数据与异常

数据库默认位于应用数据目录的 `mpider.db`，以设置页显示路径为准。正文归档和实验报告位于同目录 `workbench/archive/`、`workbench/experiments/`。会话与原始游标受当前 Windows 用户 DPAPI 保护，不能简单跨用户复制使用；普通文章导出不含密钥。便携包表示程序免安装，**不表示用户数据存于 EXE 旁**。

公众号监测在跨页、任务完成和窗口关闭到托盘后保持开启；底栏常驻显示状态，公众号页也提供开关。开启监测本身不发列表请求或采集文章，程序不会搜索、点击、刷新或改变微信窗口。应用重启不静默附加微信。

首页/历史任务首次需要新会话时会自动启动监测；待提示就绪后，手动打开或刷新目标公众号主页，完整新会话到齐后自动继续原任务。只有首页响应、其他公众号或迟到旧事件均不能恢复任务；更新后仍失败时停止自动循环，可点击“更新会话并继续”。暂停、取消、关闭监测或应用重启都会撤销自动继续意图，详见[实现原理](how-it-works.md#会话更新与自动继续)。

关闭主窗口进入托盘，从托盘退出才停止程序。重启后未完成任务保留检查点并暂停，用户继续后再按需要更新会话；实验保留日程并记录缺测。实验室默认隐藏，在“设置 → 系统配置 → 显示实验室”开启；关闭入口保留已有实验与后台进度。不要手工修改游标或删除任务来解决会话过期。

底栏仅在存在未结束任务时显示正文/历史进度；点击正文区域查看下载队列，点击历史区域进入对应标签。底部另有mp2rss持续订阅引导与反馈入口，外部链接由系统浏览器打开。

微信升级后必须重新验证指纹与桥协议，不能只替换版本号或套用旧 RVA。监测失败、网络失败、会话失效和游标不可用分别记录，构建成功不能视为采集成功。适配细节见[协议说明](wechat-session-protocol.md)，实际发行验收结果见对应 GitHub Release。

会话过期不等于游标失效。更新会话后可尝试沿检查点继续，不给所有游标设定统一有效期。
