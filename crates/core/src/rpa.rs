//! RPA：自动打开/关闭微信内置浏览器（服务层，跨平台；对齐 Python `rpa.py`）。
//!
//! 每次任务只需最省事的一步：把**种子链接**送进微信内置浏览器（让 WebView 发起 `/s`，
//! 抓凭证代理据此抓 key/uin/token，注入接力脚本再自动接力打开本批其余链接），抓完**关浏览器**
//! 收尾成闭环，下一批重复。
//!
//! **种子点位由人工标定**（2026-09-04 起，不再做截图 / 模板匹配 / 绿气泡识别 / 自校准）：
//! 用户在 GUI 点「框选种子」，App 弹出全屏框选层，用户在「文件传输助手」窗口里框住种子链接那条消息；
//! 框选中心相对文件助手窗口左上角的偏移落盘为 [`ClickCache`]。之后每次 `open_seed` 只做
//! 「置顶文件助手 → 按缓存点位点击 → 等新内置浏览器窗口」。
//! 窗口尺寸 / 屏幕分辨率变了视为标定失效，需重新框选。
//!
//! **不再 Ctrl+F5 硬刷**（2026-09-08 起）：硬刷是种子页还在外网域名、要经 MITM 注入那时候的遗留（命中
//! WebView 缓存代理就看不到请求）。种子页改由本机服务直出 + `no-store` 后这个理由不成立，而且硬刷落在
//! 窗口出现 2.5s 之后——那时页面早已跳到首条文章，等于把文章页白白重载一次。文章页自身的缓存改由 MITM
//! 给 `/s` 响应加 `no-store` 兜住。保留 [`set_hard_refresh`] 开关（默认关）供回退验证。
//!
//! **Windows 走纯 Rust 全原生**：windows-rs `EnumWindows` 找窗口、`SetForegroundWindow` 置顶
//! （点击前**必须**保证文件助手置顶可见，否则点击只会被当成激活窗口吞掉）、`SendInput` 注入
//! 「移动→按下→抬起」拉起内置浏览器（内置浏览器是 CEF，须走注入输入流才登记指针命中，见 `click_at`）、
//! 枚举 `WeChatAppEx.exe` 顶层窗口发 `WM_CLOSE` 关浏览器。整块编进 Tauri 单一二进制。
//!
//! 实现都"尽力而为 + 永不 panic 打断编排"：
//! - Windows：见下方 `win_engine`。产品里 Tauri 进程本就跑在交互桌面（session 1），直接进程内调用，
//!   无需 `schtasks` 注入（那只是脱离桌面用 SSH 驱动时的测试脚手架）。
//! - macOS：**人工模式**（[`MacWeChatController`]，2026-09-10 起明确为产品路径而非退化）：不做窗口枚举 /
//!   模拟点击，`osascript` 激活微信后提示用户在微信内置浏览器**打开任意一篇公众号文章**即可——代理层对经过它
//!   的每个文章页都注入接力脚本（`capture.rs::relay_response` 不要求是种子页），打开一篇就能自动接力本批其余链接，
//!   凭证照常入库。种子链接与框选在 mac 上都不需要；编排层按 [`WeChatController::manual`] 把等待上限抬到
//!   [`MANUAL_WAIT_SECS`] 并推全局提醒（`runstate::alert_push("manual_open")`）让人来得及操作。
//! - 兜底 / `rpa_enabled=false`：[`NoOpController`]，同为人工模式，放剪贴板并提示人工打开。

use std::sync::atomic::{AtomicBool, Ordering};

use tracing::debug;

/// 拉起内置浏览器后是否再发一次 Ctrl+F5 硬刷（进程级开关，默认**关**；对应运行配置 `rpa_hard_refresh`）。
static HARD_REFRESH: AtomicBool = AtomicBool::new(false);

/// 设置「拉起后硬刷」开关（`runner::build_orchestrator` / Tauri 应用配置时调）。默认关，见模块文档。
pub fn set_hard_refresh(on: bool) {
    HARD_REFRESH.store(on, Ordering::Relaxed);
}

/// 「拉起后硬刷」开关当前值。
pub fn hard_refresh_enabled() -> bool {
    HARD_REFRESH.load(Ordering::Relaxed)
}

/// 当前平台微信内置浏览器（Chromium 内核）的**硬刷新**快捷键说明，用于人工模式的提示文案。
///
/// 快捷键随平台不同：Windows 是 `Ctrl+F5`（`Ctrl+Shift+R` 亦可），macOS 是 `⌘⇧R`
/// （`Cmd+Shift+R`；mac 上 `Ctrl+F5` 无效）。2026-09-08 起硬刷不再是必做步骤（见模块文档），
/// 只在 `rpa_hard_refresh` 开关打开时才出现在人工模式提示里。
pub fn hard_refresh_hint() -> &'static str {
    if cfg!(target_os = "windows") {
        "Ctrl+F5（或 Ctrl+Shift+R）"
    } else if cfg!(target_os = "macos") {
        "⌘⇧R（Cmd+Shift+R）"
    } else {
        "Ctrl+Shift+R"
    }
}

/// 人工模式（mac / 关闭 RPA）下「等凭证」的**等待下限**（秒）：机器点种子 2 分钟够用，人要收到提醒、切到微信、
/// 找一篇文章点开，`capture_wait_seconds` 默认的 120s 常常来不及。编排 / 续期在 [`WeChatController::manual`]
/// 为真时取 `max(配置值, 本值)`；凭证到位即返回，不会白等满。
pub const MANUAL_WAIT_SECS: u64 = 600;

/// 人工模式的全局提醒种类（`runstate::alert_push` 的 `kind`，GUI 横幅据此显示「请打开一篇文章」）。
pub const ALERT_KIND_MANUAL_OPEN: &str = "manual_open";

/// 人工模式下推一条全局提醒，告诉用户现在该在微信里打开一篇文章；返回提醒 id（去重命中时 `None`），
/// 凭证到位 / 等待结束后调 `runstate::alert_ack` 收掉。`targets` 为本批目标号的显示名（可为空：短链批预知不了号）。
pub fn manual_open_alert(targets: &[String], wait_secs: u64) -> Option<i64> {
    let who = if targets.is_empty() {
        "任意一篇公众号文章".to_string()
    } else {
        format!(
            "任意一篇公众号文章（本批目标：{}）",
            targets
                .iter()
                .map(|t| format!("「{t}」"))
                .collect::<Vec<_>>()
                .join("")
        )
    };
    crate::runstate::alert_push(
        ALERT_KIND_MANUAL_OPEN,
        format!(
            "请在微信内置浏览器里打开{who}：打开一篇即可自动接力本批其余链接并抓到凭证，之后浏览器会停在待命页，别关它，后续批次自动接力；{} 分钟内未打开本批将跳过、稍后重试。",
            wait_secs.div_ceil(60)
        ),
    )
}

/// 人工模式下「不支持自动点击」类功能的提示：mac 说明人工模式怎么用，其它平台只说不支持。
fn unsupported_note(what: &str) -> String {
    if cfg!(target_os = "macos") {
        format!(
            "mac 为人工模式，无需{what}：运行时在微信内置浏览器打开任意一篇公众号文章即可触发抓取（不需要种子链接与框选）"
        )
    } else {
        format!("当前平台不支持{what}")
    }
}

/// 把文本放系统剪贴板（best-effort：任何失败都返回 false，绝不 panic）。
pub fn copy_to_clipboard(text: &str) -> bool {
    match arboard::Clipboard::new().and_then(|mut c| c.set_text(text.to_string())) {
        Ok(()) => true,
        Err(e) => {
            debug!(error = %e, "clipboard set_text failed");
            false
        }
    }
}

/// 屏幕矩形 `(left, top, right, bottom)`，物理像素，右/下为独占上界。
pub type Rect = (i32, i32, i32, i32);

/// 种子点位标定状态（[`SelfCheck::mark`]）。
pub const MARK_NONE: &str = "none";
pub const MARK_OK: &str = "ok";
pub const MARK_STALE: &str = "stale";

/// 运行前自检结果（可 serde 给 GUI 展示）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SelfCheck {
    /// 是否就绪（文件助手窗口在 + 种子点位已标定且仍有效）。
    pub ready: bool,
    /// 是否找到「文件传输助手」独立窗口。
    pub fh_found: bool,
    /// 文件助手窗口矩形（物理像素）。
    pub fh_rect: Option<Rect>,
    /// 主屏物理分辨率（宽,高）。
    pub screen: Option<(i32, i32)>,
    /// 文件助手窗口所在显示器的**显示缩放百分比**（如 100 / 125 / 150；`GetDpiForWindow`=dpi*100/96）。
    /// 坐标换算与点击**全程走物理像素**，不受缩放影响；此值仅供诊断「分辨率/缩放」类点击落空。
    /// `None` = 取不到（非 Windows / 老系统）。
    pub scale_pct: Option<u32>,
    /// 种子点位标定状态：[`MARK_NONE`]（未标定）/ [`MARK_OK`]（有效）/ [`MARK_STALE`]（窗口尺寸或分辨率变了，已失效）。
    pub mark: String,
    /// 标定点位按当前窗口换算后的绝对坐标；`None` = 未标定或已失效。
    pub cached_point: Option<(i32, i32)>,
    /// 标定时框选的矩形按当前窗口换算后的绝对坐标；`None` 同上。
    pub cached_rect: Option<Rect>,
    /// 人类可读诊断。
    pub message: String,
}

impl SelfCheck {
    fn unsupported() -> Self {
        SelfCheck {
            ready: false,
            fh_found: false,
            fh_rect: None,
            screen: None,
            scale_pct: None,
            mark: MARK_NONE.to_string(),
            cached_point: None,
            cached_rect: None,
            message: unsupported_note("自检"),
        }
    }
}

/// 「测试点击」结果：不发种子、只按标定点位点一次文件助手里的种子链接，
/// 把点位 / 是否点击 / 是否拉起浏览器全部回显，供人工核对标定是否正确。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TestClick {
    /// 是否点击后检测到新的内置浏览器窗口（= 自动点击链路整体成功）。
    pub ok: bool,
    /// 是否找到「文件传输助手」窗口。
    pub fh_found: bool,
    /// 文件助手窗口矩形（物理像素）。
    pub fh_rect: Option<Rect>,
    /// 主屏物理分辨率（宽,高）。
    pub screen: Option<(i32, i32)>,
    /// 文件助手窗口所在显示器的显示缩放百分比（诊断分辨率/缩放问题；点击坐标始终走物理像素）。
    pub scale_pct: Option<u32>,
    /// 本次点击的点位（物理像素）；`None` = 未标定 / 标定失效，没点。
    pub point: Option<(i32, i32)>,
    /// 是否真的执行了点击。
    pub clicked: bool,
    /// 点击后新出现的内置浏览器窗口标题。
    pub opened: Vec<String>,
    /// 人类可读结论。
    pub message: String,
}

impl TestClick {
    fn unsupported() -> Self {
        TestClick {
            ok: false,
            fh_found: false,
            fh_rect: None,
            screen: None,
            scale_pct: None,
            point: None,
            clicked: false,
            opened: Vec::new(),
            message: unsupported_note("测试点击"),
        }
    }
}

/// 「框选种子」提交结果：标定已落盘。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SeedMark {
    /// 标定点位（框选中心，绝对物理像素）。
    pub point: (i32, i32),
    /// 框选矩形（绝对物理像素，已规范化为左上→右下）。
    pub rect: Rect,
    /// 标定时的文件助手窗口矩形。
    pub fh_rect: Rect,
    /// 缓存文件路径。
    pub cache_path: String,
    /// 人类可读结论。
    pub message: String,
}

/// **种子点位缓存**：「框选种子」提交后落盘；`open_seed` / 「测试点击」直接按它点。
///
/// 存的是**相对文件助手窗口左上角的偏移**而不是绝对坐标：窗口挪了位置照样能用；
/// 窗口尺寸或屏幕分辨率变了（换 DPI / 拉伸窗口，消息会重排）则视为失效，需重新框选。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ClickCache {
    /// 点位（框选中心）相对文件助手窗口左上角的偏移 `(dx, dy)`（物理像素）。
    pub offset: (i32, i32),
    /// 框选矩形相对文件助手窗口左上角的偏移 `(x0, y0, x1, y1)`。
    pub rect: Rect,
    /// 标定时文件助手窗口尺寸 `(宽, 高)`；不一致即失效。
    pub fh_size: (i32, i32),
    /// 标定时主屏物理分辨率 `(宽, 高)`；不一致即失效。
    pub screen: (i32, i32),
    /// 标定时间（unix 秒）。
    pub saved_at: u64,
}

/// 把任意两角给出的矩形规范化为 `(left, top, right, bottom)`。
pub fn normalize_rect(r: Rect) -> Rect {
    let (x0, y0, x1, y1) = r;
    (x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1))
}

/// 矩形中心。
pub fn rect_center(r: Rect) -> (i32, i32) {
    let (x0, y0, x1, y1) = normalize_rect(r);
    ((x0 + x1) / 2, (y0 + y1) / 2)
}

/// 时间种子的轻量随机数（splitmix64；非加密，仅用于点击点抖动，够用，不引 rand 依赖）。
/// 点击间隔通常数秒，纳秒时间种子足以每次不同。
fn rand_u64() -> u64 {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// 在框选矩形**内随机取一点**（物理像素绝对坐标）——测试点击 / 正式点击都用它，避免每次点固定中心
/// 落在链接文字行外。四边内缩 [`RECT_INSET`] 像素避免贴边（框太小则不缩）。一次 `rand_u64` 拆高/低位
/// 分别定 x、y，避免相邻两次调用时间种子相近导致相关。
pub fn random_point_in_rect(rect: Rect) -> (i32, i32) {
    const RECT_INSET: i32 = 2;
    let (x0, y0, x1, y1) = normalize_rect(rect);
    let inset = |lo: i32, hi: i32| -> (i32, i32) {
        if hi - lo > 2 * RECT_INSET {
            (lo + RECT_INSET, hi - RECT_INSET)
        } else {
            (lo, hi)
        }
    };
    let (ax0, ax1) = inset(x0, x1);
    let (ay0, ay1) = inset(y0, y1);
    let r = rand_u64();
    let pick = |lo: i32, hi: i32, bits: u64| -> i32 {
        let span = (hi - lo).max(0) as u64;
        lo + (bits % (span + 1)) as i32
    };
    (pick(ax0, ax1, r >> 32), pick(ay0, ay1, r & 0xFFFF_FFFF))
}

impl ClickCache {
    /// 由一次**框选**的绝对矩形 + 当时的文件助手窗口矩形 / 屏幕尺寸构造。
    pub fn from_rect(rect: Rect, fh_rect: Rect, screen: (i32, i32)) -> Self {
        let (x0, y0, x1, y1) = normalize_rect(rect);
        let (cx, cy) = rect_center(rect);
        let (l, t, r, b) = fh_rect;
        ClickCache {
            offset: (cx - l, cy - t),
            rect: (x0 - l, y0 - t, x1 - l, y1 - t),
            fh_size: (r - l, b - t),
            screen,
            saved_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }
    }

    /// 是否仍适用于**当前**窗口尺寸 / 屏幕分辨率。
    pub fn is_valid_for(&self, fh_rect: Rect, screen: (i32, i32)) -> bool {
        let (l, t, r, b) = fh_rect;
        (r - l, b - t) == self.fh_size && screen == self.screen
    }

    /// 按**当前**窗口矩形 / 屏幕尺寸换算出绝对 `(点位, 框选矩形)`；
    /// 窗口尺寸或分辨率变了、或点位落到窗口外，返回 `None`（标定失效）。
    pub fn resolve(&self, fh_rect: Rect, screen: (i32, i32)) -> Option<((i32, i32), Rect)> {
        if !self.is_valid_for(fh_rect, screen) {
            return None;
        }
        let (l, t, r, b) = fh_rect;
        let (x, y) = (l + self.offset.0, t + self.offset.1);
        if !(x >= l && x < r && y >= t && y < b) {
            return None;
        }
        let (rx0, ry0, rx1, ry1) = self.rect;
        Some(((x, y), (l + rx0, t + ry0, l + rx1, t + ry1)))
    }

    /// 读缓存文件；不存在 / 损坏（含旧版视觉定位时代的格式）都当没有（best-effort）。
    pub fn load(path: &std::path::Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        match serde_json::from_str::<Self>(&text) {
            Ok(c) => Some(c),
            Err(e) => {
                debug!(path = %path.display(), error = %e, "点位缓存解析失败，忽略（需重新框选）");
                None
            }
        }
    }

    /// 写缓存文件（覆盖），自动建目录。
    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

/// RPA 落盘目录（点位缓存所在）：`MP_RPA_DATA_DIR` 环境变量优先（产品由 Tauri 指到 app 数据目录），
/// 否则 `data/`。
pub fn rpa_data_dir() -> std::path::PathBuf {
    std::env::var_os("MP_RPA_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("data"))
}

/// 当前用于标定的微信号 id（进程级）：种子点位按微信号各存一份。`None` = 未设置（沿用旧的单文件名）。
static ACTIVE_WX_ID: std::sync::Mutex<Option<i64>> = std::sync::Mutex::new(None);

/// 设置当前用于标定 / 点击的微信号 id（激活号切换、框选指定号时调用）。
pub fn set_active_wx_id(id: Option<i64>) {
    *ACTIVE_WX_ID.lock().unwrap() = id;
}

/// 当前用于标定 / 点击的微信号 id。
pub fn active_wx_id() -> Option<i64> {
    *ACTIVE_WX_ID.lock().unwrap()
}

/// 某个微信号的种子点位缓存文件：`<RPA 数据目录>/rpa_click_cache_<id>.json`。
pub fn click_cache_path_for(id: i64) -> std::path::PathBuf {
    rpa_data_dir().join(format!("rpa_click_cache_{id}.json"))
}

/// 种子点位缓存文件：有当前微信号时按号 [`click_cache_path_for`]，否则旧的 `<RPA 数据目录>/rpa_click_cache.json`。
pub fn click_cache_path() -> std::path::PathBuf {
    match active_wx_id() {
        Some(id) => click_cache_path_for(id),
        None => rpa_data_dir().join("rpa_click_cache.json"),
    }
}

/// 老版本单文件标定 `rpa_click_cache.json` 迁到某个号名下（该号还没有标定文件时改名；幂等）。
pub fn migrate_legacy_click_cache(id: i64) {
    let legacy = rpa_data_dir().join("rpa_click_cache.json");
    let target = click_cache_path_for(id);
    if legacy.exists() && !target.exists() {
        if let Err(e) = std::fs::rename(&legacy, &target) {
            debug!(error = %e, "旧标定文件迁移失败，忽略");
        }
    }
}

/// 微信内置浏览器控制器（对齐 Python `WeChatController` Protocol）。
///
/// 方法同步、返回 `(成功?, 人类可读消息)`；实现内部只做 best-effort，不抛错打断编排。
pub trait WeChatController: Send + Sync {
    /// 把种子链接送进微信内置浏览器（触发抓凭证）。
    fn open_seed(&self, url: &str) -> (bool, String);
    /// 收尾关闭内置浏览器窗口。
    fn close_browser(&self) -> (bool, String);
    /// 是否**人工模式**：本控制器不会自动点开任何东西，`open_seed` 只是提示，得靠人在微信里打开一篇文章。
    /// 编排 / 续期据此把等待上限抬到 [`MANUAL_WAIT_SECS`] 并推 [`manual_open_alert`]；看门狗本就只在
    /// `open_seed` 成功时工作，不受影响。默认 `false`（能自动点的实现不用改）。
    fn manual(&self) -> bool {
        false
    }
    /// 运行前**自检**：环境是否就绪（文件助手窗口 / 种子点位标定）。默认「不支持」。
    fn self_check(&self) -> SelfCheck {
        SelfCheck::unsupported()
    }
    /// **框选前准备**：把文件助手窗口置顶可见（框选层要盖在它上面让人看着框），返回其当前矩形。
    /// 默认「不支持」。
    fn prepare_pick(&self) -> Result<Rect, String> {
        Err(unsupported_note("框选标定"))
    }
    /// **提交框选**：`rect` 为用户在屏幕上框出的矩形（绝对物理像素，任意两角），换算成相对文件助手
    /// 窗口的偏移并落盘缓存。框选中心不在文件助手窗口内则拒绝。默认「不支持」。
    fn mark_seed(&self, rect: Rect) -> Result<SeedMark, String> {
        let _ = rect;
        Err(unsupported_note("框选标定"))
    }
    /// **测试点击**：不发种子，按标定点位点一次种子链接，回显是否拉起浏览器。默认「不支持」。
    fn test_click(&self) -> TestClick {
        TestClick::unsupported()
    }
}

/// 人工模式提示的硬刷后缀：开关开着才提示按快捷键，默认为空（本机种子页 no-store，无需硬刷）。
fn manual_refresh_suffix() -> String {
    if hard_refresh_enabled() {
        format!("，打开后按 {} 硬刷新一次", hard_refresh_hint())
    } else {
        String::new()
    }
}

/// 兜底控制器：不自动操作，只把链接放剪贴板并提示人工打开。
pub struct NoOpController;

impl WeChatController for NoOpController {
    fn open_seed(&self, url: &str) -> (bool, String) {
        let copied = copy_to_clipboard(url);
        let tip = if copied {
            "（链接已复制到剪贴板）"
        } else {
            ""
        };
        (
            false,
            format!(
                "人工模式：请在微信内置浏览器打开任意一篇公众号文章以触发抓取（打开一篇即自动接力其余）{tip}{}；种子链接：{url}",
                manual_refresh_suffix()
            ),
        )
    }
    fn close_browser(&self) -> (bool, String) {
        (
            true,
            "（未启用 RPA，无需自动关闭；如已打开可手动关闭内置浏览器）".to_string(),
        )
    }
    fn manual(&self) -> bool {
        true
    }
}

/// macOS：**人工模式**。`osascript` 激活微信，提示用户在内置浏览器打开任意一篇公众号文章。
/// 不枚举窗口、不模拟点击，也**不碰剪贴板**（GUI 在 mac 整体隐藏了种子链接，覆盖用户剪贴板只添乱）。
pub struct MacWeChatController;

impl WeChatController for MacWeChatController {
    fn open_seed(&self, _url: &str) -> (bool, String) {
        // 激活微信（best-effort，失败忽略）。
        let activated = std::process::Command::new("osascript")
            .args(["-e", "tell application \"WeChat\" to activate"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        let tip = if activated {
            "已切到微信"
        } else {
            "请自行切到微信"
        };
        (
            false,
            format!(
                "人工模式：请在微信内置浏览器打开任意一篇公众号文章以触发抓取（打开一篇即自动接力其余，之后浏览器停在待命页、后续批次自动）{}。{tip}",
                manual_refresh_suffix()
            ),
        )
    }
    fn close_browser(&self) -> (bool, String) {
        (true, "（mac 未自动关闭内置浏览器，可手动关闭）".to_string())
    }
    fn manual(&self) -> bool {
        true
    }
}

/// Windows：纯 Rust 全原生 RPA。**点击**文件传输助手里已发好的固定种子链接（用户首次手动发送
/// 一次并「框选」标定点位，App 里有教程）→ 置顶文件助手 → 按标定点位点击拉起内置浏览器；
/// 抓完 `close_browser` 关窗成闭环。
pub struct WindowsWeChatController;

#[cfg(target_os = "windows")]
impl WeChatController for WindowsWeChatController {
    fn open_seed(&self, url: &str) -> (bool, String) {
        win_engine::open_seed(url)
    }
    fn close_browser(&self) -> (bool, String) {
        win_engine::close_browser()
    }
    fn self_check(&self) -> SelfCheck {
        win_engine::self_check()
    }
    fn prepare_pick(&self) -> Result<Rect, String> {
        win_engine::prepare_pick()
    }
    fn mark_seed(&self, rect: Rect) -> Result<SeedMark, String> {
        win_engine::mark_seed(rect)
    }
    fn test_click(&self) -> TestClick {
        win_engine::test_click()
    }
}

// 非 Windows 平台下 WindowsWeChatController 不应被构造（get_controller 不会选它）；
// 给个退化实现，保证跨平台可编译（例如 doc/test 引用类型时）。
#[cfg(not(target_os = "windows"))]
impl WeChatController for WindowsWeChatController {
    fn open_seed(&self, _url: &str) -> (bool, String) {
        (
            false,
            "WindowsWeChatController 仅在 Windows 生效".to_string(),
        )
    }
    fn close_browser(&self) -> (bool, String) {
        (true, String::new())
    }
}

/// 某个微信号的种子标定状态（`none` / `ok` / `stale`，与 [`SelfCheck::mark`] 同义）：
/// Windows 下按当前文件助手窗口 / 屏幕换算判定是否失效（找不到窗口时只看文件在不在）；其它平台只看文件在不在。
pub fn calibration_mark_for(id: i64) -> &'static str {
    let path = click_cache_path_for(id);
    let Some(cache) = ClickCache::load(&path) else {
        return MARK_NONE;
    };
    #[cfg(target_os = "windows")]
    {
        win_engine::mark_for_cache(&cache)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = cache;
        MARK_OK
    }
}

/// 按平台与 `rpa_enabled` 造控制器。`rpa_enabled=false` 或平台不支持 → NoOp。
pub fn get_controller(rpa_enabled: bool) -> Box<dyn WeChatController> {
    if !rpa_enabled {
        return Box::new(NoOpController);
    }
    #[cfg(target_os = "windows")]
    {
        Box::new(WindowsWeChatController)
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(MacWeChatController)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Box::new(NoOpController)
    }
}

/// Windows 原生 RPA 引擎：窗口枚举/置顶/关闭、鼠标键盘、按标定点位点种子闭环。
///
/// 全部 best-effort，任何系统调用失败都退化返回，不 panic。坐标系为**物理像素**：
/// 线程设为 per-monitor DPI 感知后 `GetSystemMetrics`/`GetWindowRect`/`SetCursorPos` 同处物理像素空间，
/// 与前端框选层换算出的物理坐标一致。
#[cfg(target_os = "windows")]
mod win_engine {
    use super::{
        click_cache_path, normalize_rect, rect_center, ClickCache, Rect, SeedMark, MARK_NONE,
        MARK_OK, MARK_STALE,
    };
    use std::ffi::c_void;
    use std::thread::sleep;
    use std::time::{Duration, Instant};
    use tracing::debug;
    use windows::core::{BOOL, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, POINT, RECT, TRUE, WPARAM};
    use windows::Win32::System::Threading::{
        AttachThreadInput, GetCurrentProcessId, GetCurrentThreadId, OpenProcess,
        QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::HiDpi::{
        GetDpiForWindow, SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        keybd_event, SendInput, INPUT, INPUT_0, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
        MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE,
        MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT, MOUSE_EVENT_FLAGS, VK_CONTROL, VK_F5,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, EnumWindows, GetAncestor, GetForegroundWindow, GetSystemMetrics,
        GetWindowRect, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
        IsWindowVisible, PostMessageW, SetCursorPos, SetForegroundWindow, SetWindowPos, ShowWindow,
        WindowFromPoint, GA_ROOT, HWND_NOTOPMOST, HWND_TOPMOST, SM_CXSCREEN, SM_CXVIRTUALSCREEN,
        SM_CYSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SWP_NOMOVE,
        SWP_NOSIZE, SW_RESTORE, WM_CLOSE,
    };

    /// 点击后等新内置浏览器窗口出现的上限（轮询，不再固定睡 2.5s）。
    const OPEN_WAIT: Duration = Duration::from_millis(5000);
    /// 新浏览器窗口出现后、发硬刷新前的等待。
    const REFRESH_SETTLE: Duration = Duration::from_millis(2500);

    /// 把**当前线程**设成 per-monitor DPI 感知（V2）。Tauri 进程本已如此，这里是兜底：
    /// 保证取窗口矩形（`GetWindowRect`）与点击（`SetCursorPos`）在同一线程、同一物理像素空间，
    /// 即便从非 DPI 感知的宿主（测试二进制 / schtasks 脚手架）调用也不会坐标缩放错位。
    fn ensure_dpi_aware() {
        unsafe {
            let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
    }

    const FH_TITLE: &str = "文件传输助手";
    const APPEX_PROC: &str = "wechatappex.exe";

    /// 可见顶层窗口信息（`hwnd` 存 isize，跨线程安全，用时重建 [`HWND`]）。
    #[derive(Clone)]
    struct WinInfo {
        hwnd: isize,
        title: String,
        proc: String,
        rect: Rect, // (left, top, right, bottom)
    }

    impl WinInfo {
        fn hwnd(&self) -> HWND {
            HWND(self.hwnd as *mut c_void)
        }
        fn width(&self) -> i32 {
            self.rect.2 - self.rect.0
        }
        fn height(&self) -> i32 {
            self.rect.3 - self.rect.1
        }
    }

    /// 进程名（小写文件名，如 `wechatappex.exe`）；失败返回空串。
    fn proc_name(pid: u32) -> String {
        unsafe {
            let handle = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                Ok(h) => h,
                Err(_) => return String::new(),
            };
            let mut buf = [0u16; 260];
            let mut size = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut size,
            );
            let _ = CloseHandle(handle);
            if ok.is_ok() {
                let full = String::from_utf16_lossy(&buf[..size as usize]);
                full.rsplit(['\\', '/']).next().unwrap_or("").to_lowercase()
            } else {
                String::new()
            }
        }
    }

    unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let list = &mut *(lparam.0 as *mut Vec<WinInfo>);
        if IsWindowVisible(hwnd).as_bool() {
            // 标题为空的窗口也收（刚拉起、还在加载的内置浏览器窗口标题可能暂空）；
            // `find_fh` 按标题精确匹配、`appex_windows` 按进程名+尺寸筛，都不受影响。
            let len = GetWindowTextLengthW(hwnd);
            let title = if len > 0 {
                let mut buf = vec![0u16; (len + 1) as usize];
                let n = GetWindowTextW(hwnd, &mut buf);
                String::from_utf16_lossy(&buf[..n as usize])
            } else {
                String::new()
            };
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            let mut r = RECT::default();
            let _ = GetWindowRect(hwnd, &mut r);
            list.push(WinInfo {
                hwnd: hwnd.0 as isize,
                title,
                proc: proc_name(pid),
                rect: (r.left, r.top, r.right, r.bottom),
            });
        }
        TRUE
    }

    fn list_windows() -> Vec<WinInfo> {
        let mut list: Vec<WinInfo> = Vec::new();
        unsafe {
            let _ = EnumWindows(
                Some(enum_cb),
                LPARAM(&mut list as *mut Vec<WinInfo> as isize),
            );
        }
        list
    }

    fn find_fh() -> Option<WinInfo> {
        list_windows().into_iter().find(|w| w.title == FH_TITLE)
    }

    /// 微信内置浏览器：`WeChatAppEx.exe` 拥有的、足够大的可见顶层窗口（排除渲染小面板）。
    fn appex_windows() -> Vec<WinInfo> {
        list_windows()
            .into_iter()
            .filter(|w| w.proc == APPEX_PROC && w.width() > 400 && w.height() > 300)
            .collect()
    }

    /// 置前指定窗口：还原 + 抬到最上 + 抢前台 + 设为 TOPMOST。
    ///
    /// **每次点击前必做**：窗口最小化 / 被遮挡时点击会落到别的窗口上，或只被当成激活窗口而吞掉。
    /// `SetForegroundWindow` 对后台进程默认被系统拒绝（前台锁），对齐 Python 边车：先把当前线程
    /// 的输入队列 `AttachThreadInput` 到当前前台窗口线程再抢，否则文件助手只是 z 序靠前、并未激活。
    ///
    /// `keep_topmost`：`true` 让窗口**保持** TOPMOST（文件助手用——点击期间本进程 GUI 与其它程序都压不到
    /// 它上面，也不再收起我们自己的窗口；用户本就该把它设为独立置顶窗口，这里只是兜底确保）；`false` 则
    /// 只瞬时 TOPMOST 再降回 NOTOPMOST（内置浏览器用——刷新完不该一直压在别的窗口上）。
    fn activate(hwnd: HWND, keep_topmost: bool) {
        unsafe {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = BringWindowToTop(hwnd);
            let fg = GetForegroundWindow();
            let me = GetCurrentThreadId();
            let fg_tid = if fg.0.is_null() {
                0
            } else {
                GetWindowThreadProcessId(fg, None)
            };
            let attached =
                fg_tid != 0 && fg_tid != me && AttachThreadInput(me, fg_tid, true).as_bool();
            let _ = SetForegroundWindow(hwnd);
            if attached {
                let _ = AttachThreadInput(me, fg_tid, false);
            }
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE,
            );
            if !keep_topmost {
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_NOTOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE,
                );
            }
        }
        sleep(Duration::from_millis(450));
    }

    /// 置前文件助手（保持 TOPMOST）并**重新取一次矩形**（最小化还原后矩形会变）。
    fn activate_fh(fh: &WinInfo) -> WinInfo {
        activate(fh.hwnd(), true);
        find_fh().unwrap_or_else(|| fh.clone())
    }

    /// 窗口所属进程 id。
    fn window_pid(w: &WinInfo) -> u32 {
        let mut pid = 0u32;
        unsafe {
            GetWindowThreadProcessId(w.hwnd(), Some(&mut pid));
        }
        pid
    }

    /// 「「标题」（进程）」形式的窗口描述，诊断日志用。
    fn describe_win(w: Option<WinInfo>) -> String {
        w.map(|w| format!("「{}」（{}）", w.title, w.proc))
            .unwrap_or_else(|| "无".to_string())
    }

    /// 当前前台窗口。
    fn foreground() -> Option<WinInfo> {
        let fg = unsafe { GetForegroundWindow() };
        if fg.0.is_null() {
            return None;
        }
        let key = fg.0 as isize;
        list_windows().into_iter().find(|w| w.hwnd == key)
    }

    /// 屏幕点 `(x, y)` 上方实际是哪个**顶层**窗口（`WindowFromPoint` 取到的是子控件，回到根窗口）。
    fn window_at(x: i32, y: i32) -> Option<WinInfo> {
        unsafe {
            let h = WindowFromPoint(POINT { x, y });
            if h.0.is_null() {
                return None;
            }
            let root = GetAncestor(h, GA_ROOT);
            let root = if root.0.is_null() { h } else { root };
            let key = root.0 as isize;
            list_windows().into_iter().find(|w| w.hwnd == key)
        }
    }

    /// **点击前守卫**：点位上方必须是文件助手本身，否则这一下会按到别的程序上。
    ///
    /// 实测：用户在我们 GUI 里点「启动轮询」后 GUI 窗口正压着文件助手的标定区，
    /// `activate` 没能把文件助手抬到最上，RPA 的点击落在 GUI 的「停止轮询」按钮上——轮询启动 3 秒
    /// 即被停掉。所以：点位被别的窗口压着（含本进程 GUI）就把文件助手重新置顶（TOPMOST）再查；
    /// 最多三次仍不是文件助手就**放弃点击**并说明被谁压着。**不再收起 / 最小化本进程窗口**
    /// （2026-09-07 起：只保证文件助手置顶，App 窗口保持原样）。
    fn ensure_fh_on_top(fh: &WinInfo, x: i32, y: i32) -> Result<WinInfo, String> {
        let mut fh = fh.clone();
        for _ in 0..3 {
            match window_at(x, y) {
                Some(w) if w.hwnd == fh.hwnd => return Ok(fh),
                Some(w) => {
                    let mine = window_pid(&w) == unsafe { GetCurrentProcessId() };
                    debug!(title = %w.title, process = %w.proc, mine, "点位被其它窗口压着，重新置顶文件助手");
                }
                None => {}
            }
            fh = activate_fh(&fh);
        }
        let over = window_at(x, y)
            .map(|w| format!("「{}」（{}）", w.title, w.proc))
            .unwrap_or_else(|| "未知窗口".to_string());
        Err(format!(
            "文件助手未能置顶到点位 ({x},{y}) 上方，压着它的是 {over}；已放弃点击以免误点其它程序。请让文件助手独立置顶、别被其它窗口盖住"
        ))
    }

    /// 关闭所有内置浏览器窗口，返回被关标题。
    fn close_browsers() -> Vec<String> {
        let mut closed = Vec::new();
        for w in appex_windows() {
            unsafe {
                let _ = PostMessageW(Some(w.hwnd()), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            closed.push(w.title);
        }
        sleep(Duration::from_millis(1000));
        closed
    }

    /// 轮询等「点击前不存在」的新内置浏览器窗口出现，最多 `max`；一出现立即返回。
    fn wait_new_appex(before: &[isize], max: Duration) -> Vec<WinInfo> {
        let t0 = Instant::now();
        loop {
            let opened: Vec<WinInfo> = appex_windows()
                .into_iter()
                .filter(|w| !before.contains(&w.hwnd))
                .collect();
            if !opened.is_empty() || t0.elapsed() >= max {
                return opened;
            }
            sleep(Duration::from_millis(300));
        }
    }

    /// 物理像素坐标 → `SendInput` 绝对坐标（0..65535，跨整个虚拟桌面，配 `MOUSEEVENTF_VIRTUALDESK`）。
    /// 线程已 per-monitor DPI 感知，虚拟屏指标与传入的 `(x,y)` 同处物理像素空间，多屏/负原点也正确。
    fn to_abs(x: i32, y: i32) -> (i32, i32) {
        unsafe {
            let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let vw = (GetSystemMetrics(SM_CXVIRTUALSCREEN) - 1).max(1) as i64;
            let vh = (GetSystemMetrics(SM_CYVIRTUALSCREEN) - 1).max(1) as i64;
            let ax = ((x - vx) as i64 * 65535 / vw).clamp(0, 65535) as i32;
            let ay = ((y - vy) as i64 * 65535 / vh).clamp(0, 65535) as i32;
            (ax, ay)
        }
    }

    /// 造一条鼠标 `INPUT`（绝对坐标；构造 union 是安全的）。
    fn mouse_input(flags: MOUSE_EVENT_FLAGS, ax: i32, ay: i32) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: ax,
                    dy: ay,
                    mouseData: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    /// 屏幕左键单击（物理像素绝对坐标）。
    ///
    /// **必须走 `SendInput` 注入「移动→按下→抬起」全序列、且都带绝对坐标**：微信内置浏览器是
    /// Chromium/CEF，其渲染进程按**注入输入流**里的鼠标移动更新内部指针命中；只用 `SetCursorPos`
    /// 瞬移光标（不经输入流）时，渲染器指针仍停在旧位置，随后 `mouse_event` 的按下会落在旧命中处，
    /// 链接不触发——现象正是「光标已到链接、窗口已置前，但点击不打开浏览器」。这里先从链接左上方
    /// 一点移入制造真实位移向量（零位移移动有的渲染器不更新 hover），停顿让渲染器处理 hover，再在
    /// 同一绝对坐标按下/抬起。`SetCursorPos` 仅作可见反馈兜底。
    fn click_at(x: i32, y: i32) -> bool {
        unsafe {
            let _ = SetCursorPos(x, y); // 可见反馈；真正命中靠下面的注入序列
            let (ax0, ay0) = to_abs(x - 6, y - 4);
            let (ax, ay) = to_abs(x, y);
            let move_flags = MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK;
            let moves = [
                mouse_input(move_flags, ax0, ay0),
                mouse_input(move_flags, ax, ay),
            ];
            if SendInput(&moves, std::mem::size_of::<INPUT>() as i32) == 0 {
                debug!(x, y, "SendInput 移动失败（可能无交互桌面会话）");
                return false;
            }
            sleep(Duration::from_millis(180)); // 让 CEF 渲染器处理 hover 命中
            let down = [mouse_input(
                MOUSEEVENTF_LEFTDOWN | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                ax,
                ay,
            )];
            SendInput(&down, std::mem::size_of::<INPUT>() as i32);
            sleep(Duration::from_millis(70));
            let up = [mouse_input(
                MOUSEEVENTF_LEFTUP | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                ax,
                ay,
            )];
            SendInput(&up, std::mem::size_of::<INPUT>() as i32);
        }
        true
    }

    fn key(vk: u16, up: bool) {
        let flags = if up {
            KEYEVENTF_KEYUP
        } else {
            KEYBD_EVENT_FLAGS(0)
        };
        unsafe { keybd_event(vk as u8, 0, flags, 0) };
    }

    /// Ctrl+F5 硬刷新（绕浏览器缓存，强制走网络，配合抓凭证 / 接力）。
    /// **仅 Windows**：本模块整体 `cfg(target_os = "windows")`；mac 的对应键是 ⌘⇧R，
    /// 由人工模式提示（[`super::hard_refresh_hint`]），不在这里发。
    fn hard_refresh() {
        key(VK_CONTROL.0, false);
        key(VK_F5.0, false);
        sleep(Duration::from_millis(40));
        key(VK_F5.0, true);
        key(VK_CONTROL.0, true);
    }

    /// 主屏物理分辨率（宽,高）。
    fn screen_size() -> (i32, i32) {
        unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) }
    }

    /// 指定窗口所在显示器的**显示缩放百分比**（`GetDpiForWindow`：dpi*100/96，如 96→100、144→150）。
    /// 仅诊断用：坐标换算 / 点击始终按物理像素，缩放不参与运算。取不到（老系统 / 无效句柄）返回 `None`。
    fn window_scale_pct(hwnd: HWND) -> Option<u32> {
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        if dpi == 0 {
            None
        } else {
            Some(dpi * 100 / 96)
        }
    }

    /// 缓存点位换算结果：`(标定状态, Option<(绝对点位, 绝对框选矩形)>)`。
    type CachedResolve = (&'static str, Option<((i32, i32), Rect)>);

    /// 读缓存并按当前窗口 / 屏幕换算成绝对点位；返回 `(标定状态, 换算结果)`。
    fn cached_point_for(fh: &WinInfo) -> CachedResolve {
        match ClickCache::load(&click_cache_path()) {
            None => (MARK_NONE, None),
            Some(cache) => match cache.resolve(fh.rect, screen_size()) {
                Some(v) => (MARK_OK, Some(v)),
                None => (MARK_STALE, None),
            },
        }
    }

    /// 标定缺失 / 失效时给人看的指引。
    fn mark_hint(mark: &str) -> String {
        if mark == MARK_STALE {
            "种子点位标定已失效（文件助手窗口尺寸或屏幕分辨率变了），请在「微信号管理」重新「框选种子」"
                .to_string()
        } else {
            "尚未标定种子点位：请先把固定种子链接发到「文件传输助手」，再在「启动自检」卡点「框选种子」框住那条消息"
                .to_string()
        }
    }

    /// 浏览器窗口拉起后的**可选**收尾（`rpa_hard_refresh` 开关开着才做，默认不做）：置前新窗口并 Ctrl+F5
    /// 硬刷新，绕过 WebView 缓存强制走网络。种子页改本机直出 + no-store、文章页由 MITM 加 no-store 后已无必要，
    /// 留作回退验证用。
    /// 浏览器窗口刚出现到首屏加载完有一段时间；先等 [`REFRESH_SETTLE`] 再置前发 Ctrl+F5
    /// （参考项目 wx-shortlink-worker 也是等 2.5 秒再硬刷），太早按键会被尚未就绪的 WebView 吞掉。
    fn refresh_opened(opened: &[WinInfo]) {
        if let Some(w) = opened.first() {
            sleep(REFRESH_SETTLE);
            // 内置浏览器只需瞬时置前拿到键盘焦点，刷新完不该一直压在别的窗口上。
            activate(w.hwnd(), false);
            hard_refresh();
        }
    }

    /// 某份标定按**当前**文件助手窗口 / 屏幕换算是否仍有效；找不到文件助手窗口时无法判定，按有效算。
    pub(super) fn mark_for_cache(cache: &ClickCache) -> &'static str {
        ensure_dpi_aware();
        match find_fh() {
            Some(fh) => match cache.resolve(fh.rect, screen_size()) {
                Some(_) => MARK_OK,
                None => MARK_STALE,
            },
            None => MARK_OK,
        }
    }

    /// 运行前**自检**：窗口在否 / 种子点位标定状态。不置前窗口（只是查，不打扰用户）。
    pub fn self_check() -> super::SelfCheck {
        ensure_dpi_aware();
        let screen = screen_size();
        let fh = find_fh();
        let (mark, resolved) = match fh.as_ref() {
            Some(fh) => cached_point_for(fh),
            None => (MARK_NONE, None),
        };
        let fh_found = fh.is_some();
        let ready = fh_found && mark == MARK_OK;
        let message = if !fh_found {
            format!("未找到「{FH_TITLE}」窗口，请把它设为独立置顶窗口")
        } else if mark != MARK_OK {
            mark_hint(mark)
        } else {
            let ((x, y), _) = resolved.expect("mark ok 时必有点位");
            format!("就绪：种子点位已标定，当前换算为 ({x},{y})")
        };
        super::SelfCheck {
            ready,
            fh_found,
            fh_rect: fh.as_ref().map(|w| w.rect),
            screen: Some(screen),
            scale_pct: fh.as_ref().and_then(|w| window_scale_pct(w.hwnd())),
            mark: mark.to_string(),
            cached_point: resolved.map(|(p, _)| p),
            cached_rect: resolved.map(|(_, r)| r),
            message,
        }
    }

    /// **框选前准备**：置顶文件助手（框选层随后盖在上面，用户对着真实窗口框），返回其矩形。
    pub fn prepare_pick() -> Result<Rect, String> {
        ensure_dpi_aware();
        let fh =
            find_fh().ok_or_else(|| format!("未找到「{FH_TITLE}」窗口，请把它设为独立置顶窗口"))?;
        let fh = activate_fh(&fh);
        Ok(fh.rect)
    }

    /// **提交框选**：换算成相对文件助手窗口的偏移并落盘。不置前窗口（框选层还盖在上面）。
    pub fn mark_seed(rect: Rect) -> Result<SeedMark, String> {
        ensure_dpi_aware();
        let fh = find_fh().ok_or_else(|| format!("未找到「{FH_TITLE}」窗口"))?;
        let rect = normalize_rect(rect);
        let (cx, cy) = rect_center(rect);
        let (l, t, r, b) = fh.rect;
        if !(cx >= l && cx < r && cy >= t && cy < b) {
            return Err(format!(
                "框选中心 ({cx},{cy}) 不在「{FH_TITLE}」窗口 ({l},{t})-({r},{b}) 内，请框住文件助手里的种子链接消息"
            ));
        }
        let path = click_cache_path();
        let cache = ClickCache::from_rect(rect, fh.rect, screen_size());
        cache
            .save(&path)
            .map_err(|e| format!("点位缓存存盘失败：{e}"))?;
        debug!(?rect, ?cx, ?cy, fh_rect = ?fh.rect, path = %path.display(), "种子点位已标定");
        Ok(SeedMark {
            point: (cx, cy),
            rect,
            fh_rect: fh.rect,
            cache_path: path.display().to_string(),
            message: format!(
                "已标定种子点位 ({cx},{cy})（相对文件助手窗口偏移 ({},{})），已缓存到 {}；可点「测试点击」验证",
                cache.offset.0,
                cache.offset.1,
                path.display()
            ),
        })
    }

    /// open_seed 主流程：清残留浏览器 → 找文件助手 → **置顶** → 读标定框 → **框内随机取点**点击拉起 →
    /// 轮询检测新浏览器窗口（没拉起就重新置顶、换个随机点再点一次）→（仅开关开着）Ctrl+F5 硬刷新。
    ///
    /// **不再自动发送种子**：改为用户首次手动把固定入口链接（本机种子入口服务地址，
    /// [`crate::seedserver::seed_url`]）复制粘贴进文件传输助手发送一次并框选标定（App 里有教程）；
    /// 此后本函数只**点击**这条链接拉起内置浏览器，其页面由种子服务直接带上接力脚本跳到本批任务链接。
    /// `url` 仅用于日志/兜底。
    pub fn open_seed(_url: &str) -> (bool, String) {
        ensure_dpi_aware();
        let pre = close_browsers(); // 清上一轮残留
        let fh = match find_fh() {
            Some(w) => w,
            None => {
                return (
                    false,
                    format!("未找到「{FH_TITLE}」窗口（需设为独立置顶窗口，并已把固定种子链接手动发进去一次并框选标定）"),
                );
            }
        };

        // 最多两轮：置顶 → 框内随机取点点击 → 等新窗口。第一轮没拉起就重新置前、换个随机点再来一次
        // （首次点击可能被当成激活窗口吞掉，或上次那个随机点没压在链接文字上）。每轮都重新置顶并重取矩形。
        // 不收起本进程窗口：只靠把文件助手置为 TOPMOST + 点击前守卫保证点位上方是文件助手。
        let mut clicked_any = false;
        let mut last_point = (0, 0);
        // 诊断：每次点击前点位上方是谁 / 点击后前台是谁（排查「点击落到别的窗口」）。
        let mut diag: Vec<String> = Vec::new();
        for attempt in 0..2u8 {
            let fh = activate_fh(&fh);
            let (mark, resolved) = cached_point_for(&fh);
            let rect = match resolved {
                Some((_, rect)) => rect,
                None => return (false, mark_hint(mark)),
            };
            // 框内随机取一点点击，避免每次点固定中心落在链接文字行外。
            let (cx, cy) = super::random_point_in_rect(rect);
            last_point = (cx, cy);
            // 点击前守卫：点位上方必须是文件助手，否则会按到别的程序（如我们自己的 GUI）上。
            if let Err(msg) = ensure_fh_on_top(&fh, cx, cy) {
                return (false, msg);
            }
            let over_before = describe_win(window_at(cx, cy));
            // 点击拉起浏览器；以「新增 WeChatAppEx 顶层窗口」为成功信号。
            let before: Vec<isize> = appex_windows().iter().map(|w| w.hwnd).collect();
            let clicked = click_at(cx, cy);
            clicked_any |= clicked;
            let opened = if clicked {
                wait_new_appex(&before, OPEN_WAIT)
            } else {
                Vec::new()
            };
            diag.push(format!(
                "第 {} 次：点位 ({cx},{cy}) 上方 {over_before}，点击后前台 {}",
                attempt + 1,
                describe_win(foreground())
            ));
            if !opened.is_empty() {
                // 默认不硬刷（种子页 no-store 直出、文章页由 MITM 加 no-store）；开关开着才发 Ctrl+F5。
                if super::hard_refresh_enabled() {
                    refresh_opened(&opened);
                }
                return (
                    true,
                    format!(
                        "已在标定框内点位 ({cx},{cy}) 拉起微信内置浏览器打开种子（清残留 {} 个，新窗口 {:?}{}）",
                        pre.len(),
                        opened.iter().map(|w| w.title.clone()).collect::<Vec<_>>(),
                        if attempt > 0 { "，第二次点击成功" } else { "" },
                    ),
                );
            }
            debug!(cx, cy, attempt, clicked, "点击后未检测到新内置浏览器窗口");
            if attempt == 0 {
                sleep(Duration::from_millis(800));
            }
        }
        (
            clicked_any,
            format!(
                "已在标定框内两次随机取点点击（末次 ({},{})）但未检测到新内置浏览器窗口：请确认文件助手里种子消息仍在标定框内（窗口滚动 / 有新消息会让位置变化，需重新「框选种子」），或链接已失效。诊断：{}",
                last_point.0,
                last_point.1,
                diag.join("；")
            ),
        )
    }

    /// **测试点击**：标定后的验证操作。不发种子，先清残留浏览器（让「新窗口」信号可靠），再
    /// 「置顶 → 框内随机取点点击 → 等新浏览器窗口」，中间量全部回显；新拉起的浏览器窗口**留着不关**，
    /// 让人看到打开的是哪一页。
    pub fn test_click() -> super::TestClick {
        ensure_dpi_aware();
        let mut out = super::TestClick::unsupported();
        out.screen = Some(screen_size());
        let fh = match find_fh() {
            Some(w) => w,
            None => {
                out.message = format!("未找到「{FH_TITLE}」窗口，请把它设为独立置顶窗口");
                return out;
            }
        };
        out.fh_found = true;
        out.scale_pct = window_scale_pct(fh.hwnd());
        // 清残留内置浏览器：否则上一次留下的窗口会让「点击后新增窗口」判断落空（误报未拉起）。
        close_browsers();
        let fh = activate_fh(&fh);
        out.fh_rect = Some(fh.rect);
        let (mark, resolved) = cached_point_for(&fh);
        let rect = match resolved {
            Some((_, rect)) => rect,
            None => {
                out.message = mark_hint(mark);
                return out;
            }
        };
        // 框内随机取一点点击（与正式任务一致），避免固定中心落在链接文字行外。
        let (cx, cy) = super::random_point_in_rect(rect);
        out.point = Some((cx, cy));
        if let Err(msg) = ensure_fh_on_top(&fh, cx, cy) {
            out.message = msg;
            return out;
        }
        let before: Vec<isize> = appex_windows().iter().map(|w| w.hwnd).collect();
        out.clicked = click_at(cx, cy);
        let opened = if out.clicked {
            wait_new_appex(&before, OPEN_WAIT)
        } else {
            Vec::new()
        };
        out.opened = opened.iter().map(|w| w.title.clone()).collect();
        out.ok = !opened.is_empty();
        out.message = if out.ok {
            format!(
                "在标定框内点位 ({cx},{cy}) 点击已拉起内置浏览器 {:?}；请核对打开的是种子链接那篇页面；浏览器窗口已留着，可手动关闭",
                out.opened
            )
        } else if !out.clicked {
            format!("标定框内点位 ({cx},{cy}) 注入点击失败（可能无交互桌面会话）")
        } else {
            format!(
                "已在标定框内点位 ({cx},{cy}) 点击但 {}s 内未出现新内置浏览器窗口：请确认框住的是种子链接文字且位置没变（窗口滚动 / 新消息会让位置变化），必要时重新「框选种子」框紧链接那行再试",
                OPEN_WAIT.as_secs()
            )
        };
        out
    }

    /// close_browser：关掉所有内置浏览器窗口，成闭环。
    pub fn close_browser() -> (bool, String) {
        let closed = close_browsers();
        (true, format!("已关闭微信内置浏览器 {} 个", closed.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_noop_controller_messages() {
        let c = NoOpController;
        let (ok, msg) = c.open_seed("https://mp.weixin.qq.com/s?x=1");
        assert!(!ok); // 兜底不算"自动打开成功"
                      // 人工模式：提示打开任意一篇文章即可（不再要求非点种子链接不可），种子链接仍附在末尾。
        assert!(msg.contains("任意一篇公众号文章"));
        assert!(msg.ends_with("https://mp.weixin.qq.com/s?x=1"));
        assert!(c.manual());
        let (ok2, _) = c.close_browser();
        assert!(ok2);
        // 非 Windows 默认不支持框选标定，不 panic。
        assert!(c.prepare_pick().is_err());
        assert!(c.mark_seed((0, 0, 10, 10)).is_err());
    }

    #[test]
    fn manual_mode_is_platform_specific() {
        // mac / NoOp 都是人工模式；Windows 控制器能自动点，manual 默认 false。
        assert!(MacWeChatController.manual());
        assert!(NoOpController.manual());
        assert!(!WindowsWeChatController.manual());
        // 不支持类提示：mac 讲清楚人工模式怎么用；其它平台只说不支持。
        let note = unsupported_note("自检");
        if cfg!(target_os = "macos") {
            assert!(note.contains("人工模式"));
            assert!(note.contains("任意一篇公众号文章"));
        } else {
            assert_eq!(note, "当前平台不支持自检");
        }
        assert!(SelfCheck::unsupported().message.contains("自检"));
        assert!(TestClick::unsupported().message.contains("测试点击"));
    }

    #[test]
    fn manual_open_alert_pushes_and_acks() {
        // 提醒是进程级全局：与 history / runstate 的提醒用例共用退避测试锁串行。
        let _g = crate::runstate::COOLDOWN_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::runstate::alert_reset();
        let id = manual_open_alert(&["甲".to_string(), "乙".to_string()], 600).expect("首次应推出");
        let a = crate::runstate::alert_latest().expect("有未确认提醒");
        assert_eq!(a.id, id);
        assert_eq!(a.kind, ALERT_KIND_MANUAL_OPEN);
        assert!(a.message.contains("「甲」「乙」"), "{}", a.message);
        assert!(a.message.contains("10 分钟"), "{}", a.message);
        // 同文案去重：再推一次返回 None。
        assert!(manual_open_alert(&["甲".to_string(), "乙".to_string()], 600).is_none());
        crate::runstate::alert_ack(id);
        assert!(crate::runstate::alert_latest().is_none());
        // 短链批预知不了号：不列目标，只说任意一篇。
        crate::runstate::alert_reset();
        manual_open_alert(&[], 601).unwrap();
        let a = crate::runstate::alert_latest().unwrap();
        assert!(
            a.message.contains("打开任意一篇公众号文章："),
            "{}",
            a.message
        );
        assert!(a.message.contains("11 分钟"), "{}", a.message);
        crate::runstate::alert_reset();
    }

    #[test]
    fn rect_helpers_normalize_any_corners() {
        assert_eq!(normalize_rect((10, 20, 3, 4)), (3, 4, 10, 20));
        assert_eq!(rect_center((3, 4, 11, 20)), (7, 12));
        assert_eq!(rect_center((11, 20, 3, 4)), (7, 12));
    }

    #[test]
    fn random_point_stays_inside_rect() {
        // 多次取点都必须落在（规范化后的）框内，且大框会用到内缩边界。
        let rects = [
            (2306, 674, 2420, 700), // 典型框选（宽 114 高 26）
            (2420, 700, 2306, 674), // 反向两角
            (100, 100, 103, 101),   // 极窄框（宽 3 高 1，小于 2*inset，不内缩）
            (500, 500, 500, 500),   // 退化成一点
        ];
        for &r in &rects {
            let (x0, y0, x1, y1) = normalize_rect(r);
            for _ in 0..200 {
                let (px, py) = random_point_in_rect(r);
                assert!(px >= x0 && px <= x1, "x {px} 越界 [{x0},{x1}]");
                assert!(py >= y0 && py <= y1, "y {py} 越界 [{y0},{y1}]");
                // 微调时间种子，制造不同取值
                std::hint::black_box(&px);
            }
        }
        // 退化成一点：必回该点
        assert_eq!(random_point_in_rect((500, 500, 500, 500)), (500, 500));
    }

    #[test]
    fn click_cache_roundtrip_and_resolve_follows_window_move() {
        let fh = (2093, 20, 2551, 887);
        let screen = (2560, 1440);
        // 用户从右下往左上框（任意两角）也要正确规范化。
        let c = ClickCache::from_rect((2420, 700, 2306, 674), fh, screen);
        assert_eq!(c.offset, (270, 667));
        assert_eq!(c.rect, (213, 654, 327, 680));
        assert_eq!(c.fh_size, (458, 867));
        // 同尺寸同分辨率：原位还原
        assert_eq!(
            c.resolve(fh, screen),
            Some(((2363, 687), (2306, 674, 2420, 700)))
        );
        // 窗口整体挪动：跟着挪
        assert_eq!(
            c.resolve((100, 50, 558, 917), screen),
            Some(((370, 717), (313, 704, 427, 730)))
        );
        // 窗口尺寸变了 / 分辨率变了：失效
        assert_eq!(c.resolve((2093, 20, 2551, 900), screen), None);
        assert_eq!(c.resolve(fh, (1920, 1080)), None);
        assert!(!c.is_valid_for(fh, (1920, 1080)));

        let dir = std::env::temp_dir().join(format!("mp_click_cache_{}", std::process::id()));
        let path = dir.join("sub").join("rpa_click_cache.json");
        c.save(&path).unwrap();
        assert_eq!(ClickCache::load(&path), Some(c));
        // 损坏文件 → None，不 panic
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(ClickCache::load(&path), None);
        // 旧版（视觉定位时代）缓存缺 rect 字段 → 视为无标定，需重新框选
        std::fs::write(
            &path,
            r#"{"offset":[1,2],"fh_size":[3,4],"screen":[5,6],"method":"bubble","saved_at":0}"#,
        )
        .unwrap();
        assert_eq!(ClickCache::load(&path), None);
        assert_eq!(ClickCache::load(&dir.join("missing.json")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn click_cache_path_sits_in_rpa_data_dir() {
        // 进程级激活号 id 可能被并行单测改动：只断言两条分支各自的文件名与目录。
        assert_eq!(
            click_cache_path_for(7).file_name().and_then(|n| n.to_str()),
            Some("rpa_click_cache_7.json")
        );
        assert_eq!(
            click_cache_path_for(7).parent(),
            Some(rpa_data_dir().as_path())
        );
        let name = click_cache_path()
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap()
            .to_string();
        assert!(
            name.starts_with("rpa_click_cache") && name.ends_with(".json"),
            "{name}"
        );
        assert_eq!(click_cache_path().parent(), Some(rpa_data_dir().as_path()));
    }

    #[test]
    fn test_get_controller_disabled_is_noop() {
        // rpa_enabled=false → NoOp（open_seed 返回 false）
        let c = get_controller(false);
        let (ok, _) = c.open_seed("https://mp.weixin.qq.com/s?x=1");
        assert!(!ok);
    }
}
