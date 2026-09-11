//! mpider 的 Tauri v2 外壳：把 `mpider-core` 的能力用 `#[tauri::command]` 薄封装给前端，
//! 另有托盘与证书向导等跨平台工程化结构。
//!
//! **关键约束（§9.0 / §3.4）**：抓凭证依赖微信内置浏览器把流量导到本机 MITM 代理，
//! 期间需要激活微信窗口、走系统信任的根 CA、可能弹系统授权框——这些都要求一个
//! **有交互的桌面会话**。因此本应用**不能做成无头后台服务**；托盘/自启只是让它常驻，
//! 真正抓取仍需前台桌面环境（RPA 送种子链接 + 接力自动翻页）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::Serialize;
use tauri::{Emitter, Manager, State};

use mpider_core::applog::{self, Stage};
use mpider_core::{run_loop_real, Account, AppLogRow, CaMaterial, LogEvent, RealRunConfig, Store};

/// 应用共享状态：核心存储句柄 + 主循环的运行/停止标志。
///
/// `Store` 内部是 `Mutex<Connection>`（`Send + Sync`），用 `Arc` 在 tokio 多任务间共享；
/// 未来 MITM 代理任务、编排任务都从这里取同一个 store（对齐 §1 单进程模型）。
///
/// 主循环：`loop_running` 记录后台主循环（巡检模式 / 历史模式）是否在跑（避免重复启动）；
/// `loop_stop` 是给 `run_loop_real` 的停止信号（置真后，当前执行单元处理完即退出）。
///
/// 日志：启动时把 `store` 绑定为 `mpider_core::applog` 的入库目标（关键节点入 `app_log`，留 3 天），
/// 并订阅总线把每条 [`LogEvent`] 经 [`EV_LOG`] 推给前端；`_log_sub` 持有订阅句柄（Drop 即退订，
/// 与应用同寿命）。
struct AppState {
    store: Arc<Store>,
    db_path: String,
    loop_running: Arc<AtomicBool>,
    loop_stop: Arc<AtomicBool>,
    _log_sub: applog::Subscription,
}

/// `core_health` 返回体：一眼看出「前端 ↔ Rust ↔ mpider-core」链路是否打通。
#[derive(Serialize)]
struct HealthInfo {
    ok: bool,
    app_version: String,
    tauri_version: String,
    core: String,
    db_path: String,
    account_count: usize,
    platform: String,
    arch: String,
}

/// 根证书状态（傻瓜式向导用）。`installed` = 钥匙串里已找到本工具的证书。
#[derive(Serialize, Clone)]
struct CertInfo {
    /// 是否已安装（钥匙串中存在 CN="MPider MITM CA" 的证书）。
    installed: bool,
    /// 平台（macos/windows/…），决定能否一键自动安装。
    platform: String,
    /// 能否一键自动安装（当前仅 macOS/Windows）。
    can_auto: bool,
    /// 证书文件路径（手动安装 / 在访达查看用；可能尚未生成）。
    ca_path: String,
    /// 证书文件是否已生成落盘。
    ca_exists: bool,
    /// 手动安装命令（一键失败时的兜底，大白话步骤在前端展示）。
    manual_command: String,
}

/// 一键安装的结果（带过程日志，前端逐行展示）。
#[derive(Serialize)]
struct CertInstallOutcome {
    ok: bool,
    installed: bool,
    log: Vec<String>,
    info: CertInfo,
}

/// 健康检查：读一次 accounts 计数，证明 core 的 SQLite 存储在本进程内可用。
#[tauri::command]
fn core_health(state: State<'_, AppState>) -> Result<HealthInfo, String> {
    let accounts = state.store.list_accounts().map_err(|e| e.to_string())?;
    Ok(HealthInfo {
        ok: true,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        tauri_version: tauri::VERSION.to_string(),
        core: "mpider-core (path 依赖，同 workspace 编译)".to_string(),
        db_path: state.db_path.clone(),
        account_count: accounts.len(),
        platform: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
    })
}

/// 公众号列表一行：账号基本信息 + 该号凭证**热数据**摘要（`credentials` 表每号一行）。
///
/// 只暴露时间戳与计数，**不带** key / pass_ticket / uin 等参数本体（前端展示用，不需要）。
#[derive(Serialize)]
struct AccountItem {
    #[serde(flatten)]
    account: Account,
    /// 是否抓到过凭证（credentials 表有该号且 key 非空）。
    cred_has_key: bool,
    /// 凭证当前是否新鲜（key 非空、未实测失效、距抓到未超 TTL）。
    cred_fresh: bool,
    /// 最近一次抓到 / 刷新凭证的时刻（epoch 秒）。
    cred_captured_at: Option<f64>,
    /// 预估过期时刻（`captured_at + ttl`）。
    cred_expires_at: Option<f64>,
    /// 实测失效时刻（回放拿到 ret=-3 时打点；有值即当前 key 已失效）。
    cred_invalidated_at: Option<f64>,
    /// 最近一次拿它回放接口的时刻。
    cred_last_used_at: Option<f64>,
    /// 当前 key 累计回放次数。
    cred_use_count: i64,
    /// 累计换 key 次数。
    cred_refresh_count: i64,
    /// 该号已采到的最新一篇文章的发布时间（epoch 秒；没有文章或都无发布时间时为 None）。
    /// 列表「最新更新」列用：一眼看出手头数据新到哪天。
    latest_published_at: Option<f64>,
    /// 该号最新一条「抓取历史文章」任务（任何状态；没有过则 None）。
    history: Option<mpider_core::HistoryJobView>,
}

/// 读取某公众号的凭证完整行（`credentials` 表热数据，含 key / pass_ticket 等参数本体），
/// 供公众号列表「查看凭证数据」对话框展示；无则 `None`。数据只在本机弹窗显示，不进日志。
#[tauri::command]
fn get_credential(
    state: State<'_, AppState>,
    biz: String,
) -> Result<Option<mpider_core::Credential>, String> {
    state.store.get_credential(&biz).map_err(|e| e.to_string())
}

/// 公众号列表一页（翻页用：行 + 总数）。
#[derive(Serialize)]
struct AccountPage {
    items: Vec<AccountItem>,
    total: i64,
}

/// 列出已知公众号（按 last_seen 倒序），每行附带该号凭证热数据摘要。
///
/// `limit` 省略 = 全量（文章页的公众号下拉用）；给了则按 `limit/offset` 翻页（公众号列表页用）。
/// `total` 恒为满足条件的总数，与本页行数无关。
#[tauri::command]
fn list_accounts(
    state: State<'_, AppState>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<AccountPage, String> {
    let accounts = match limit {
        Some(n) => state
            .store
            .list_accounts_page(n.clamp(1, 1000), offset.unwrap_or(0)),
        None => state.store.list_accounts(),
    }
    .map_err(|e| e.to_string())?;
    let total = state.store.count_accounts().map_err(|e| e.to_string())?;
    let creds: std::collections::HashMap<String, mpider_core::Credential> = state
        .store
        .list_credentials()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|c| (c.biz.clone(), c))
        .collect();
    let latest = state
        .store
        .latest_published_by_biz()
        .map_err(|e| e.to_string())?;
    let ttl = state.store.cred_ttl() as f64;
    let now = mpider_core::model::now();
    let histories = state
        .store
        .history_latest_by_biz()
        .map_err(|e| e.to_string())?;
    let items = accounts
        .into_iter()
        .map(|account| {
            let c = creds.get(&account.biz);
            let history = histories
                .get(&account.biz)
                .map(|j| mpider_core::history::view(&state.store, j));
            let latest_published_at = latest.get(&account.biz).copied();
            let has_key = c
                .and_then(|c| c.key.as_deref())
                .is_some_and(|k| !k.is_empty());
            let fresh = c.is_some_and(|c| {
                has_key && c.invalidated_at.is_none() && (now - c.captured_at) < ttl
            });
            AccountItem {
                account,
                cred_has_key: has_key,
                cred_fresh: fresh,
                cred_captured_at: c.map(|c| c.captured_at),
                cred_expires_at: c.and_then(|c| c.expires_at),
                cred_invalidated_at: c.and_then(|c| c.invalidated_at),
                cred_last_used_at: c.and_then(|c| c.last_used_at),
                cred_use_count: c.map_or(0, |c| c.use_count),
                cred_refresh_count: c.map_or(0, |c| c.refresh_count),
                latest_published_at,
                history,
            }
        })
        .collect();
    Ok(AccountPage { items, total })
}

/// **手动抓凭证代理**（调试，`mpider_core::manualcap`）状态：是否在跑、监听地址、
/// 系统代理是否已指向、开启 / 自动停止时刻、最近错误。设置页「抓包与系统代理」面板轮询。
#[tauri::command]
fn capture_status() -> mpider_core::ManualCaptureStatus {
    mpider_core::manualcap::status()
}

/// **手动开启抓凭证代理**：不跑任务只起 MITM + 系统代理，请求追踪写「抓凭证」环节日志，
/// `minutes` 分钟后自动停（默认 10、封顶 60）。与整链互斥（采集进行中会报「已有采集在运行」）。
/// CA / db 路径与 `run_agent_once` 一样覆盖成 app_data_dir 下的稳定路径。
#[tauri::command]
async fn capture_manual_start(
    app: tauri::AppHandle,
    mut cfg: RealRunConfig,
    minutes: Option<u64>,
) -> Result<mpider_core::ManualCaptureStatus, String> {
    let (crt, key) = ca_paths(&app)?;
    cfg.ca_cert_path = crt.display().to_string();
    cfg.ca_key_path = key.display().to_string();
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    cfg.db_path = dir.join("mpider.db").display().to_string();
    mpider_core::manualcap::start(&cfg, minutes)
        .await
        .map_err(|e| format!("{e:#}"))
}

/// **停止手动抓凭证代理**（幂等）：复位系统代理 → 停 MITM。
#[tauri::command]
async fn capture_manual_stop() -> mpider_core::ManualCaptureStatus {
    mpider_core::manualcap::stop().await
}

/// 本工具根证书的固定 CommonName（见 `mpider_core::ca`，用于在钥匙串里识别本工具的证书）。
const CA_COMMON_NAME: &str = "MPider MITM CA";

/// 证书文件的稳定落盘路径（app_data_dir/ca.crt + ca.key）。
///
/// 说明：这也是**将来 core 应从此路径载入 CA** 的约定位置（当前 `run_once_real` 每次重新
/// 生成临时 CA，安装的这张暂不会被真机抓取直接复用——待办）。
fn ca_paths(app: &tauri::AppHandle) -> Result<(PathBuf, PathBuf), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    Ok((dir.join("ca.crt"), dir.join("ca.key")))
}

/// 系统信任库里是否已存在本工具的证书（read-only，安全）。
///
/// - macOS：`security find-certificate -c <CN>`，命中退出码 0。
/// - Windows：`certutil -store -user Root <CN>`，按 CommonName 在当前用户 Root
///   存储里查找，命中退出码 0（安装走的也是 `-user Root`，两端一致）。
fn keychain_has_cert() -> bool {
    match std::env::consts::OS {
        "macos" => std::process::Command::new("security")
            .args(["find-certificate", "-c", CA_COMMON_NAME])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false),
        "windows" => std::process::Command::new("certutil")
            .args(["-store", "-user", "Root", CA_COMMON_NAME])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false),
        _ => false,
    }
}

/// 组装当前证书状态（供 `cert_status` 与安装后回读）。
fn cert_info(app: &tauri::AppHandle) -> CertInfo {
    let (crt, _key) = ca_paths(app).unwrap_or_default();
    let os = std::env::consts::OS;
    let manual_command = match os {
        "macos" => format!(
            "security add-trusted-cert -r trustRoot -k ~/Library/Keychains/login.keychain-db \"{}\"",
            crt.display()
        ),
        "windows" => format!("certutil -addstore -user Root \"{}\"", crt.display()),
        _ => String::new(),
    };
    CertInfo {
        installed: keychain_has_cert(),
        platform: os.to_string(),
        can_auto: matches!(os, "macos" | "windows"),
        ca_path: crt.display().to_string(),
        ca_exists: crt.exists(),
        manual_command,
    }
}

/// 查询根证书状态（read-only）。
#[tauri::command]
fn cert_status(app: tauri::AppHandle) -> CertInfo {
    cert_info(&app)
}

/// **一键（傻瓜式）安装根证书**：生成/复用本工具的根证书 → 落到稳定路径 → 装进系统信任库。
///
/// - macOS：`security add-trusted-cert -r trustRoot -k <login.keychain> <ca.crt>`，
///   系统会**弹出密码框让用户授权**（无需 sudo，装在当前用户的登录钥匙串）。
/// - Windows：`certutil -addstore -user Root <ca.crt>`（用户级 Root 存储）。
///
/// 只装本工具自签的这一张证书；用户随时可在「钥匙串访问 / certmgr」里删除。
#[tauri::command]
fn install_cert(app: tauri::AppHandle) -> Result<CertInstallOutcome, String> {
    let mut log: Vec<String> = Vec::new();
    let (crt, key) = ca_paths(&app)?;
    if let Some(p) = crt.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }

    // 生成或复用 CA（复用可避免重复安装多张证书）。
    if crt.exists() && key.exists() {
        log.push(format!("复用已有证书文件：{}", crt.display()));
    } else {
        log.push("生成本工具专用的根证书…".to_string());
        let ca = CaMaterial::generate().map_err(|e| e.to_string())?;
        std::fs::write(&crt, ca.cert_pem.as_bytes()).map_err(|e| e.to_string())?;
        std::fs::write(&key, ca.key_pem.as_bytes()).map_err(|e| e.to_string())?;
        log.push(format!("已保存到：{}", crt.display()));
    }

    let ok = match std::env::consts::OS {
        "macos" => install_ca_macos(&crt, &mut log),
        "windows" => install_ca_windows(&crt, &mut log),
        other => {
            log.push(format!(
                "暂不支持在 {other} 上自动安装，请按下方手动步骤操作。"
            ));
            false
        }
    };

    let info = cert_info(&app);
    if ok {
        log.push("✅ 完成！证书已装好，可以开始抓取了。".to_string());
        applog::info(
            Stage::App,
            format!("根证书已安装到系统信任库：{}", crt.display()),
        );
    } else if !info.installed {
        log.push("⚠ 自动安装未完成。可点「手动安装」按提示操作，或重试。".to_string());
        applog::warn(
            Stage::App,
            format!(
                "根证书自动安装未完成：{}",
                log.last().cloned().unwrap_or_default()
            ),
        );
    }
    Ok(CertInstallOutcome {
        ok,
        installed: info.installed,
        log,
        info,
    })
}

/// macOS 一键安装：装进当前用户登录钥匙串并设为受信任根（会弹系统密码框授权）。
fn install_ca_macos(crt: &Path, log: &mut Vec<String>) -> bool {
    // 解析登录钥匙串路径（失败则让 security 用默认钥匙串）。
    let keychain = std::process::Command::new("security")
        .args(["default-keychain", "-d", "user"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty());

    let mut cmd = std::process::Command::new("security");
    cmd.args(["add-trusted-cert", "-r", "trustRoot"]);
    if let Some(kc) = &keychain {
        cmd.args(["-k", kc]);
    }
    cmd.arg(crt);
    log.push("系统将弹出密码框，请输入本机登录密码授权安装…".to_string());
    match cmd.output() {
        Ok(o) if o.status.success() => {
            log.push("已添加到钥匙串并设为受信任。".to_string());
            true
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            log.push(format!("安装命令返回错误：{}", err.trim()));
            false
        }
        Err(e) => {
            log.push(format!("无法执行 security 命令：{e}"));
            false
        }
    }
}

/// **移除根证书**：把本工具装进系统信任库的那张证书删掉（证书文件保留，下次一键安装直接复用）。
///
/// - macOS：先 `security remove-trusted-cert <ca.crt>` 撤销信任设置（可能弹密码框），
///   再 `security find-certificate -a -Z` 列出所有同名证书的指纹、逐张 `delete-certificate -Z <sha1> -t`
///   删（按 CN 删遇到多份会报 ambiguous，多次安装 / 不同数据目录各留一张时必现）。
/// - Windows：`certutil -delstore -user Root <CN>`（按 CommonName 删用户级 Root 存储里的匹配项）。
///
/// 返回形状与 `install_cert` 相同（`ok` 表示删除命令成功，`installed` 是删完后实查的结果）。
#[tauri::command]
fn uninstall_cert(app: tauri::AppHandle) -> Result<CertInstallOutcome, String> {
    let mut log: Vec<String> = Vec::new();
    let (crt, _key) = ca_paths(&app)?;
    let ok = match std::env::consts::OS {
        "macos" => uninstall_ca_macos(&crt, &mut log),
        "windows" => uninstall_ca_windows(&mut log),
        other => {
            log.push(format!(
                "暂不支持在 {other} 上自动移除，请到系统证书管理里手动删除「{CA_COMMON_NAME}」。"
            ));
            false
        }
    };
    let info = cert_info(&app);
    if ok && !info.installed {
        log.push("✅ 已移除。证书文件仍保留在本机，需要时可再次一键安装。".to_string());
        applog::info(Stage::App, "根证书已从系统信任库移除");
    } else if info.installed {
        log.push(
            "⚠ 证书仍在系统信任库里，可重试或到「钥匙串访问 / certmgr」手动删除。".to_string(),
        );
        applog::warn(
            Stage::App,
            format!(
                "根证书移除未完成：{}",
                log.last().cloned().unwrap_or_default()
            ),
        );
    }
    Ok(CertInstallOutcome {
        ok: ok && !info.installed,
        installed: info.installed,
        log,
        info,
    })
}

/// 列出钥匙串搜索列表里所有 CN 为本工具的证书 SHA-1 指纹（`security find-certificate -a -Z`）。
///
/// 多次安装 / 不同数据目录各生成一张 CA 时会有多份同名证书，`delete-certificate -c <CN>`
/// 遇到多份会拒绝（"is ambiguous"），所以删除一律按指纹逐张来。
fn macos_cert_sha1s() -> Vec<String> {
    let Ok(o) = std::process::Command::new("security")
        .args(["find-certificate", "-a", "-c", CA_COMMON_NAME, "-Z"])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| l.trim().strip_prefix("SHA-1 hash:"))
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .collect()
}

/// macOS 移除：撤销当前证书文件的信任设置，再按指纹逐张删钥匙串里的同名证书（`-t` 连带删各自的信任设置）。
fn uninstall_ca_macos(crt: &Path, log: &mut Vec<String>) -> bool {
    if crt.exists() {
        log.push("系统可能弹出密码框，请输入本机登录密码授权移除…".to_string());
        // 撤销信任设置；证书早已被手动删掉时这一步会报错，不影响后续删证书。
        if let Ok(o) = std::process::Command::new("security")
            .args(["remove-trusted-cert"])
            .arg(crt)
            .output()
        {
            if o.status.success() {
                log.push("已撤销该证书的信任设置。".to_string());
            }
        }
    }
    let hashes = macos_cert_sha1s();
    if hashes.is_empty() {
        log.push("钥匙串里没有找到本工具的证书。".to_string());
        return true;
    }
    if hashes.len() > 1 {
        log.push(format!(
            "钥匙串里有 {} 份同名证书（多次安装或不同数据目录留下的），逐份删除。",
            hashes.len()
        ));
    }
    let mut removed = 0;
    for h in &hashes {
        match std::process::Command::new("security")
            .args(["delete-certificate", "-Z", h, "-t"])
            .output()
        {
            Ok(o) if o.status.success() => removed += 1,
            Ok(o) => {
                log.push(format!(
                    "删除命令返回错误：{}",
                    String::from_utf8_lossy(&o.stderr).trim()
                ));
                return false;
            }
            Err(e) => {
                log.push(format!("无法执行 security 命令：{e}"));
                return false;
            }
        }
    }
    log.push(format!("已从钥匙串删除 {removed} 份证书。"));
    true
}

/// Windows 移除：从当前用户 Root 存储按 CN 删除。
fn uninstall_ca_windows(log: &mut Vec<String>) -> bool {
    match std::process::Command::new("certutil")
        .args(["-delstore", "-user", "Root", CA_COMMON_NAME])
        .output()
    {
        Ok(o) if o.status.success() => {
            log.push("已从当前用户的受信任根存储删除。".to_string());
            true
        }
        Ok(o) => {
            log.push(format!(
                "certutil 返回错误：{}",
                String::from_utf8_lossy(&o.stderr).trim()
            ));
            false
        }
        Err(e) => {
            log.push(format!("无法执行 certutil：{e}"));
            false
        }
    }
}

/// Windows 一键安装：装进当前用户 Root 存储。
fn install_ca_windows(crt: &Path, log: &mut Vec<String>) -> bool {
    match std::process::Command::new("certutil")
        .args(["-addstore", "-user", "Root"])
        .arg(crt)
        .output()
    {
        Ok(o) if o.status.success() => {
            log.push("已添加到当前用户的受信任根存储。".to_string());
            true
        }
        Ok(o) => {
            log.push(format!(
                "certutil 返回错误：{}",
                String::from_utf8_lossy(&o.stderr).trim()
            ));
            false
        }
        Err(e) => {
            log.push(format!("无法执行 certutil：{e}"));
            false
        }
    }
}

// ============ 底部状态栏：系统级状态汇总 ============

/// 底部状态栏一次拉取的系统状态（证书 / 系统代理 / 抓包代理 / 微信）。
#[derive(Serialize)]
struct SystemStatus {
    platform: String,
    /// 根证书是否已装（钥匙串命中本工具 CN）。
    cert_installed: bool,
    /// 系统（全局）代理是否已开启。
    proxy_enabled: bool,
    /// 系统代理指向（host:port），未开启时可能仍返回上次配置。
    proxy_endpoint: Option<String>,
    /// 查询用的网络服务名（macOS，如 Wi-Fi）。
    proxy_service: String,
    /// 本工具的抓包 MITM 代理是否在跑（读 `mpider_core::runstate`，运行窗口内为 true）。
    capture_running: bool,
    capture_addr: Option<String>,
    /// 微信是否在运行（进程存在≈已打开；精确登录态需 RPA，暂以进程存在近似）。
    wechat_running: bool,
    /// 本平台能否**自动点开**种子链接（Windows 原生 RPA）。`false` = 人工模式（mac / 其它）：运行时提醒用户
    /// 在微信里打开一篇文章即可，前端据此**整体隐藏**种子链接设置 / 框选标定 / 自检 / 测试点击等 UI，
    /// 而不是显示「不支持」。
    rpa_auto: bool,
}

/// 汇总系统状态给底部状态栏（前端定时轮询）。`service` = 要查的网络服务名。
#[tauri::command]
fn system_status(service: Option<String>) -> SystemStatus {
    let svc = service
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Wi-Fi".to_string());
    let (proxy_enabled, proxy_endpoint) = read_system_proxy(&svc);
    // 抓包代理由每次 run 临时起停，真实状态从 core 的进程级 runstate 读
    // （前端按钮和托盘菜单两条触发路径都能被状态栏看见）。
    let capture_addr = mpider_core::runstate::capture_addr();
    SystemStatus {
        platform: std::env::consts::OS.to_string(),
        cert_installed: keychain_has_cert(),
        proxy_enabled,
        proxy_endpoint,
        proxy_service: svc,
        capture_running: capture_addr.is_some(),
        capture_addr,
        wechat_running: wechat_running(),
        rpa_auto: !mpider_core::get_controller(true).manual(),
    }
}

/// **启动自检：清理上次残留的系统代理**。系统代理开着、指向回环地址、端口无人监听 → 判为上次 App 没正常收尾
/// （崩溃 / `kill -9` / 断电，退出钩子都没机会跑）留下的现场，直接关掉并返回一句提示给 GUI 弹 toast；
/// 不是残留返回 `None`。正在抓凭证（编排代理 / 手动抓凭证代理在跑）时代理是本次合法设置，跳过不查。
/// 前端启动时调一次；幂等，多调无害。
#[tauri::command]
fn sysproxy_reset_stale(service: Option<String>) -> Result<Option<String>, String> {
    if mpider_core::runstate::capture_addr().is_some() || mpider_core::manualcap::is_running() {
        return Ok(None);
    }
    let svc = service
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Wi-Fi".to_string());
    // mac 的 HTTP / HTTPS 代理是两项独立设置，任一项是残留就清（关闭命令两项一起关）；Windows 只有一项。
    let readings: Vec<(bool, Option<String>)> = if std::env::consts::OS == "macos" {
        vec![
            read_system_proxy_macos_kind(&svc, "-getwebproxy"),
            read_system_proxy_macos_kind(&svc, "-getsecurewebproxy"),
        ]
    } else {
        vec![read_system_proxy(&svc)]
    };
    let Some(port) = readings.iter().find_map(|(enabled, endpoint)| {
        mpider_core::sysproxy::is_stale_loopback_proxy(*enabled, endpoint.as_deref())
    }) else {
        return Ok(None);
    };
    mpider_core::sysproxy::disable_system_proxy(&svc)
        .map_err(|e| format!("发现上次残留的系统代理（127.0.0.1:{port}）但关闭失败：{e}"))?;
    let msg = format!("上次没有正常退出，系统代理还指向已失效的 127.0.0.1:{port}，已自动关闭");
    mpider_core::applog::warn(mpider_core::applog::Stage::Proxy, msg.clone());
    Ok(Some(msg))
}

/// 读系统（全局）代理开关与指向。
fn read_system_proxy(service: &str) -> (bool, Option<String>) {
    match std::env::consts::OS {
        "macos" => read_system_proxy_macos(service),
        "windows" => read_system_proxy_windows(),
        _ => (false, None),
    }
}

/// macOS：`networksetup -getsecurewebproxy <service>` 解析 Enabled/Server/Port（状态展示以 HTTPS 代理为准）。
fn read_system_proxy_macos(service: &str) -> (bool, Option<String>) {
    read_system_proxy_macos_kind(service, "-getsecurewebproxy")
}

/// macOS：按 `kind`（`-getwebproxy` HTTP / `-getsecurewebproxy` HTTPS）读一项代理的 Enabled/Server/Port。
/// 本工具设代理时两项一起设，但残留清理要两项都看：只剩一项开着（如用户手工关了另一项）同样会断网。
fn read_system_proxy_macos_kind(service: &str, kind: &str) -> (bool, Option<String>) {
    let out = std::process::Command::new("networksetup")
        .args([kind, service])
        .output();
    let Ok(o) = out else {
        return (false, None);
    };
    if !o.status.success() {
        return (false, None);
    }
    let text = String::from_utf8_lossy(&o.stdout);
    let (mut enabled, mut server, mut port) = (false, String::new(), String::new());
    for line in text.lines() {
        let l = line.trim();
        if let Some(v) = l.strip_prefix("Enabled:") {
            enabled = v.trim().eq_ignore_ascii_case("Yes");
        } else if let Some(v) = l.strip_prefix("Server:") {
            server = v.trim().to_string();
        } else if let Some(v) = l.strip_prefix("Port:") {
            port = v.trim().to_string();
        }
    }
    let endpoint = if server.is_empty() {
        None
    } else {
        Some(format!("{server}:{port}"))
    };
    (enabled, endpoint)
}

/// Windows：注册表 Internet Settings 的 ProxyEnable / ProxyServer（best-effort）。
fn read_system_proxy_windows() -> (bool, Option<String>) {
    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";
    let enabled = std::process::Command::new("reg")
        .args(["query", KEY, "/v", "ProxyEnable"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("0x1"))
        .unwrap_or(false);
    let endpoint = std::process::Command::new("reg")
        .args(["query", KEY, "/v", "ProxyServer"])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .find(|l| l.contains("ProxyServer"))
                .and_then(|l| l.split_whitespace().last().map(str::to_string))
        });
    (enabled, endpoint)
}

/// 微信进程是否在运行（best-effort）。
fn wechat_running() -> bool {
    match std::env::consts::OS {
        "macos" => {
            let hit = |name: &str| {
                std::process::Command::new("pgrep")
                    .args(["-x", name])
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
            };
            hit("WeChat") || hit("Weixin") || hit("微信")
        }
        "windows" => std::process::Command::new("tasklist")
            .output()
            .ok()
            .map(|o| {
                let s = String::from_utf8_lossy(&o.stdout);
                s.contains("WeChat.exe") || s.contains("Weixin.exe")
            })
            .unwrap_or(false),
        _ => false,
    }
}

/// 事件名：环节日志（`LogEvent` JSON，整链各环节 + 应用级事件，前端日志页 / 控制面板订阅）
/// 与每条任务的最终结果（前端 `listen` 订阅）。
const EV_LOG: &str = "agent://log";

/// 拉起主循环（`mpider_core::run_loop_real`）：`sweep_on` 为真是巡检模式（「开始巡检」），否则是历史模式
/// （开始 / 继续历史抓取时拉起，没有活动任务自行退出）。
///
/// 幂等：已在跑则返回 `Err`（调用方按需处理）。每次启动前清零 `loop_stop`，把 CA / db_path 覆盖成
/// 稳定路径（复用「一键安装」时已被系统信任的那张 CA；db 指向 GUI 读取的同一个库），随后 `spawn`
/// 一个后台任务跑 `run_loop_real`——各环节日志经 `EV_LOG` 实时推给前端；循环退出（收到停止信号、
/// 历史任务结束或出错）后复位 `loop_running` 标志。
fn spawn_main_loop(
    app: &tauri::AppHandle,
    state: &AppState,
    mut cfg: RealRunConfig,
    sweep_on: bool,
) -> Result<(), String> {
    if state.loop_running.swap(true, Ordering::SeqCst) {
        return Err(if sweep_on {
            "巡检已在进行中。".to_string()
        } else {
            "主循环已在跑。".to_string()
        });
    }
    state.loop_stop.store(false, Ordering::SeqCst);
    mpider_core::runstate::sweep_set_stopping(false);

    let (crt, key) = match ca_paths(app) {
        Ok(v) => v,
        Err(e) => {
            state.loop_running.store(false, Ordering::SeqCst);
            return Err(e);
        }
    };
    cfg.ca_cert_path = crt.display().to_string();
    cfg.ca_key_path = key.display().to_string();
    cfg.db_path = state.db_path.clone();

    let running = state.loop_running.clone();
    let stop = state.loop_stop.clone();
    if sweep_on {
        applog::info(
            Stage::App,
            format!(
                "巡检已启动：每批 {} 个号，轮间停留 {} 分钟，批次间隔 {}s，整批失败暂停 {} 分钟；结果上报={}，自动设系统代理={}，RPA={}",
                cfg.sweep_batch_size,
                cfg.sweep_idle_seconds / 60,
                cfg.sweep_batch_gap_seconds,
                cfg.sweep_fail_pause_seconds / 60,
                if cfg.report_config().active() {
                    format!("开（{}）", cfg.report_url.trim())
                } else {
                    "关".to_string()
                },
                if cfg.set_sysproxy { "开" } else { "关" },
                if cfg.rpa_enabled { "开" } else { "关" }
            ),
        );
    } else {
        applog::info(Stage::App, "历史抓取主循环已启动");
    }
    // 启动是正常操作，不推飞书；只有出错退出（无人值守时没人再领任务）才预警。
    tauri::async_runtime::spawn(async move {
        let res = run_loop_real(cfg, sweep_on, stop, |_| {}).await;
        if let Err(e) = res {
            applog::error(Stage::App, format!("主循环结束（出错）：{e}"));
            mpider_core::notify::notify(
                mpider_core::notify::Kind::App,
                format!(
                    "主循环结束（出错）：{}，不再领任务，请检查",
                    applog::redact(&e.to_string())
                ),
            );
        } else if sweep_on {
            applog::info(Stage::App, "巡检已停止。");
        }
        running.store(false, Ordering::SeqCst);
    });
    Ok(())
}

/// 等历史模式主循环退出（最多 `max_ms`）：历史任务结束 / 取消后，循环要到下一次检查（≤ `poll_secs`）
/// 才发现没有活动任务而退出，退出后整链锁才释放。返回退出后是否已空闲。
async fn wait_loop_idle(state: &AppState, max_ms: u64) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(max_ms);
    loop {
        if !state.loop_running.load(Ordering::SeqCst) && !mpider_core::runstate::run_busy() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// 历史抓取开始 / 继续后：保证有一条历史模式主循环在推进它。
///
/// 主循环没在跑就直接拉起。主循环在跑时分两种：上一条历史任务刚结束、循环正要退出（`run_forever`
/// 每轮先查有没有活动任务，查到没有就退出——与这次新建任务之间有竞争）；或者循环活着在等一条暂停的
/// 任务（`resume` 场景，循环下一轮会自己领到）。两者从标志上分不开，所以短暂等待：期间循环退出了就
/// 重新拉起，一直活着就认定它会接手。
async fn ensure_history_loop(
    app: &tauri::AppHandle,
    state: &AppState,
    cfg: RealRunConfig,
) -> Result<(), String> {
    if state.loop_running.load(Ordering::SeqCst) {
        // 给正在退出的循环一点时间；仍在跑就是活的历史模式循环，会自己领到任务。
        if !wait_loop_idle(state, 1_500).await {
            return Ok(());
        }
    } else if mpider_core::runstate::run_busy() {
        // 标志已清、整链锁还没释放（几十毫秒）。
        wait_loop_idle(state, 1_500).await;
    }
    for attempt in 0..10u32 {
        match spawn_main_loop(app, state, cfg.clone(), false) {
            Ok(()) => return Ok(()),
            Err(_) if attempt + 1 < 10 => {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            Err(e) => return Err(e),
        }
    }
    Err("主循环仍被占用，稍后再试".to_string())
}

/// **开始巡检**：后台长驻主循环，按批巡检库里全部公众号（`mpider_core::run_loop_real` 巡检模式）。
/// 有活动的历史抓取任务（进行中 / 暂停）时拒绝：两条链会抢同一个微信窗口与同一份预算。
/// 历史任务刚取消 / 完成时，历史模式循环可能还没退出（≤ 主循环空转间隔），等它几秒再拉起。
#[tauri::command]
async fn sweep_start(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    cfg: RealRunConfig,
) -> Result<(), String> {
    if mpider_core::runstate::sweep_on() {
        return Err("巡检已在进行中。".to_string());
    }
    mpider_core::sweep::start_check(&state.store).map_err(|e| e.to_string())?;
    if !wait_loop_idle(&state, 8_000).await {
        return Err("上一条历史抓取还在收尾，稍后再开始巡检。".to_string());
    }
    spawn_main_loop(&app, &state, cfg, true)
}

/// **停止巡检**：给主循环发停止信号（当前正在处理的批次会先跑完再退出；期间状态栏显示「停止中」）。
#[tauri::command]
fn sweep_stop(state: State<'_, AppState>) -> Result<(), String> {
    if !state.loop_running.load(Ordering::SeqCst) || !mpider_core::runstate::sweep_on() {
        return Ok(());
    }
    state.loop_stop.store(true, Ordering::SeqCst);
    mpider_core::runstate::sweep_set_stopping(true);
    applog::info(Stage::App, "收到停止巡检请求：当前批次跑完后退出。");
    Ok(())
}

/// `loop_status` 返回体：主循环是否在跑、巡检开关是否打开、是否正在停止。
#[derive(Serialize)]
struct LoopStatus {
    /// 后台主循环（巡检模式 / 历史模式）在跑。
    loop_running: bool,
    /// 巡检开关打开（含「停止中」：已请求停止、当前批次还在收尾）。
    sweep_on: bool,
    /// 已请求停止巡检、等当前批次结束。
    stopping: bool,
}

/// 主循环 / 巡检开关状态（前端启动 / 刷新时据此恢复按钮态；3 秒轮询）。
#[tauri::command]
fn loop_status(state: State<'_, AppState>) -> LoopStatus {
    let s = mpider_core::runstate::sweep_status();
    LoopStatus {
        loop_running: state.loop_running.load(Ordering::SeqCst),
        sweep_on: s.phase != mpider_core::SweepPhase::Off,
        stopping: s.stopping,
    }
}

/// 固定入口种子链接（前端展示「首次设置」教程用；拼法单一数据源在 mpider-core）：本机种子入口服务的
/// `http://<seed_host>:<seed_port>/`，host / port 来自前端保存的运行配置。
#[tauri::command]
fn seed_bootstrap_url(seed_host: String, seed_port: u16) -> String {
    mpider_core::seed_url(&seed_host, seed_port)
}

/// **应用种子入口服务配置**（幂等）：按运行配置的 host / 端口确保常驻服务在跑、更新停留参数；地址变了换绑。
/// 前端应用启动、保存配置、「种子链接」卡「重试」都会调；起不来（端口被占）返回错误文案，状态里也有 `last_error`。
#[tauri::command]
fn seed_server_apply(
    cfg: RealRunConfig,
) -> Result<mpider_core::seedserver::SeedServerStatus, String> {
    // 顺带应用 RPA「拉起后硬刷」开关（进程级，默认关）——启动 / 保存配置都经这里。
    mpider_core::rpa::set_hard_refresh(cfg.rpa_hard_refresh);
    // 人工模式（mac / 关闭 RPA）开待命页自轮询：没任务时种子页也长轮询等下一批（runner 运行开始时会再设一次）。
    mpider_core::seedserver::set_resident_mode(
        mpider_core::get_controller(cfg.rpa_enabled).manual(),
    );
    mpider_core::seedserver::apply(&cfg.seed_host, cfg.seed_port, cfg.seed_dwell())
        .map_err(|e| format!("{e:#}"))?;
    Ok(mpider_core::seedserver::status())
}

/// 种子入口服务状态（「种子链接」卡轮询）：是否在跑、监听地址、最近错误、访问统计、当前队列条数。
#[tauri::command]
fn seed_server_status() -> mpider_core::seedserver::SeedServerStatus {
    mpider_core::seedserver::status()
}

// ============ 公众号文章：列表 / 详情 / 计数 / 补采 ============

/// 「公众号文章」页的一行（在 ArticleRow 基础上补公众号昵称，前端免二次查询）。
#[derive(Serialize)]
struct ArticleItem {
    id: i64,
    biz: String,
    /// 所属公众号昵称（accounts 表；没有昵称时为 None，前端显示 biz）。
    account: Option<String>,
    title: Option<String>,
    author: Option<String>,
    published_at: Option<f64>,
    /// 1=已采详情 / 0=待采 / -1=永久不可用（微信提示页，原因见 detail_error）。
    detail_done: i64,
    /// 最近一次补详情失败/不可用的原因（前端悬停提示）。
    detail_error: Option<String>,
    content_url: Option<String>,
}

/// 文章列表一页（翻页用：行 + 满足过滤的总数）。
#[derive(Serialize)]
struct ArticlePage {
    items: Vec<ArticleItem>,
    total: i64,
}

/// 文章列表（发布时间倒序）。`biz` 过滤某个公众号；分页 limit/offset。
#[tauri::command]
fn list_articles(
    state: State<'_, AppState>,
    biz: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<ArticlePage, String> {
    let rows = state
        .store
        .list_articles(
            biz.as_deref(),
            limit.unwrap_or(200).clamp(1, 1000),
            offset.unwrap_or(0),
            false,
        )
        .map_err(|e| e.to_string())?;
    let total = state
        .store
        .count_articles(biz.as_deref())
        .map_err(|e| e.to_string())?;
    let nick: std::collections::HashMap<String, Option<String>> = state
        .store
        .list_accounts()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|a| (a.biz, a.nickname))
        .collect();
    let items = rows
        .into_iter()
        .map(|r| ArticleItem {
            account: nick.get(&r.biz).cloned().flatten(),
            id: r.id,
            biz: r.biz,
            title: r.title,
            author: r.author,
            published_at: r.published_at,
            detail_done: r.detail_done,
            detail_error: r.detail_error,
            content_url: r.content_url,
        })
        .collect();
    Ok(ArticlePage { items, total })
}

/// 单篇文章详情（Markdown 阅读用）。
#[derive(Serialize)]
struct ArticleDetail {
    id: i64,
    title: Option<String>,
    author: Option<String>,
    published_at: Option<f64>,
    content_url: Option<String>,
    content_md: Option<String>,
}

#[tauri::command]
fn get_article_detail(state: State<'_, AppState>, id: i64) -> Result<ArticleDetail, String> {
    let (row, md) = state
        .store
        .get_article_md(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("文章不存在：id={id}"))?;
    Ok(ArticleDetail {
        id: row.id,
        title: row.title,
        author: row.author,
        published_at: row.published_at,
        content_url: row.content_url,
        content_md: md,
    })
}

/// 文章计数（控制面板状态卡）：总数 / 已采详情数。
#[derive(Serialize)]
struct ArticleCounts {
    total: i64,
    detail_done: i64,
    /// 还会被自动批量补采处理的待补数（排除不可用与失败满 3 次的）。
    pending: i64,
}

#[tauri::command]
fn article_counts(state: State<'_, AppState>) -> Result<ArticleCounts, String> {
    let (total, detail_done, pending) = state.store.article_counts().map_err(|e| e.to_string())?;
    Ok(ArticleCounts {
        total,
        detail_done,
        pending,
    })
}

/// 软删除一个公众号，并级联软删除其全部文章。返回被删除的文章数（前端提示用）。
#[tauri::command]
fn delete_account(state: State<'_, AppState>, biz: String) -> Result<i64, String> {
    state
        .store
        .soft_delete_account(&biz)
        .map_err(|e| e.to_string())
}

/// **物理删除**一篇文章（DELETE 行，不可恢复；评论一并删除）。返回是否真的删掉了。
#[tauri::command]
fn delete_article(state: State<'_, AppState>, id: i64) -> Result<bool, String> {
    state
        .store
        .hard_delete_article(id)
        .map_err(|e| e.to_string())
}

// ============ 定时巡检 ============

/// 巡检进度 + 整机限流退避状态（控制面板轮询用；进程级单点，见 `mpider_core::runstate`）。
#[derive(Serialize)]
struct SweepInfo {
    #[serde(flatten)]
    status: mpider_core::SweepStatus,
    /// 参与巡检的号总数（未删除且未被排除）。
    total_enabled: i64,
    /// 退避等级（连续触发次数）。
    cooldown_level: u32,
}

/// 巡检进度（巡检没开时 phase=off）。
#[tauri::command]
fn sweep_status(state: State<'_, AppState>) -> Result<SweepInfo, String> {
    let status = mpider_core::runstate::sweep_status();
    let cd = mpider_core::runstate::cooldown_snapshot();
    Ok(SweepInfo {
        status,
        total_enabled: state.store.sweep_total().map_err(|e| e.to_string())?,
        cooldown_level: cd.level,
    })
}

/// 设置某公众号是否参与巡检（列表里的「暂停 / 恢复巡检」）。
#[tauri::command]
fn set_account_sweep_enabled(
    state: State<'_, AppState>,
    biz: String,
    enabled: bool,
) -> Result<(), String> {
    state
        .store
        .set_sweep_enabled(&biz, enabled)
        .map_err(|e| e.to_string())?;
    applog::info(
        Stage::Sweep,
        format!(
            "{} {}",
            state.store.account_label(&biz),
            if enabled {
                "恢复参与巡检"
            } else {
                "已暂停巡检"
            }
        ),
    );
    Ok(())
}

/// 立即开始新一轮巡检：跳过轮间停留 / 环境故障暂停，所有号（含本轮已记失败的）重新排队。
/// 巡检在跑时由巡检在下一次要批次时消费信号；没在跑时只清持久化状态，下次「开始巡检」即开新一轮。
#[tauri::command]
fn sweep_restart_pass(state: State<'_, AppState>) -> Result<(), String> {
    if mpider_core::runstate::sweep_on() {
        mpider_core::runstate::sweep_request_restart();
    } else {
        mpider_core::Sweep::reset_persisted(&state.store);
        applog::info(
            Stage::Sweep,
            "已重置巡检轮状态：下次「开始巡检」即开始新一轮".to_string(),
        );
    }
    Ok(())
}

/// 单号「重新巡检」（**立即**执行，不排队）：凭证有效就直接采最新列表（只发 getmsg）；凭证不可用则
/// 立刻对该号跑一次单链接接力换 key 再采（起代理 / 开微信，与巡检批次同一条链路）。有执行单元
/// （巡检批次 / 历史页 / 手动抓凭证代理）在跑或有活动的历史任务时拒绝，前端按 `run_status` 置灰按钮。
/// `cfg` 用系统设置里的采集参数；CA 路径覆盖成「一键安装」那张。
#[tauri::command]
async fn sweep_retry_account(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    biz: String,
    mut cfg: RealRunConfig,
) -> Result<mpider_core::RetryOutcome, String> {
    let (crt, key) = ca_paths(&app)?;
    cfg.ca_cert_path = crt.display().to_string();
    cfg.ca_key_path = key.display().to_string();
    cfg.db_path = state.db_path.clone();
    let store = state.store.clone();
    mpider_core::sweep_retry_account(&cfg, store, &biz)
        .await
        .map_err(|e| e.to_string())
}

// ============ 批量添加公众号（「公众号列表」页） ============

const EV_ADD_LINKS_PROGRESS: &str = "addlinks://progress";

/// `addlinks://progress` 事件载荷：已处理 / 总数 / 正在解析的行。
#[derive(Serialize, Clone)]
struct AddLinksProgress {
    done: usize,
    total: usize,
    current: String,
}

/// 「公众号列表 → 批量添加」：每行一条公众号文章**短链**。逐行校验、去重后逐条匿名直连文章页
/// （不开微信、不走代理），解析出公众号 biz / 名称 / 头像即建号并记下种子链接；只收图文文章。
/// 进度经 `addlinks://progress` 推送，返回逐行结果。
#[tauri::command]
async fn add_links(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    links: Vec<String>,
) -> Result<mpider_core::AddLinksSummary, String> {
    let emitter = app.clone();
    let progress = move |done: usize, total: usize, current: &str| {
        let _ = emitter.emit(
            EV_ADD_LINKS_PROGRESS,
            AddLinksProgress {
                done,
                total,
                current: current.to_string(),
            },
        );
    };
    Ok(mpider_core::addlink::add_links(&state.store, &links, &progress).await)
}

/// `run_status` 返回体：是否有执行单元在跑与当前阶段名。
#[derive(Serialize)]
struct RunStatus {
    /// 有执行单元（巡检批次 / 历史页 / 单号重试）正在处理——前端据此置灰「重新巡检」。
    running: bool,
    /// 主循环（巡检 / 历史模式）是否在跑——单元之间空闲时也为 true。
    loop_running: bool,
    phase: String,
}

/// 运行状态（前端据此置灰「重新巡检」等互斥操作、决定提示文案）。
#[tauri::command]
fn run_status() -> RunStatus {
    RunStatus {
        running: mpider_core::runstate::unit_busy(),
        loop_running: mpider_core::runstate::run_busy(),
        phase: mpider_core::phases::current().unwrap_or_default(),
    }
}

// ============ 抓取历史文章（`mpider_core::history`） ============

/// 上报闭包（历史任务在暂停 / 取消 / 结束时上报一次；未启用上报恒 skipped）。
fn report_fn_from(cfg: &RealRunConfig) -> Result<mpider_core::orchestrator::ReportFn, String> {
    let reporter =
        mpider_core::HttpReporter::new(cfg.report_config()).map_err(|e| e.to_string())?;
    Ok(Arc::new(move |payload| {
        let r = reporter.clone();
        Box::pin(async move { r.report(&payload).await })
    }))
}

/// 开始抓取某号的历史文章（同一时刻只能有一个号在抓；巡检开着时拒绝）。每页条数 / 页间隔 / 预算按
/// 传入的运行配置快照；任务建好后拉起历史模式主循环推进它。
#[tauri::command]
async fn history_start(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    biz: String,
    target: mpider_core::HistoryTarget,
    cfg: RealRunConfig,
) -> Result<mpider_core::HistoryJobView, String> {
    let job = mpider_core::history::start(&state.store, &biz, &target, &cfg.history_config())
        .map_err(|e| e.to_string())?;
    ensure_history_loop(&app, &state, cfg).await?;
    Ok(mpider_core::history::view(&state.store, &job))
}

/// 人工暂停（当前页结束后生效）；暂停即上报一次。
#[tauri::command]
async fn history_pause(
    state: State<'_, AppState>,
    biz: String,
    cfg: RealRunConfig,
) -> Result<mpider_core::HistoryJobView, String> {
    let job = mpider_core::history::pause(&state.store, &biz).map_err(|e| e.to_string())?;
    let report = report_fn_from(&cfg)?;
    mpider_core::history::report_job(&state.store, &job, &report).await;
    Ok(mpider_core::history::view(&state.store, &job))
}

/// 人工继续（任何暂停原因；巡检开着时拒绝）；主循环没在跑（如应用重启后）就拉起历史模式。
#[tauri::command]
async fn history_resume(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    biz: String,
    cfg: RealRunConfig,
) -> Result<mpider_core::HistoryJobView, String> {
    let job = mpider_core::history::resume(&state.store, &biz).map_err(|e| e.to_string())?;
    ensure_history_loop(&app, &state, cfg).await?;
    Ok(mpider_core::history::view(&state.store, &job))
}

/// 取消（当前页结束后生效）；取消即上报一次。
#[tauri::command]
async fn history_cancel(
    state: State<'_, AppState>,
    biz: String,
    cfg: RealRunConfig,
) -> Result<mpider_core::HistoryJobView, String> {
    let job = mpider_core::history::cancel(&state.store, &biz).map_err(|e| e.to_string())?;
    let report = report_fn_from(&cfg)?;
    mpider_core::history::report_job(&state.store, &job, &report).await;
    Ok(mpider_core::history::view(&state.store, &job))
}

/// 状态栏 / 公众号列表轮询：当前活动任务 + 当前微信号预算。
#[tauri::command]
fn history_status(state: State<'_, AppState>, cfg: RealRunConfig) -> mpider_core::HistoryStatus {
    mpider_core::history::status(&state.store, &cfg.history_config())
}

/// 开始前的估算（页数 / 耗时 / 剩余预算）。
#[tauri::command]
fn history_estimate(
    state: State<'_, AppState>,
    target: mpider_core::HistoryTarget,
    cfg: RealRunConfig,
) -> mpider_core::HistoryEstimate {
    mpider_core::history::estimate(&state.store, &target, &cfg.history_config())
}

/// 最近一条未确认的全局提醒（预算达到上限 / 微信号受限 / 历史任务暂停）。
#[tauri::command]
fn alert_latest() -> Option<mpider_core::Alert> {
    mpider_core::runstate::alert_latest()
}

/// 用户点「知道了」。
#[tauri::command]
fn alert_ack(id: i64) {
    mpider_core::runstate::alert_ack(id);
}

/// 人工解除整机限流退避（确认是坏链假阳性时用；退避等级一并归零）。
#[tauri::command]
fn clear_cooldown() {
    mpider_core::runstate::cooldown_reset();
    applog::warn(Stage::Sweep, "用户手动解除了整机限流退避".to_string());
}

// ============ 微信号管理 ============

/// 「微信号管理」页的一行（`wx_accounts` + 派生的标定 / 预算 / 限流状态）。
#[derive(Serialize)]
struct WxAccountView {
    id: i64,
    alias: String,
    note: Option<String>,
    /// uin 的短哈希（不含原文），页面上作「标识」。
    uin_hash: Option<String>,
    is_active: bool,
    created_at: f64,
    last_captured_at: Option<f64>,
    /// 种子标定状态：`none` / `ok` / `stale`。
    calibrated: String,
    /// 该号近 24 小时列表请求数与每号预算（0 = 不限）。
    budget_used_24h: i64,
    budget: i64,
    blocked_until: Option<f64>,
    blocked_reason: Option<String>,
    /// `normal` / `budget`（预算用完）/ `blocked`（封号退避中）。
    status: String,
}

/// 把当前激活号同步给 RPA（标定文件按号存）。
fn restore_rpa_active(store: &Store) {
    let id = store.wx_active().ok().flatten().map(|a| a.id);
    mpider_core::rpa::set_active_wx_id(id);
}

/// 「临时把 RPA 标定路径切到某个微信号」的守卫：Drop 时恢复为激活号（任何 `?` 提前返回都会恢复）。
/// 框选流程成功开启后由 `rpa_pick_commit` / `rpa_pick_cancel` 收尾恢复，届时调用 [`Self::defuse`] 放弃自动恢复。
struct RpaTargetGuard<'a> {
    store: &'a Store,
    armed: bool,
}

impl<'a> RpaTargetGuard<'a> {
    fn switch(store: &'a Store, wx_id: Option<i64>) -> Self {
        match wx_id {
            Some(id) => mpider_core::rpa::set_active_wx_id(Some(id)),
            None => restore_rpa_active(store),
        }
        Self { store, armed: true }
    }

    /// 放弃 Drop 时的自动恢复（框选层已开，等提交 / 取消再恢复）。
    fn defuse(mut self) {
        self.armed = false;
    }
}

impl Drop for RpaTargetGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            restore_rpa_active(self.store);
        }
    }
}

fn wx_view(store: &Store, a: mpider_core::model::WxAccount) -> WxAccountView {
    let uin_hash = mpider_core::model::short_hash(&a.uin);
    let (_, budget) = mpider_core::runstate::list_calls_snapshot();
    let used = uin_hash
        .as_deref()
        .and_then(|h| {
            store
                .list_calls_window(Some(h), mpider_core::runstate::LIST_BUDGET_WINDOW_SECS)
                .ok()
        })
        .map(|(n, _)| n)
        .unwrap_or(0);
    let now = mpider_core::model::now();
    let status = if a.blocked_until.is_some_and(|u| u > now) {
        "blocked"
    } else if budget > 0 && used >= budget {
        "budget"
    } else {
        "normal"
    };
    WxAccountView {
        id: a.id,
        alias: a.alias,
        note: a.note,
        uin_hash,
        is_active: a.is_active,
        created_at: a.created_at,
        last_captured_at: a.last_captured_at,
        calibrated: mpider_core::rpa::calibration_mark_for(a.id).to_string(),
        budget_used_24h: used,
        budget,
        blocked_until: a.blocked_until.filter(|u| *u > now),
        blocked_reason: a.blocked_reason,
        status: status.to_string(),
    }
}

/// 全部微信号（激活的排最前）。
#[tauri::command]
fn wx_list(state: State<'_, AppState>) -> Result<Vec<WxAccountView>, String> {
    let list = state.store.wx_list().map_err(|e| e.to_string())?;
    Ok(list.into_iter().map(|a| wx_view(&state.store, a)).collect())
}

/// 当前激活的微信号。
#[tauri::command]
fn wx_active(state: State<'_, AppState>) -> Result<Option<WxAccountView>, String> {
    Ok(state
        .store
        .wx_active()
        .map_err(|e| e.to_string())?
        .map(|a| wx_view(&state.store, a)))
}

/// 改别名 / 备注。
#[tauri::command]
fn wx_update(
    state: State<'_, AppState>,
    id: i64,
    alias: String,
    note: Option<String>,
) -> Result<(), String> {
    state
        .store
        .wx_update(id, &alias, note.as_deref())
        .map_err(|e| e.to_string())
}

/// 删除微信号：激活中且采集在跑时拒绝；顺带删标定文件。
#[tauri::command]
fn wx_delete(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    let a = state
        .store
        .wx_get(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("微信号不存在：#{id}"))?;
    if a.is_active && mpider_core::runstate::run_busy() {
        return Err("该微信号正在采集中（巡检 / 历史抓取），先停止再删除".into());
    }
    state.store.wx_delete(id).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(mpider_core::rpa::click_cache_path_for(id));
    restore_rpa_active(&state.store);
    mpider_core::runstate::sync_active_block(&state.store);
    applog::info(Stage::App, format!("删除微信号『{}』", a.alias));
    Ok(())
}

/// 激活某个微信号：采集在跑时拒绝。切换后标定路径 / 封号退避随之切换。
#[tauri::command]
fn wx_activate(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    if mpider_core::runstate::run_busy() {
        return Err("采集进行中（巡检 / 历史抓取），先停止再切换微信号".into());
    }
    state.store.wx_activate(id).map_err(|e| e.to_string())?;
    restore_rpa_active(&state.store);
    mpider_core::runstate::sync_active_block(&state.store);
    if let Ok(Some(a)) = state.store.wx_get(id) {
        applog::info(Stage::App, format!("已激活微信号『{}』", a.alias));
    }
    Ok(())
}

/// 解封：清掉该号的封号退避；若是激活号，全局退避同步解除。
#[tauri::command]
fn wx_unblock(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    state.store.wx_unblock(id).map_err(|e| e.to_string())?;
    mpider_core::runstate::sync_active_block(&state.store);
    if let Ok(Some(a)) = state.store.wx_get(id) {
        applog::warn(
            Stage::App,
            format!("用户手动解除了微信号『{}』的封号退避", a.alias),
        );
    }
    Ok(())
}

// ============ 任务列表 ============

/// 「任务列表」页的一页（行 + 总数）。
#[derive(Serialize)]
struct JobPage {
    items: Vec<mpider_core::model::JobListItem>,
    total: i64,
}

/// 任务列表：按本地 id 倒序翻页；`kind` = `manual` / `sweep` / 省略（全部）。每行只带计数与状态，
/// 任务数据 / 上报数据正文用 `get_job` 取。
#[tauri::command]
fn list_jobs(
    state: State<'_, AppState>,
    kind: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<JobPage, String> {
    let kind = kind.filter(|k| !k.is_empty());
    let items = state
        .store
        .list_jobs(
            kind.as_deref(),
            limit.unwrap_or(50).clamp(1, 500),
            offset.unwrap_or(0).max(0),
        )
        .map_err(|e| e.to_string())?;
    let total = state
        .store
        .count_jobs(kind.as_deref())
        .map_err(|e| e.to_string())?;
    Ok(JobPage { items, total })
}

/// 一条任务的完整记录：链接 / 最后更新时间 / 采集结果（按号视图与上报状态）/ 起止时刻 / 结果分类。
#[tauri::command]
fn get_job(state: State<'_, AppState>, id: i64) -> Result<Option<mpider_core::JobRow>, String> {
    state.store.get_job(id).map_err(|e| e.to_string())
}

/// 物理删除一条任务（前端已二次确认；进行中的任务后端拒绝）。返回是否真的删掉了。
#[tauri::command]
fn delete_job(state: State<'_, AppState>, id: i64) -> Result<bool, String> {
    let ok = state.store.delete_job(id).map_err(|e| e.to_string())?;
    if ok {
        applog::info(Stage::App, format!("用户删除了任务记录（本地 job#{id}）"));
    }
    Ok(ok)
}

/// 一键清空全部已结束的任务记录（前端已二次确认；进行中的保留）。返回删掉的条数。
#[tauri::command]
fn clear_jobs(state: State<'_, AppState>) -> Result<usize, String> {
    let n = state
        .store
        .clear_finished_jobs()
        .map_err(|e| e.to_string())?;
    applog::info(
        Stage::App,
        format!("用户清空了任务记录：删除 {n} 条已结束任务"),
    );
    Ok(n)
}

// ============ 限流分析 ============

/// 「限流分析」页的统计：最近 `window_secs` 秒（默认 24 小时，600s ~ 30 天）的列表请求频率 / 间隔分布 /
/// 按来源与结果的计数 / 巡检轮次与批次摘要 / 退避记录（聚合逻辑在 `mpider_core::ratelimit`）。
#[tauri::command]
fn ratelimit_stats(
    state: State<'_, AppState>,
    window_secs: Option<i64>,
) -> Result<mpider_core::RateLimitStats, String> {
    mpider_core::ratelimit_stats(&state.store, window_secs.unwrap_or(86_400))
        .map_err(|e| e.to_string())
}

/// 限流分析原始数据导出文件信息。
#[derive(Serialize)]
struct RateLimitExport {
    path: String,
    list_calls: usize,
    run_logs: usize,
    cooldown_logs: usize,
}

/// **导出限流分析原始数据**：把窗口内的 `list_call_log`（含影响因素列）/ `run_log` / `cooldown_log` 三表
/// 原样写成一个 JSON（`app_data_dir/logs/mpider-ratelimit-<时间>.json`），供跑一段时间后离线做
/// 「限流与代理 / 微信号 / 节奏是否相关」的数据分析。不含接口 URL 与凭证原文（表里本来就没有）。
#[tauri::command]
fn ratelimit_export(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    window_secs: Option<i64>,
) -> Result<RateLimitExport, String> {
    let window = window_secs.unwrap_or(30 * 86_400).clamp(600, 30 * 86_400);
    let now = mpider_core::model::now();
    let since = now - window as f64;
    let list_calls = state
        .store
        .list_calls_since(since, mpider_core::store::RATELIMIT_LOG_MAX_ROWS)
        .map_err(|e| e.to_string())?;
    let run_logs = state
        .store
        .run_logs_since(since, 50_000)
        .map_err(|e| e.to_string())?;
    let cooldown_logs = state
        .store
        .cooldown_logs_since(since, 10_000)
        .map_err(|e| e.to_string())?;
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("logs");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建导出目录失败：{e}"))?;
    let stamp = applog::format_local(now)
        .replace([' ', ':'], "-")
        .replace("--", "-");
    let path = dir.join(format!("mpider-ratelimit-{stamp}.json"));
    let payload = serde_json::json!({
        "exported_at": now,
        "window_secs": window,
        "app_version": app.package_info().version.to_string(),
        "platform": std::env::consts::OS,
        "list_calls": list_calls,
        "run_logs": run_logs,
        "cooldown_logs": cooldown_logs,
    });
    let text = serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("写导出文件失败：{e}"))?;
    applog::info(
        Stage::App,
        format!(
            "限流分析原始数据已导出：{}（请求 {} 条 / 执行单元 {} 条 / 退避 {} 条）",
            path.display(),
            list_calls.len(),
            run_logs.len(),
            cooldown_logs.len()
        ),
    );
    Ok(RateLimitExport {
        path: path.display().to_string(),
        list_calls: list_calls.len(),
        run_logs: run_logs.len(),
        cooldown_logs: cooldown_logs.len(),
    })
}

// ============ 飞书通知（mpider_core::notify） ============

/// **应用飞书通知配置**（幂等）：前端应用启动与保存配置时调用。
/// 应用启动不推飞书（只报异常，不报状态）；要确认配置通不通用「发送测试消息」。
#[tauri::command]
fn notify_apply(cfg: RealRunConfig) -> Result<(), String> {
    mpider_core::notify::configure(cfg.feishu_config());
    Ok(())
}

/// **发送测试消息**：用表单里的配置（可未保存）立刻发一条，返回飞书的结论。
#[tauri::command]
async fn notify_test(cfg: RealRunConfig) -> Result<(), String> {
    let fc = cfg.feishu_config();
    mpider_core::notify::send_test(&fc)
        .await
        .map_err(|e| format!("{e:#}"))
}

/// 通知运行状态（累计发送 / 失败 / 最近错误）。
#[tauri::command]
fn notify_status() -> mpider_core::NotifyStatus {
    mpider_core::notify::status()
}

/// 事件名：补详情的结构化进度（`DetailProgress` JSON，前端进度条 + 日志共用）。
const EV_DETAIL_PROGRESS: &str = "detail://progress";

/// 补详情任务进程级互斥：全局/单号/单篇入口共用一个开关，避免并发两批互相打架。
static DETAIL_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// **补采文章详情**：`biz` 限定单号、`article_id` 限定单篇、都不传 = 全局补采。
/// `limit` 不传 = 不设上限，待补多少就抓多少（逐篇处理，速率由间隔/并发控制）。
/// 并发抓公开 /s 页 → article-md 解析 Markdown 入库；进度经 `detail://progress` 事件推送。
#[tauri::command]
async fn run_detail(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    biz: Option<String>,
    article_id: Option<i64>,
    limit: Option<i64>,
    throttle_ms: Option<u64>,
    workers: Option<usize>,
) -> Result<mpider_core::DetailSummary, String> {
    if DETAIL_RUNNING.swap(true, Ordering::SeqCst) {
        return Err("已有一批详情补采在进行中，请等它结束。".to_string());
    }
    let store = state.store.clone();
    let defaults = mpider_core::DetailConfig::default();
    let cfg = mpider_core::DetailConfig {
        biz,
        article_id,
        limit,
        // 系统设置里的「详情抓取间隔 / 详情并发数」；微信限流时调大间隔、调小并发再试。
        // 注意节流是每 worker 的，整体速率 ≈ workers/throttle_ms（见 mpider-core detail.rs）。
        throttle_ms: throttle_ms.unwrap_or(defaults.throttle_ms),
        workers: workers.unwrap_or(defaults.workers),
    };
    let emitter = app.clone();
    let result = mpider_core::collect_detail(store, cfg, move |p| {
        let _ = emitter.emit(EV_DETAIL_PROGRESS, &p);
    })
    .await
    .map_err(|e| e.to_string());
    DETAIL_RUNNING.store(false, Ordering::SeqCst);
    result
}

/// 用系统默认浏览器打开外部链接（WebView 里 target=_blank 不会开外部浏览器）。
/// 只放行 http/https，避免被当成任意命令执行入口。
#[tauri::command]
fn open_external(url: String) -> Result<(), String> {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err(format!("仅支持 http/https 链接：{url}"));
    }
    #[cfg(windows)]
    {
        open_external_windows(&url)
    }
    #[cfg(not(windows))]
    {
        let result = match std::env::consts::OS {
            "macos" => std::process::Command::new("open").arg(&url).spawn(),
            _ => std::process::Command::new("xdg-open").arg(&url).spawn(),
        };
        result
            .map(|_| ())
            .map_err(|e| format!("打开浏览器失败：{e}"))
    }
}

/// Windows：用 `ShellExecuteW` 以默认协议处理器打开 URL。
///
/// 不经 cmd（`&` 会被当命令分隔符）也不经 explorer（带查询串的 URL 会被当成打开资源管理器），
/// URL 里的 `&` / `#` / 查询串原样交给默认浏览器。约定返回值 > 32 表示成功。
#[cfg(windows)]
fn open_external_windows(url: &str) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    // 以 null 结尾的 UTF-16；两个 Vec 在整个调用期间保持存活。
    let op: Vec<u16> = "open\0".encode_utf16().collect();
    let file: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    let hinst = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(op.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if hinst.0 as isize > 32 {
        Ok(())
    } else {
        Err(format!(
            "ShellExecuteW 打开失败 (code={})",
            hinst.0 as isize
        ))
    }
}

/// RPA **自检**：微信内置浏览器自动化环境是否就绪（文件助手窗口 / 种子点位是否已框选标定）。
/// 前端启动即自动调用，用横线 step 呈现各步通过与否。返回 `SelfCheck`（见 mpider-core）。
#[tauri::command]
fn rpa_self_check() -> mpider_core::SelfCheck {
    mpider_core::get_controller(true).self_check()
}

/// 框选层窗口的 label（全屏透明置顶，盖在文件助手上让用户框种子消息）。
const PICK_WINDOW: &str = "seed-pick";
/// 框选完成 / 取消后通知主窗口的事件（载荷：`SeedMark` / 无）。
const EV_SEED_MARKED: &str = "rpa://seed-marked";
const EV_SEED_PICK_CANCELLED: &str = "rpa://seed-pick-cancelled";

/// 关掉框选层（若在）。
fn close_pick_window(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window(PICK_WINDOW) {
        let _ = w.close();
    }
}

/// 「框选前准备」返回给前端的信息：文件助手矩形 + 框选层覆盖的显示器原点/尺寸（框选层据此画提示框、
/// 换算坐标）。
#[derive(Serialize)]
struct PickBegin {
    fh_rect: (i32, i32, i32, i32),
    /// 框选层覆盖的显示器**物理原点**（左上角）。多屏时非 `(0,0)`：前端把 `clientX*dpr` 加上它换算回
    /// 绝对物理坐标，与 mpider-core `GetWindowRect` 同一坐标系。
    origin: (i32, i32),
    /// 框选层覆盖的显示器物理尺寸（宽,高）。
    screen: (i32, i32),
}

/// 选「包含点 `(x,y)`（物理像素）」的显示器；多屏时文件助手可能不在主屏，框选层要盖在它所在的屏上，
/// 否则用户根本框不到它。逐个比对各显示器物理矩形；都不含则退回主显示器。
fn pick_monitor_containing(
    app: &tauri::AppHandle,
    x: i32,
    y: i32,
) -> Result<tauri::Monitor, String> {
    if let Ok(monitors) = app.available_monitors() {
        for m in monitors {
            let p = m.position();
            let s = m.size();
            let (l, t) = (p.x, p.y);
            let (r, b) = (p.x + s.width as i32, p.y + s.height as i32);
            if x >= l && x < r && y >= t && y < b {
                return Ok(m);
            }
        }
    }
    app.primary_monitor()
        .map_err(|e| format!("取主显示器失败：{e}"))?
        .ok_or_else(|| "未找到主显示器".to_string())
}

/// RPA **开始框选种子**：先把「文件传输助手」置顶可见，再在主屏上盖一层全屏透明置顶的框选窗口
/// （加载前端 `index.html?view=pick`）。用户在框选层里拖出一个框住种子链接消息的矩形，
/// 框选层调 [`rpa_pick_commit`] 提交（或 Esc 调 [`rpa_pick_cancel`]）。
///
/// 框选层按**文件助手所在显示器**的物理像素定位与定尺寸（多屏时未必是主屏），并把该显示器物理原点
/// 经 URL 参数 `ox/oy` 传给框选层；前端用 `origin + clientX*devicePixelRatio` 换算回绝对物理坐标，
/// 与 mpider-core 里 `GetWindowRect` 的坐标系一致。
#[tauri::command]
async fn rpa_pick_begin(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    wx_id: Option<i64>,
) -> Result<PickBegin, String> {
    use tauri::{WebviewUrl, WebviewWindowBuilder};

    // 指定了微信号：本次标定落到该号的文件（提交 / 取消后恢复为激活号）；没指定用激活号。
    // 准备阶段任何一步失败都由守卫恢复，避免 RPA 路径停在非激活号上。
    let guard = RpaTargetGuard::switch(&state.store, wx_id);
    let fh_rect =
        tauri::async_runtime::spawn_blocking(|| mpider_core::get_controller(true).prepare_pick())
            .await
            .map_err(|e| format!("框选准备线程异常：{e}"))??;
    // 盖在文件助手所在的显示器上（取其中心判定），多屏时文件助手可能不在主屏。
    let (fl, ft, fr, fb) = fh_rect;
    let monitor = pick_monitor_containing(&app, (fl + fr) / 2, (ft + fb) / 2)?;
    let size = *monitor.size();
    let position = *monitor.position();

    // 上一次的框选层若还在（异常残留），先关掉再建，避免 label 冲突。
    close_pick_window(&app);
    // 把框选层覆盖显示器的物理原点带给前端（多屏时非 0,0），据此把 clientX 换算成绝对物理坐标。
    let url = format!("index.html?view=pick&ox={}&oy={}", position.x, position.y);
    let builder = WebviewWindowBuilder::new(&app, PICK_WINDOW, WebviewUrl::App(url.into()))
        .title("框选种子链接")
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .visible(false);
    // 透明窗口：mac 需要 macos-private-api 特性才暴露 `transparent`，而框选标定只在 Windows 生效
    // （mac 的 prepare_pick 已先行报「不支持」），这里只在非 mac 目标上开透明。
    #[cfg(not(target_os = "macos"))]
    let builder = builder.transparent(true);
    let win = builder
        .build()
        .map_err(|e| format!("创建框选层失败：{e}"))?;
    win.set_position(tauri::Position::Physical(position))
        .map_err(|e| format!("框选层定位失败：{e}"))?;
    win.set_size(tauri::Size::Physical(size))
        .map_err(|e| format!("框选层定尺寸失败：{e}"))?;
    win.show().map_err(|e| format!("显示框选层失败：{e}"))?;
    let _ = win.set_focus();
    guard.defuse();
    Ok(PickBegin {
        fh_rect,
        origin: (position.x, position.y),
        screen: (size.width as i32, size.height as i32),
    })
}

/// RPA **提交框选**：`rect` 为框选层报上来的屏幕物理像素矩形（任意两角）。落盘为相对文件助手
/// 窗口的点位缓存，关掉框选层，并向主窗口广播 `rpa://seed-marked`。失败（如框选中心不在文件助手内）
/// 时**不关**框选层，让用户改框或 Esc 取消。
#[tauri::command]
async fn rpa_pick_commit(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    rect: (i32, i32, i32, i32),
) -> Result<mpider_core::SeedMark, String> {
    let out = tauri::async_runtime::spawn_blocking(move || {
        mpider_core::get_controller(true).mark_seed(rect)
    })
    .await
    .map_err(|e| format!("框选提交线程异常：{e}"));
    let out = match out {
        Ok(Ok(v)) => v,
        Ok(Err(e)) | Err(e) => return Err(e),
    };
    restore_rpa_active(&state.store);
    close_pick_window(&app);
    let _ = app.emit_to("main", EV_SEED_MARKED, &out);
    Ok(out)
}

/// RPA **取消框选**：关掉框选层，通知主窗口复位按钮态。
#[tauri::command]
fn rpa_pick_cancel(app: tauri::AppHandle, state: State<'_, AppState>) {
    restore_rpa_active(&state.store);
    close_pick_window(&app);
    let _ = app.emit_to("main", EV_SEED_PICK_CANCELLED, ());
}

/// RPA **测试点击**：标定后的验证操作。不发种子，只「置顶文件助手 → 按标定点位点击 → 等新内置浏览器窗口」，
/// 回显点位 / 结果。会真的动鼠标、拉起微信内置浏览器（窗口留着不关）。耗时数秒，放 blocking 线程池免卡 UI。
#[tauri::command]
async fn rpa_test_click(
    state: State<'_, AppState>,
    wx_id: Option<i64>,
) -> Result<mpider_core::TestClick, String> {
    let _guard = RpaTargetGuard::switch(&state.store, wx_id);
    tauri::async_runtime::spawn_blocking(|| mpider_core::get_controller(true).test_click())
        .await
        .map_err(|e| format!("测试点击线程异常：{e}"))
}

/// RPA **拦截探针**：真设一次系统代理 → 经 MITM 发一个 HTTPS 请求验证能否拦截 → 立即复位。
/// 用**已安装的那张 CA**（app_data_dir/ca.crt+ca.key），在真跑前把「代理设不上 / 拦不下来」提前暴露。
#[tauri::command]
async fn rpa_proxy_probe(
    app: tauri::AppHandle,
    service: Option<String>,
) -> Result<mpider_core::InterceptProbe, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let cert = dir.join("ca.crt");
    let key = dir.join("ca.key");
    if !(cert.exists() && key.exists()) {
        return Err("尚未生成/安装证书（先到系统设置一键安装证书）".to_string());
    }
    let ca = CaMaterial::from_pem(
        std::fs::read_to_string(&cert).map_err(|e| e.to_string())?,
        std::fs::read_to_string(&key).map_err(|e| e.to_string())?,
    );
    let svc = service.unwrap_or_else(|| "Wi-Fi".to_string());
    Ok(mpider_core::probe_intercept(ca, true, &svc).await)
}

// ============ 环节日志：查询 / 导出 / 一键上报 ============

/// 查询环节日志（入库的关键节点，最近 3 天）。`since_secs`：只看最近多少秒（如 86400 = 近 1 天）；
/// `stage`：环节 code；`min_level`：`warn` / `error`；`limit` 默认 2000。时间升序。
#[tauri::command]
fn list_logs(
    state: State<'_, AppState>,
    since_secs: Option<f64>,
    stage: Option<String>,
    min_level: Option<String>,
    limit: Option<i64>,
) -> Result<Vec<AppLogRow>, String> {
    let since = since_secs
        .filter(|s| *s > 0.0)
        .map(|s| mpider_core::model::now() - s);
    state
        .store
        .list_logs(
            since,
            stage.as_deref().filter(|s| !s.is_empty()),
            min_level.as_deref().filter(|s| !s.is_empty()),
            limit.unwrap_or(2000).clamp(1, 20_000),
        )
        .map_err(|e| e.to_string())
}

/// 日志概况（日志页头部展示）。
#[derive(Serialize)]
struct LogStats {
    /// 库里现存的日志行数。
    count: i64,
    /// 保留天数（固定 3）。
    retention_days: i64,
}

#[tauri::command]
fn log_stats(state: State<'_, AppState>) -> Result<LogStats, String> {
    Ok(LogStats {
        count: state.store.count_logs().map_err(|e| e.to_string())?,
        retention_days: mpider_core::store::LOG_RETENTION_SECS / 86_400,
    })
}

/// 清空库里的环节日志（不可恢复）。返回删除行数。
#[tauri::command]
fn clear_logs(state: State<'_, AppState>) -> Result<usize, String> {
    let n = state.store.clear_logs().map_err(|e| e.to_string())?;
    applog::info(Stage::App, format!("已清空环节日志（{n} 行）"));
    Ok(n)
}

/// 本机主机名（导出日志时标明来自哪台机）。
fn hostname() -> String {
    for key in ["COMPUTERNAME", "HOSTNAME"] {
        if let Ok(v) = std::env::var(key) {
            if !v.trim().is_empty() {
                return v.trim().to_string();
            }
        }
    }
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// 日志导出载荷（近 3 天全部环节日志 + 机器信息）。
struct LogExportPayload {
    app_version: String,
    platform: String,
    arch: String,
    hostname: String,
    exported_at: f64,
    count: usize,
    logs: Vec<AppLogRow>,
}

/// 读库里全部（近 3 天）日志 + 组装导出载荷。
fn build_log_payload(state: &AppState) -> Result<LogExportPayload, String> {
    let logs = state
        .store
        .list_logs(None, None, None, 20_000)
        .map_err(|e| e.to_string())?;
    Ok(LogExportPayload {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        platform: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        hostname: hostname(),
        exported_at: mpider_core::model::now(),
        count: logs.len(),
        logs,
    })
}

/// 导出结果：文件路径 + 行数。
#[derive(Serialize)]
struct LogExport {
    path: String,
    count: usize,
}

/// **导出日志文件**：把近 3 天的环节日志写成文本（`app_data_dir/logs/mpider-log-<时间>.txt`），
/// 返回路径，便于排查时手动发给分析的人。
#[tauri::command]
fn export_logs(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<LogExport, String> {
    let payload = build_log_payload(&state)?;
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("logs");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建日志目录失败：{e}"))?;
    let stamp = applog::format_local(payload.exported_at)
        .replace([' ', ':'], "-")
        .replace("--", "-");
    let path = dir.join(format!("mpider-log-{stamp}.txt"));
    let mut text = String::new();
    text.push_str(&format!(
        "# mpider 环节日志导出\n# 版本 v{}  平台 {}/{}  主机 {}  导出时间 {}  共 {} 行（近 {} 天）\n\n",
        payload.app_version,
        payload.platform,
        payload.arch,
        payload.hostname,
        applog::format_local(payload.exported_at),
        payload.count,
        mpider_core::store::LOG_RETENTION_SECS / 86_400
    ));
    for r in &payload.logs {
        text.push_str(&format!(
            "{} [{}][{}]{} {}\n",
            applog::format_local(r.ts),
            r.level.to_ascii_uppercase(),
            mpider_core::applog::Stage::parse(&r.stage).label(),
            r.job_id.map(|j| format!("[job#{j}]")).unwrap_or_default(),
            r.message
        ));
    }
    std::fs::write(&path, text).map_err(|e| format!("写日志文件失败：{e}"))?;
    applog::info(
        Stage::App,
        format!("日志已导出：{}（{} 行）", path.display(), payload.count),
    );
    Ok(LogExport {
        path: path.display().to_string(),
        count: payload.count,
    })
}

/// 运行配置导出结果：`text` 是导出的 JSON 文本（复制到剪贴板用），`path` 只在写了文件时有值。
#[derive(Serialize)]
struct ConfigExport {
    path: Option<String>,
    text: String,
}

/// **导出运行配置**：把 GUI 传来的运行配置（前端 localStorage 里那份 `RealRunConfig`）包一层信封
/// `{kind, version, exported_at, app_version, platform, config}` 写成 JSON；`to_file` 为真时落到
/// `app_data_dir/exports/mpider-config-<时间>.json` 并返回路径，否则只返回文本供复制到剪贴板。
/// 文本里含上报 token / 飞书 Webhook 与密钥原文（迁移到别的机器要用），**这里不写日志内容**，日志只记路径。
#[tauri::command]
fn export_config(
    app: tauri::AppHandle,
    cfg: serde_json::Value,
    to_file: bool,
) -> Result<ConfigExport, String> {
    let now = mpider_core::model::now();
    let payload = serde_json::json!({
        "kind": "mpider-config",
        "version": 1,
        "exported_at": applog::format_local(now),
        "app_version": app.package_info().version.to_string(),
        "platform": std::env::consts::OS,
        "config": cfg,
    });
    let text = serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?;
    if !to_file {
        return Ok(ConfigExport { path: None, text });
    }
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("exports");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建导出目录失败：{e}"))?;
    let stamp = applog::format_local(now)
        .replace([' ', ':'], "-")
        .replace("--", "-");
    let path = dir.join(format!("mpider-config-{stamp}.json"));
    std::fs::write(&path, &text).map_err(|e| format!("写导出文件失败：{e}"))?;
    applog::info(Stage::App, format!("运行配置已导出：{}", path.display()));
    Ok(ConfigExport {
        path: Some(path.display().to_string()),
        text,
    })
}

/// 在访达 / 资源管理器里定位一个文件（导出日志后「打开所在文件夹」）。
#[tauri::command]
fn reveal_in_folder(path: String) -> Result<(), String> {
    let p = Path::new(&path);
    if !p.exists() {
        return Err(format!("文件不存在：{path}"));
    }
    let res = match std::env::consts::OS {
        "macos" => std::process::Command::new("open")
            .args(["-R", &path])
            .spawn(),
        "windows" => std::process::Command::new("explorer")
            .arg(format!("/select,{path}"))
            .spawn(),
        _ => std::process::Command::new("xdg-open")
            .arg(
                p.parent()
                    .map(|d| d.display().to_string())
                    .unwrap_or(path.clone()),
            )
            .spawn(),
    };
    res.map(|_| ()).map_err(|e| format!("打开文件夹失败：{e}"))
}

/// 打开核心存储：优先落在应用数据目录下的 `mpider.db`；失败则退化为内存库，保证前端可用。
fn open_store(app: &tauri::AppHandle) -> (Store, String) {
    match app.path().app_data_dir() {
        Ok(dir) => {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                eprintln!("[store] 创建数据目录 {dir:?} 失败：{e}，退化内存库");
            } else {
                let path = dir.join("mpider.db");
                match Store::open(&path) {
                    Ok(s) => return (s, path.display().to_string()),
                    Err(e) => eprintln!("[store] 打开 {path:?} 失败：{e}，退化内存库"),
                }
            }
        }
        Err(e) => eprintln!("[store] 取 app_data_dir 失败：{e}，退化内存库"),
    }
    (
        Store::open_in_memory().expect("in-memory store 应可用"),
        ":memory:".to_string(),
    )
}

/// §9 系统托盘骨架（best-effort）：托盘菜单「显示窗口 / 退出」。
///
/// 无默认窗口图标时跳过（不阻塞启动）。关闭到托盘的完整逻辑见 `run()` 里的窗口事件 TODO。
fn setup_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::TrayIconBuilder;

    let show = MenuItem::with_id(app, "show", "显示窗口", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    let mut builder = TrayIconBuilder::new()
        .menu(&menu)
        .tooltip("mpider")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
            "quit" => app.exit(0),
            _ => {}
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// 应用入口（供 `main.rs` 调用；放在 lib 便于将来接 mobile 目标）。
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // 打开核心存储并注入为共享状态。
            let (store, db_path) = open_store(app.handle());
            let store = Arc::new(store);
            // 环节日志：入库到同一个库（留 3 天），并把每条事件推给前端（订阅句柄随 AppState 同寿命）。
            applog::bus().set_store(Some(store.clone()));
            // 整机退避状态持久化在库里：上次被封 / 限流的倒计时重启后接着算，不会一重启就能继续打。
            mpider_core::runstate::cooldown_restore_logged(&store);
            // 上次退出时还在跑的历史抓取转为暂停：重启后不自动抢微信窗口，由用户点「继续」。
            if let Err(e) = mpider_core::history::pause_on_restart(&store) {
                eprintln!("[history] 重启暂停失败：{e}");
            }
            let emitter = app.handle().clone();
            let log_sub = applog::bus().subscribe(Arc::new(move |ev: &LogEvent| {
                let _ = emitter.emit(EV_LOG, ev);
            }));
            app.manage(AppState {
                store,
                db_path: db_path.clone(),
                loop_running: Arc::new(AtomicBool::new(false)),
                loop_stop: Arc::new(AtomicBool::new(false)),
                _log_sub: log_sub,
            });
            applog::info(
                Stage::App,
                format!(
                    "应用启动 v{}（{}/{}），数据库：{db_path}",
                    env!("CARGO_PKG_VERSION"),
                    std::env::consts::OS,
                    std::env::consts::ARCH
                ),
            );

            // RPA 落盘目录（种子点位缓存 rpa_click_cache.json）指到 app 数据目录
            // （rpa_pick_commit 存、self_check/open_seed/test_click 读）。
            // 启动早期、单线程阶段设环境变量（edition 2021，安全）。
            if let Ok(dir) = app.path().app_data_dir() {
                std::env::set_var("MP_RPA_DATA_DIR", &dir);
            }
            // 多微信号：标定文件按号存，老的单文件迁到激活号名下；封号退避按激活号对齐。
            {
                let st = app.state::<AppState>();
                if let Ok(Some(a)) = st.store.wx_active() {
                    mpider_core::rpa::migrate_legacy_click_cache(a.id);
                }
                restore_rpa_active(&st.store);
                mpider_core::runstate::sync_active_block(&st.store);
            }

            // §9 系统托盘骨架（失败不阻塞启动）。
            if let Err(e) = setup_tray(app.handle()) {
                eprintln!("[tray] 初始化跳过：{e}");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            core_health,
            list_accounts,
            get_credential,
            capture_status,
            capture_manual_start,
            capture_manual_stop,
            cert_status,
            install_cert,
            uninstall_cert,
            system_status,
            sysproxy_reset_stale,
            sweep_start,
            sweep_stop,
            loop_status,
            seed_bootstrap_url,
            seed_server_apply,
            seed_server_status,
            list_articles,
            get_article_detail,
            article_counts,
            run_detail,
            delete_account,
            delete_article,
            sweep_status,
            set_account_sweep_enabled,
            sweep_restart_pass,
            sweep_retry_account,
            add_links,
            run_status,
            clear_cooldown,
            list_jobs,
            get_job,
            delete_job,
            clear_jobs,
            ratelimit_stats,
            ratelimit_export,
            notify_apply,
            notify_test,
            notify_status,
            open_external,
            rpa_self_check,
            rpa_pick_begin,
            rpa_pick_commit,
            rpa_pick_cancel,
            rpa_test_click,
            rpa_proxy_probe,
            wx_list,
            wx_active,
            wx_update,
            wx_delete,
            wx_activate,
            wx_unblock,
            history_start,
            history_pause,
            history_resume,
            history_cancel,
            history_status,
            history_estimate,
            alert_latest,
            alert_ack,
            list_logs,
            log_stats,
            clear_logs,
            export_logs,
            export_config,
            reveal_in_folder
        ])
        .build(tauri::generate_context!())
        .expect("构建 mpider Tauri 应用出错")
        .run(|_app, event| {
            // 退出兜底：⌘Q / 关窗退出 / 托盘「退出」都走进程退出，主循环里持有的系统代理守卫来不及析构，
            // 会把全机代理留在一个已无人监听的端口上（浏览器断网）。这里把仍激活的守卫当场复位（幂等）。
            if matches!(
                event,
                tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
            ) {
                let n = mpider_core::sysproxy::force_restore_all();
                if n > 0 {
                    eprintln!("[exit] 已复位 {n} 个仍激活的系统代理设置");
                }
            }
        });
}
