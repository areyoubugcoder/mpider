//! 真机整链的库级入口（供 GUI / CLI 复用，避免各处重复拼装 core 内部）。
//!
//! [`run_loop_real`] 内部就是"拼装真依赖并长驻跑 orchestrator 主循环"：HttpReporter（结果上报）+
//! CaptureProxy(共享 relay) + get_controller + SystemProxyGuard(Drop 复位) + collect_list；
//! 巡检模式跑巡检批次，历史模式只推进历史抓取任务。[`sweep_retry_account`] 是单号「重新巡检」。
//!
//! 日志：整链各环节都写 [`crate::applog`]（入库 `app_log` 留 3 天 + 推给订阅者）；`progress`
//! 回调即一次 run 期间的订阅者（GUI 把 [`LogEvent`] 推给前端 / CLI 打印 `render()`），run 结束自动退订。

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::Result;
use serde::Deserialize;

use crate::applog::{self, LogEvent, Stage};
use crate::capture::{self, CaptureConfig, CaptureProxy, PassthroughDecider};
use crate::collector::{collect_list, ListRequest};
use crate::orchestrator::{
    default_sleep, CaptureStartFn, CaptureStopFn, CollectFn, Orchestrator, OrchestratorConfig,
    ReportFn,
};
use crate::relay::RelayQueue;
use crate::report::{HttpReporter, ReportConfig};
use crate::rpa::get_controller;
use crate::seedserver::{self, SeedDwell};
use crate::sweep::{Sweep, SweepConfig};
#[cfg(not(windows))]
use crate::sysproxy::CommandApplier;
use crate::sysproxy::SystemProxyGuard;

/// 平台相关的系统代理执行器：Windows 走 WinINET（reg 改注册表 + InternetSetOption 刷新），
/// 其余平台走命令（macOS 的 `networksetup`）。
#[cfg(windows)]
pub(crate) type SysProxyApplier = crate::sysproxy::WinInetApplier;
#[cfg(not(windows))]
pub(crate) type SysProxyApplier = CommandApplier;
use crate::{CaMaterial, Store};

/// 主循环空转（历史下一页没到点、巡检没到点）时的检查间隔（秒）。
pub const MAIN_LOOP_POLL_SECS: u64 = 5;

/// 真机整链运行配置（serde 可反序列化，GUI/CLI/文件都能喂）。字段有合理默认；老配置里多余的字段
/// （早期的上游 / 代理池项）被 serde 忽略。
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct RealRunConfig {
    /// 结果上报开关（默认关）：开了之后每个号列表采完就 POST 一条到 `report_url`（见 `report.rs`）。
    pub report_enabled: bool,
    /// 接收上报的 HTTP(S) 地址。
    pub report_url: String,
    /// 可选 Bearer token。**不得写日志。**
    pub report_token: String,
    /// 上报请求超时（秒，默认 15）。
    pub report_timeout_secs: u64,
    /// MITM 监听端口（0 = 系统分配）。
    pub capture_port: u16,
    /// 本机种子入口服务写进种子链接里的 host（默认 `127.0.0.1`；配成本机局域网 IP 时服务绑 `0.0.0.0`）。
    /// 种子链接 = `http://<seed_host>:<seed_port>/`，用户首次手动发进文件传输助手；改了要重发并重新框选。
    pub seed_host: String,
    /// 本机种子入口服务端口（0 = 默认 8787）。必须固定：文件助手里那条链接不会跟着变。
    pub seed_port: u16,
    /// 每条链接在 WebView 停留下限（毫秒）。
    pub relay_dwell_ms: i64,
    /// 停留上限（毫秒）；每跳在 `[relay_dwell_ms, relay_dwell_max_ms]` 随机，避免固定节拍。
    /// `<= relay_dwell_ms` 即固定停留。
    pub relay_dwell_max_ms: i64,
    /// 种子链接打开后等多久跳到本批第一条任务链接（毫秒，固定值，下限 0，默认 100）。种子页本身没内容要看，
    /// 只是拉起浏览器的入口，所以单独配置、通常比文章页停留短。之后各篇仍按 `[relay_dwell_ms, relay_dwell_max_ms]` 随机。
    pub seed_dwell_ms: i64,
    pub capture_wait_seconds: u64,
    /// 看门狗：点种子后多少秒无文章请求判"点空"重点。
    pub relay_launch_timeout_seconds: u64,
    /// 看门狗：接力中多少秒无文章请求判停滞（实际取值不小于 停留上限 + 8 秒）。
    pub relay_stall_seconds: u64,
    /// 看门狗：无进展重拉的最大次数。
    pub relay_max_relaunch: u32,
    pub cred_ttl_seconds: i64,
    /// 列表 getmsg 每页条数。
    pub list_page_count: i64,
    /// 有「最后更新时间」时单号单次最多翻页数（防 since 太早无止境翻）。
    pub list_max_pages: usize,
    /// 翻页之间随机等待的下限 / 上限（毫秒）。
    pub page_sleep_min_ms: u64,
    pub page_sleep_max_ms: u64,
    /// 凭证续期：一次续期最多尝试几轮（≥3）。
    pub cred_refresh_attempts: u32,
    /// 凭证续期：每轮等新 key 的上限（秒）。
    pub cred_refresh_wait_seconds: u64,
    /// 凭证续期：同一批任务里最多做几次「集中续期 → 续采」循环。
    pub cred_refresh_rounds: u32,
    /// 是否自动设/复位系统代理（SystemProxyGuard Drop 复位）。
    pub set_sysproxy: bool,
    pub sysproxy_service: String,
    pub rpa_enabled: bool,
    /// RPA 拉起内置浏览器后是否再发 Ctrl+F5 硬刷（默认**关**，2026-09-08）。种子页本机直出 + no-store、文章页
    /// 由 MITM 加 no-store 后硬刷没有意义，且它落在首条文章页上白白多一次重载；留开关供回退验证。
    pub rpa_hard_refresh: bool,
    /// SQLite 路径。
    pub db_path: String,
    /// 根 CA 证书 / 私钥的固定路径（约定 `app_data_dir/ca.crt` + `app_data_dir/ca.key`）。
    ///
    /// 两者都非空且都存在时**复用**这张 CA（macOS 信任认「具体某张证书」而非 CN，必须复用
    /// gui 一键装进系统信任库的那张）；否则新生成并落盘到这两个路径（供 gui 安装）。空串则
    /// 生成临时 CA（不落盘、不持久信任）。
    pub ca_cert_path: String,
    pub ca_key_path: String,
    /// 定时巡检：一轮跑完后停留多久再开始下一轮（秒，默认 1 小时）。
    pub sweep_idle_seconds: u64,
    /// 定时巡检：每批几个号（默认 20；一批的接力 + 采集要在凭证 30 分钟有效期内跑完）。
    pub sweep_batch_size: usize,
    /// 等凭证 / 等续期的上限按每条链接再给的秒数（实际 = max(固定上限, 链接数 × 本值)）。
    pub capture_wait_per_link_seconds: u64,
    /// 撞限流信号（429 / 5xx / 验证页）后等多久再重试同一页一次（毫秒）。
    pub rate_retry_wait_ms: u64,
    /// 全局列表请求闸门：任意两次 `getmsg`（跨号 / 跨页 / 跨任务源）之间的随机间隔区间（毫秒）。
    /// 同批号之间连发会被微信号级封禁（ret=-6），故设硬闸；默认 8–20 秒。
    pub list_gap_min_ms: u64,
    pub list_gap_max_ms: u64,
    /// 每号列表请求预算：当前微信号近 24 小时 `getmsg` 上限（所有路径合计；0 = 不限），达到后历史抓取与巡检
    /// 都暂停领取，等最早一次请求滑出窗口。默认 180：实测微信号在累计约 200 次处被 `ret=-6`，取下限再留
    /// 一批的余量（见 GUI「常见问题」）。
    pub list_daily_budget: i64,
    /// 巡检：两个批次之间至少隔多久（秒）。
    pub sweep_batch_gap_seconds: u64,
    /// 巡检：一批无一成功且有号待重试时，整个巡检暂停多久再继续（秒）。
    pub sweep_fail_pause_seconds: u64,
    /// 抓取历史文章：每页条数（1–50，默认 10；`getmsg` 的 `count`，服务端实测按 10 封顶）。
    pub history_page_count: i64,
    /// 抓取历史文章：页间隔（秒，默认 60；不低于列表闸门上限）。
    pub history_gap_seconds: u64,
    /// 抓取历史文章：为巡检保留的预算（默认 30；历史任务不吃掉当前微信号 24 小时预算的最后这些次）。
    pub history_budget_reserve: i64,
    /// 飞书通知开关（默认关）：开了要配机器人 Webhook；异常（任务异常 / 限流退避 / 巡检故障 / 轮询退出）
    /// 推文本消息到群（见 `notify.rs`）。
    pub feishu_enabled: bool,
    /// 飞书自定义机器人 Webhook（`https://open.feishu.cn/open-apis/bot/v2/hook/<uuid>`）。**不得写日志。**
    pub feishu_webhook: String,
    /// 飞书机器人「签名校验」密钥（空 = 机器人未开签名）。**不得写日志。**
    pub feishu_secret: String,
}

impl RealRunConfig {
    /// 派生飞书通知配置。
    pub fn feishu_config(&self) -> crate::notify::FeishuConfig {
        crate::notify::FeishuConfig {
            enabled: self.feishu_enabled,
            webhook: self.feishu_webhook.trim().to_string(),
            secret: self.feishu_secret.trim().to_string(),
        }
    }

    /// 派生历史抓取参数（GUI 的开始 / 估算 / 状态命令用；主循环里由编排配置同源派生）。
    pub fn history_config(&self) -> crate::history::HistoryConfig {
        crate::history::HistoryConfig {
            page_count: self.history_page_count,
            gap_secs: self.history_gap_seconds,
            budget_reserve: self.history_budget_reserve,
            budget: self.list_daily_budget.max(0),
            cred_ttl_secs: self.cred_ttl_seconds,
            rate_retry_wait_ms: self.rate_retry_wait_ms,
            gap_min_ms: self.list_gap_min_ms,
            gap_max_ms: self.list_gap_max_ms,
        }
    }

    /// 派生结果上报配置。
    pub fn report_config(&self) -> ReportConfig {
        ReportConfig {
            enabled: self.report_enabled,
            url: self.report_url.trim().to_string(),
            token: self.report_token.trim().to_string(),
            timeout_secs: self.report_timeout_secs.max(1),
        }
    }
}

impl RealRunConfig {
    /// 种子服务的三个停留参数（常驻服务 `seedserver::apply` 用）。
    pub fn seed_dwell(&self) -> SeedDwell {
        SeedDwell {
            seed_dwell_ms: self.seed_dwell_ms,
            relay_dwell_ms: self.relay_dwell_ms,
            relay_dwell_max_ms: self.relay_dwell_max_ms,
        }
    }
}

impl Default for RealRunConfig {
    fn default() -> Self {
        Self {
            report_enabled: false,
            report_url: String::new(),
            report_token: String::new(),
            report_timeout_secs: 15,
            capture_port: 0,
            seed_host: crate::seedserver::DEFAULT_SEED_HOST.to_string(),
            seed_port: crate::seedserver::DEFAULT_SEED_PORT,
            relay_dwell_ms: 2500,
            relay_dwell_max_ms: 4000,
            seed_dwell_ms: 100,
            capture_wait_seconds: 120,
            relay_launch_timeout_seconds: 10,
            relay_stall_seconds: 15,
            relay_max_relaunch: 3,
            cred_ttl_seconds: 30 * 60,
            list_page_count: 10,
            list_max_pages: 50,
            page_sleep_min_ms: 3000,
            page_sleep_max_ms: 8000,
            cred_refresh_attempts: 3,
            cred_refresh_wait_seconds: 60,
            cred_refresh_rounds: 3,
            set_sysproxy: false,
            sysproxy_service: "Wi-Fi".to_string(),
            rpa_enabled: true,
            rpa_hard_refresh: false,
            db_path: "data/mpider.db".to_string(),
            ca_cert_path: String::new(),
            ca_key_path: String::new(),
            sweep_idle_seconds: 3600,
            sweep_batch_size: 20,
            capture_wait_per_link_seconds: 8,
            rate_retry_wait_ms: 15_000,
            list_gap_min_ms: 8_000,
            list_gap_max_ms: 20_000,
            list_daily_budget: 180,
            sweep_batch_gap_seconds: 60,
            sweep_fail_pause_seconds: 900,
            history_page_count: 10,
            history_gap_seconds: 60,
            history_budget_reserve: 30,
            feishu_enabled: false,
            feishu_webhook: String::new(),
            feishu_secret: String::new(),
        }
    }
}

/// 另一条整链（主循环 / 手动抓凭证代理）已在跑时的提示。
pub const RUN_BUSY_MSG: &str =
    "已有整链在运行（巡检 / 历史抓取 / 手动抓凭证），代理 / 微信窗口不能同时被两条整链占用";

/// 拼装真实依赖，**长驻主循环**（[`Orchestrator::run_forever`]）：`sweep_on` 为真是巡检模式（GUI
/// 「开始巡检」），否则是历史模式（只推进历史抓取任务，没有活动任务自行退出）。`stop` 置真时
/// （当前单元处理完后）退出。
///
/// `progress` 是本次 run 期间的日志订阅者（收到整链各环节的 [`LogEvent`]；GUI 推给前端、
/// CLI 打印 `render()`），run 结束自动退订。
pub async fn run_loop_real(
    cfg: RealRunConfig,
    sweep_on: bool,
    stop: Arc<AtomicBool>,
    progress: impl Fn(LogEvent) + Send + Sync + 'static,
) -> Result<()> {
    let _sub = applog::bus().subscribe(Arc::new(move |e| progress(e.clone())));
    let Some(_guard) = crate::runstate::try_acquire_run() else {
        anyhow::bail!(RUN_BUSY_MSG);
    };
    let store = open_store(&cfg)?;
    let orch = build_orchestrator(&cfg, store, sweep_on).await?;
    orch.run_forever(stop).await;
    Ok(())
}

/// 单号「重新巡检」（GUI 公众号列表的手动操作）——**立即**执行，不排队：
///
/// - 有活动的历史抓取任务（进行中 / 暂停）时拒绝：两条链会抢同一个微信窗口与同一份预算。
/// - 先占**执行单元**互斥（[`crate::runstate::try_acquire_unit`]）：有巡检批次 / 历史页正在处理时直接拒绝，
///   提示「巡检进行中，完成后可用」；主循环长驻但批次之间空闲时可以立即执行。手动抓凭证代理开着时同样拒绝
///   （它独占 MITM / 系统代理）。
/// - 凭证新鲜 → 直接 `getmsg` 采最新列表（不起代理、不开微信）；成功即写回 `ok` + 新的 `last_published_at`。
/// - 凭证不可用（没抓过 / 过期 / 实测失效 / 直采途中 `ret=-3`）→ **立刻对该号单链接跑一次完整接力**：
///   取库里该号最新一篇长链，起抓凭证代理 + RPA 点种子 + 接力打开它换 key，再采列表（与巡检批次同一条
///   `process_job` 链路，翻页依据 `accounts.last_published_at`）。结果按巡检同样的判定写回 `accounts`。
/// - 整机限流退避中 / 预算用完 / 撞到限流信号 → 拒绝并说明（后者顺带触发退避）。
pub async fn sweep_retry_account(
    cfg: &RealRunConfig,
    store: Arc<Store>,
    biz: &str,
) -> Result<crate::sweep::RetryOutcome> {
    use crate::collector::{
        is_account_blocked_err, is_credential_expired_err, is_rate_limited_err, ListPlan,
        ListSource,
    };
    use crate::sweep::RetryOutcome;

    let label = store.account_label(biz);
    if store.get_account(biz)?.is_none() {
        anyhow::bail!("公众号不存在：{biz}");
    }
    if crate::manualcap::status().running {
        anyhow::bail!("手动抓凭证代理开着，先停止它再重新巡检");
    }
    if let Some(j) = store.history_active()? {
        anyhow::bail!(
            "『{}』的历史抓取{}，先取消它（或等它完成）再重新巡检",
            store.account_label(&j.biz),
            if j.status == crate::model::HISTORY_PAUSED {
                "已暂停但未结束"
            } else {
                "进行中"
            }
        );
    }
    let Some(_guard) = crate::runstate::try_acquire_unit() else {
        anyhow::bail!(RETRY_BUSY_MSG);
    };
    if let Some(remain) = crate::runstate::cooldown_remaining_secs() {
        anyhow::bail!(
            "整机限流退避中，{} 分钟后再试（或在控制面板「解除退避」）",
            remain.div_ceil(60)
        );
    }
    if let Some(h) = crate::runstate::list_budget_check(&store, cfg.list_daily_budget.max(0)) {
        anyhow::bail!(
            "当前微信号近 24 小时列表请求已达预算（{}/{}），{} 后再试",
            h.used,
            h.budget,
            applog::format_local(h.resume_at)
        );
    }
    store.set_cred_ttl(cfg.cred_ttl_seconds);
    if !store.credential_is_fresh(biz, cfg.cred_ttl_seconds)? {
        applog::info(
            Stage::Sweep,
            format!("🔁 手动重新巡检 {label}：凭证不可用，立即接力打开该号最新一篇换 key…"),
        );
        return retry_via_relay(cfg, store, biz, &label).await;
    }

    let since = store
        .get_account(biz)?
        .and_then(|a| a.last_published_at)
        .or(store.latest_published_for(biz)?);
    let plan = ListPlan {
        since,
        start_offset: 0,
        count: cfg.list_page_count,
        max_pages: cfg.list_max_pages,
        page_sleep_min_ms: cfg.page_sleep_min_ms,
        page_sleep_max_ms: cfg.page_sleep_max_ms,
        rate_retry_wait_ms: cfg.rate_retry_wait_ms,
        gap_min_ms: cfg.list_gap_min_ms,
        gap_max_ms: cfg.list_gap_max_ms,
        source: ListSource::Retry,
        paginate: false,
    };
    applog::info(
        Stage::Sweep,
        format!("🔁 手动重新巡检 {label}：凭证有效，直接采集最新文章列表…"),
    );
    let outcome = collect_list(&store, biz, &plan, cfg.cred_ttl_seconds, None).await;
    match outcome {
        Ok(o) if o.cred_expired => {
            applog::warn(
                Stage::Sweep,
                format!(
                    "{label} 采集途中凭证已失效（已采 {} 篇），立即接力换 key 后重采",
                    o.total
                ),
            );
            retry_via_relay(cfg, store, biz, &label).await
        }
        Ok(o) => {
            let latest = store.latest_published_for(biz)?;
            store.mark_sweep_checked(biz, "ok", "", latest)?;
            let msg = format!(
                "{label} 重新巡检完成：{}{}",
                o.feedback,
                latest
                    .map(|t| format!("，最新发布 {}", applog::format_local(t)))
                    .unwrap_or_default()
            );
            applog::info(Stage::Sweep, msg.clone());
            Ok(RetryOutcome {
                collected: true,
                relayed: false,
                message: msg,
                total: o.total,
                new_articles: o.new,
                last_published_at: latest,
            })
        }
        Err(e) if is_credential_expired_err(&e) => {
            applog::warn(
                Stage::Sweep,
                format!("{label} 凭证已失效，立即接力换 key 后重采"),
            );
            retry_via_relay(cfg, store, biz, &label).await
        }
        Err(e) if is_account_blocked_err(&e) => {
            let secs = crate::runstate::block_trigger(format!("{label}（手动重试）：{e}"));
            applog::error(
                Stage::Sweep,
                format!(
                    "⛔ {label} 手动重试：微信号已被限制（{e}）；整机退避 {} 小时，期间不领任务、不接力换 key",
                    secs / 3600
                ),
            );
            anyhow::bail!(
                "微信号已被微信限制（ret=-6，约 1 天）：{e}；已进入整机退避 {} 小时。换 key 无用，请等待或换微信号",
                secs / 3600
            )
        }
        Err(e) if is_rate_limited_err(&e) => {
            let secs = crate::runstate::cooldown_trigger(format!("{label}（手动重试）：{e}"));
            applog::error(
                Stage::Sweep,
                format!(
                    "⛔ {label} 手动重试被限流：{e}；整机退避 {} 分钟",
                    secs / 60
                ),
            );
            anyhow::bail!("被限流：{e}；已进入整机退避 {} 分钟", secs / 60)
        }
        Err(e) => {
            applog::error(Stage::Sweep, format!("{label} 手动重试失败：{e}"));
            Err(e)
        }
    }
}

/// 「重新巡检」撞上另一个执行单元（巡检批次 / 历史页）时的提示。
pub const RETRY_BUSY_MSG: &str = "巡检进行中，完成后可用";

/// 单号立即接力：取该号最新一篇长链合成一条单链接巡检任务，走完整 `process_job`（起代理 → RPA 点种子 →
/// 接力打开换 key → 采列表 → 收尾），再按巡检批次同样的判定把结果写回 `accounts`。
/// 调用方已持整链互斥守卫、已过退避 / 预算检查。
async fn retry_via_relay(
    cfg: &RealRunConfig,
    store: Arc<Store>,
    biz: &str,
    label: &str,
) -> Result<crate::sweep::RetryOutcome> {
    use crate::model::{Job, JobKind};
    use crate::sweep::RetryOutcome;

    // 取样：最新一篇带 sn 的长链（打开即触发凭证请求）；库里没文章时是批量添加记下的种子短链。
    let sample = store
        .latest_article_urls(biz, 5)?
        .into_iter()
        .find(|u| crate::sweep::sample_matches(u, biz));
    let Some(url) = sample else {
        let reason = "库里没有可用的文章链接，无法打开文章换凭证".to_string();
        store.mark_sweep_checked(biz, "no_sample", &reason, None)?;
        let msg = format!("{label} {reason}；请先用「批量添加」给该号加一条文章链接");
        applog::warn(Stage::Sweep, msg.clone());
        return Ok(RetryOutcome {
            collected: false,
            relayed: false,
            message: msg,
            ..Default::default()
        });
    };
    let since = store
        .get_account(biz)?
        .and_then(|a| a.last_published_at)
        .or(store.latest_published_for(biz)?);
    let mut link_since = std::collections::HashMap::new();
    if let Some(s) = since {
        link_since.insert(url.clone(), s);
    }
    let job = Job {
        links: vec![url.clone()],
        local_id: None,
        kind: JobKind::Sweep,
        last_updated_at: None,
        link_since,
    };
    let orch = build_orchestrator(cfg, store.clone(), false).await?;
    let report = orch.process_job(job).await;

    // 判定（对齐 sweep::finish_batch 的单号逻辑）。
    if report.finished_bizs.iter().any(|b| b == biz) {
        let latest = store.latest_published_for(biz)?;
        store.mark_sweep_checked(biz, "ok", "", latest)?;
        let msg = format!(
            "{label} 重新巡检完成（接力换 key 后采集）：新增 {} 篇{}",
            report.new_articles,
            latest
                .map(|t| format!("，最新发布 {}", applog::format_local(t)))
                .unwrap_or_default()
        );
        applog::info(Stage::Sweep, msg.clone());
        return Ok(RetryOutcome {
            collected: true,
            relayed: true,
            message: msg,
            total: report.urls.len(),
            new_articles: report.new_articles,
            last_published_at: latest,
        });
    }
    let hit_verify = report.verify_url.as_deref() == Some(url.as_str());
    let isolated = report.isolated_links.contains(&url);
    let never_opened = report.unopened_links.contains(&url);
    let captured = report.captured_bizs.iter().any(|b| b == biz);
    let reason = if report.env_failure {
        format!(
            "环境故障：{}。请检查文件助手窗口是否被遮挡、种子消息是否仍在标定框内、微信是否在线",
            report
                .abort_reason
                .clone()
                .unwrap_or_else(|| "接力等待期间没有任何文章请求".to_string())
        )
    } else if hit_verify {
        "打开该篇文章命中验证页".to_string()
    } else if isolated {
        "打开该篇文章后页面无响应 / 不跳转（白屏）".to_string()
    } else if never_opened {
        report
            .abort_reason
            .clone()
            .unwrap_or_else(|| "接力没有打开该篇文章（超时 / 中止）".to_string())
    } else if !captured {
        "打开该篇文章后没抓到凭证（疑似已删除 / 设为隐私）".to_string()
    } else {
        report
            .abort_reason
            .clone()
            .unwrap_or_else(|| "凭证已到手但采集未完成（续期失败 / 采集异常）".to_string())
    };
    // 环境故障 / 限流不算该号失败（不覆盖上次状态），其余记 failed。
    if !report.env_failure && !report.rate_limited {
        store.mark_sweep_checked(biz, "failed", &reason, None)?;
    }
    let msg = format!("{label} 重新巡检未成功：{reason}");
    applog::warn(Stage::Sweep, msg.clone());
    Ok(RetryOutcome {
        collected: false,
        relayed: true,
        message: msg,
        total: report.urls.len(),
        new_articles: report.new_articles,
        last_published_at: None,
    })
}

/// 按 [`RealRunConfig`] 拼装真实依赖，产出可直接跑（一次或长驻）的 [`Orchestrator`]。
///
/// 抽出 `run_loop_real` / `sweep_retry_account` 的公共装配：CA 复用/落盘、Store（同时绑定为日志入库目标）、
/// 上报客户端、`capture_start/stop`（含系统代理 Drop 复位）、列表采集与上报闭包。
/// CA：优先复用固定路径下 gui 已装进系统信任库的那张（macOS 信任认「具体某张证书」，
/// 每次换新等于白装）；否则生成并落盘到固定路径，供 gui「一键安装证书」。
/// `build_orchestrator` 与手动抓凭证代理（[`crate::manualcap`]）共用。
pub(crate) fn load_or_create_ca(cfg: &RealRunConfig) -> Result<CaMaterial> {
    let cert_path = std::path::Path::new(&cfg.ca_cert_path);
    let key_path = std::path::Path::new(&cfg.ca_key_path);
    let ca = if !cfg.ca_cert_path.is_empty()
        && !cfg.ca_key_path.is_empty()
        && cert_path.exists()
        && key_path.exists()
    {
        let cert_pem = std::fs::read_to_string(cert_path)?;
        let key_pem = std::fs::read_to_string(key_path)?;
        applog::info(
            Stage::Proxy,
            format!("复用已安装的根证书：{}", cfg.ca_cert_path),
        );
        CaMaterial::from_pem(cert_pem, key_pem)
    } else {
        let ca = CaMaterial::generate()?;
        if !cfg.ca_cert_path.is_empty() && !cfg.ca_key_path.is_empty() {
            if let Some(parent) = cert_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Some(parent) = key_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(cert_path, ca.cert_pem.as_bytes())?;
            std::fs::write(key_path, ca.key_pem.as_bytes())?;
            applog::warn(
                Stage::Proxy,
                format!(
                    "已生成新根证书并落盘：{}，请到系统设置点「一键安装证书」信任它，否则抓不到凭证",
                    cfg.ca_cert_path
                ),
            );
        } else {
            applog::warn(
                Stage::Proxy,
                "已生成临时根证书（未指定 ca_cert_path/ca_key_path，不落盘、不持久信任）",
            );
        }
        ca
    };
    Ok(ca)
}

/// 打开运行配置指向的库（`db_path` 空则 `data/mpider.db`），并把它绑定为环节日志的入库目标。
fn open_store(cfg: &RealRunConfig) -> Result<Arc<Store>> {
    let db_path = if cfg.db_path.is_empty() {
        "data/mpider.db".to_string()
    } else {
        cfg.db_path.clone()
    };
    if let Some(parent) = std::path::Path::new(&db_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let store = Arc::new(Store::open(&db_path)?);
    // 凭证热表的 expires_at 按同一 TTL 预估。
    store.set_cred_ttl(cfg.cred_ttl_seconds);
    // 环节日志入库到同一个库（GUI 启动时已绑定同一路径的 store；这里再绑一次保证 CLI 也入库）。
    if applog::bus().store().is_none() {
        applog::bus().set_store(Some(store.clone()));
    }
    Ok(store)
}

async fn build_orchestrator(
    cfg: &RealRunConfig,
    store: Arc<Store>,
    sweep_on: bool,
) -> Result<Orchestrator> {
    let ca = load_or_create_ca(cfg)?;

    // 上次进程的整机退避（被封 / 限流）从库里接着算，重启不清零（GUI 启动已恢复过一次，这里是 CLI 兜底）。
    crate::runstate::cooldown_restore_logged(&store);
    let relay = RelayQueue::new();

    // 飞书通知：按本次配置（幂等）。
    crate::notify::configure(cfg.feishu_config());

    // 结果上报客户端（未启用时 report 恒返回 skipped）
    let reporter = HttpReporter::new(cfg.report_config())?;
    if reporter.config().active() {
        applog::info(
            Stage::Report,
            format!(
                "结果上报已启用：每个号列表采完即 POST 到 {}",
                reporter.config().url
            ),
        );
    }

    // 固定种子入口链接：本机种子入口服务的地址（用户首次手动发进文件助手，RPA 每批点它）。
    let seed_url = seedserver::seed_url(&cfg.seed_host, cfg.seed_port);
    // 人工模式（mac / 关闭 RPA）：待命页自轮询，接力末条回落到种子页（见 seedserver 模块文档）。
    let resident = get_controller(cfg.rpa_enabled).manual();
    seedserver::set_resident_mode(resident);
    // 种子入口服务常驻（GUI 启动时通常已起）：按本次配置确保在跑、更新停留参数。起不来（端口被占）种子链接
    // 就点不开、整批接力无从谈起，在这里就报错，比等到起代理时才发现早。
    seedserver::apply(&cfg.seed_host, cfg.seed_port, cfg.seed_dwell())?;

    // capture start/stop：真起 CaptureProxy(共享 relay) + 可选系统代理(Drop 复位)；种子服务常驻，只接 / 摘队列
    let proxy_cell: Arc<tokio::sync::Mutex<Option<CaptureProxy>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    let guard_cell: Arc<std::sync::Mutex<Option<SystemProxyGuard<SysProxyApplier>>>> =
        Arc::new(std::sync::Mutex::new(None));

    let capture_start: CaptureStartFn = {
        let ca = ca.clone();
        let store = store.clone();
        let relay = relay.clone();
        let pc = proxy_cell.clone();
        let gc = guard_cell.clone();
        let service = cfg.sysproxy_service.clone();
        let set_sysproxy = cfg.set_sysproxy;
        let port = cfg.capture_port;
        let dwell = cfg.relay_dwell_ms;
        let dwell_max = cfg.relay_dwell_max_ms;
        let seed_dwell = cfg.seed_dwell_ms;
        let seed_url = seed_url.clone();
        let seed_host = cfg.seed_host.clone();
        let seed_port = cfg.seed_port;
        let seed_dwell_cfg = cfg.seed_dwell();
        let resident_home = resident.then(|| seed_url.clone());
        Arc::new(move || {
            let resident_home = resident_home.clone();
            let (ca, store, relay, pc, gc, service, seed_url, seed_host) = (
                ca.clone(),
                store.clone(),
                relay.clone(),
                pc.clone(),
                gc.clone(),
                service.clone(),
                seed_url.clone(),
                seed_host.clone(),
            );
            Box::pin(async move {
                // 常驻的种子入口服务：确保在跑（被停了 / 端口被占就在这里报错，该批交回上层）。
                // 接力队列要等 MITM 起来、系统代理设好之后再接上（见本闭包末尾）：待命页的长轮询一拿到首条就跳，
                // 早接会让首条文章在代理生效前发出、抓不到凭证（2026-09-11 真机日志里差了不到 1 秒）。
                seedserver::apply(&seed_host, seed_port, seed_dwell_cfg)?;
                let relay_for_seed = relay.clone();
                let proxy = capture::start_on(
                    CaptureConfig {
                        ca,
                        store,
                        relay_enabled: true,
                        relay_dwell_ms: dwell,
                        relay_dwell_max_ms: dwell_max,
                        seed_dwell_ms: seed_dwell,
                        decider: PassthroughDecider::new(),
                        relay,
                        // 固定入口链接（本机种子服务）：页面已自带接力脚本；种子 host 配成局域网 IP
                        // 时请求会经代理，这里按 host 识别入口页兜底（已注入的页不会二次注入）。
                        bootstrap_seed: Some(seed_url),
                        trace_requests: false,
                        resident_home,
                    },
                    &format!("127.0.0.1:{port}"),
                )
                .await?;
                let bound = proxy.addr;
                applog::info(Stage::Proxy, format!("抓凭证代理已启动，监听 {bound}"));
                *pc.lock().await = Some(proxy);
                crate::runstate::set_capture_addr(Some(bound.to_string()));
                let host = "127.0.0.1";
                let port = bound.port();
                // service 只在 macOS 分支用到；Windows 下避免未使用告警。
                #[cfg(windows)]
                let _ = &service;
                if set_sysproxy {
                    #[cfg(windows)]
                    let res = SystemProxyGuard::set(
                        crate::sysproxy::WinInetApplier,
                        crate::sysproxy::win_commands(true, host, port),
                        crate::sysproxy::win_commands(false, host, port),
                    );
                    #[cfg(not(windows))]
                    let res = SystemProxyGuard::set_mac(CommandApplier, host, port, &service);
                    match res {
                        Ok(g) => {
                            *gc.lock().unwrap() = Some(g);
                            applog::info(
                                Stage::Proxy,
                                format!(
                                    "系统代理已指向 {host}:{port}（任务结束 / 退出时自动复位）"
                                ),
                            );
                        }
                        Err(e) => applog::error(
                            Stage::Proxy,
                            format!("设置系统代理失败：{e}；微信流量不会经过本机代理，抓不到凭证"),
                        ),
                    }
                } else {
                    #[cfg(windows)]
                    applog::warn(
                        Stage::Proxy,
                        format!(
                            "未自动设系统代理(set_sysproxy=false)。手动：设 HKCU\\…\\Internet Settings 的 ProxyServer={host}:{port} 且 ProxyEnable=1"
                        ),
                    );
                    #[cfg(not(windows))]
                    applog::warn(
                        Stage::Proxy,
                        format!(
                            "未自动设系统代理(set_sysproxy=false)。手动：networksetup -setwebproxy \"{service}\" {host} {port}"
                        ),
                    );
                }
                // 代理链路就绪后才把本批队列接上种子服务：种子页 / 待命页现读队列，打开即跳本批首条。
                seedserver::attach(relay_for_seed);
                Ok(())
            })
        })
    };

    let capture_stop: CaptureStopFn = {
        let pc = proxy_cell.clone();
        let gc = guard_cell.clone();
        Arc::new(move || {
            let (pc, gc) = (pc.clone(), gc.clone());
            Box::pin(async move {
                // 先复位系统代理(Drop guard)，再停 MITM；种子入口服务常驻不停，只把本次队列摘下
                // （空闲时点开只给说明页，不会跳到残留链接）。
                if let Some(g) = gc.lock().unwrap().take() {
                    drop(g);
                }
                if let Some(p) = pc.lock().await.take() {
                    p.shutdown().await;
                }
                seedserver::detach();
                crate::runstate::set_capture_addr(None);
            })
        })
    };

    // 采集：真 getmsg（第一页 / 按最后更新时间翻页 / 续采），直连
    let collect_list_fn: CollectFn = {
        let store = store.clone();
        let ttl = cfg.cred_ttl_seconds;
        Arc::new(move |req: ListRequest| {
            let store = store.clone();
            Box::pin(async move { collect_list(&store, &req.biz, &req.plan, ttl, None).await })
        })
    };

    let report: ReportFn = {
        let r = reporter.clone();
        Arc::new(move |payload| {
            let r = r.clone();
            Box::pin(async move { r.report(&payload).await })
        })
    };

    crate::rpa::set_hard_refresh(cfg.rpa_hard_refresh);
    Ok(Orchestrator {
        store,
        relay,
        rpa: Arc::from(get_controller(cfg.rpa_enabled)),
        report,
        capture_start,
        capture_stop,
        collect_list: collect_list_fn,
        sleep: default_sleep(),
        cfg: OrchestratorConfig {
            relay_enabled: true,
            capture_wait_secs: cfg.capture_wait_seconds,
            cred_ttl_secs: cfg.cred_ttl_seconds,
            poll_secs: MAIN_LOOP_POLL_SECS,
            launch_timeout_secs: cfg.relay_launch_timeout_seconds,
            // 停滞阈值必须大于"停留上限 + 一页加载"，否则正常翻页也会被当停滞。
            stall_secs: cfg
                .relay_stall_seconds
                .max(effective_dwell_max_secs(cfg) + 8),
            max_relaunch: cfg.relay_max_relaunch,
            list_page_count: cfg.list_page_count,
            list_max_pages: cfg.list_max_pages,
            page_sleep_min_ms: cfg.page_sleep_min_ms,
            page_sleep_max_ms: cfg.page_sleep_max_ms,
            refresh_attempts: cfg.cred_refresh_attempts.max(1),
            refresh_wait_secs: cfg.cred_refresh_wait_seconds,
            refresh_rounds: cfg.cred_refresh_rounds,
            // 真机走固定入口页接力（本机种子服务地址，与 capture 的 bootstrap_seed 一致）。
            refresh_seed_url: Some(seed_url.clone()),
            capture_wait_per_link_secs: cfg.capture_wait_per_link_seconds,
            rate_retry_wait_ms: cfg.rate_retry_wait_ms,
            list_gap_min_ms: cfg.list_gap_min_ms,
            list_gap_max_ms: cfg.list_gap_max_ms,
            list_daily_budget: cfg.list_daily_budget.max(0),
            history_page_count: cfg.history_page_count,
            history_gap_secs: cfg.history_gap_seconds,
            history_budget_reserve: cfg.history_budget_reserve,
        },
        sweep: if sweep_on {
            Some(Sweep::new(SweepConfig {
                idle_secs: cfg.sweep_idle_seconds,
                batch_size: cfg.sweep_batch_size.clamp(1, 100),
                batch_gap_secs: cfg.sweep_batch_gap_seconds,
                fail_pause_secs: cfg.sweep_fail_pause_seconds,
                daily_budget: cfg.list_daily_budget.max(0),
                ..Default::default()
            }))
        } else {
            None
        },
    })
}

/// 实际停留上限（秒，向上取整）：`relay_dwell_max_ms` 小于下限时按下限算。
fn effective_dwell_max_secs(cfg: &RealRunConfig) -> u64 {
    let ms = cfg.relay_dwell_max_ms.max(cfg.relay_dwell_ms).max(0) as u64;
    ms.div_ceil(1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_real_run_config_deserialize_defaults() {
        // 部分字段的 JSON，其余走 default；老配置里的上游 / 代理池字段被忽略
        let cfg: RealRunConfig = serde_json::from_str(
            r#"{"report_url":"http://x","report_enabled":true,"upstream_base_url":"http://old","kp_proxy_enabled":true}"#,
        )
        .unwrap();
        assert_eq!(cfg.report_url, "http://x");
        assert!(cfg.report_enabled);
        assert_eq!(cfg.report_timeout_secs, 15);
        assert!(cfg.report_config().active());
        assert!(!RealRunConfig::default().report_config().active());
        assert_eq!(cfg.relay_dwell_ms, 2500); // 默认
        assert_eq!(cfg.relay_dwell_max_ms, 4000);
        assert_eq!(cfg.seed_dwell_ms, 100);
        assert!(!cfg.rpa_hard_refresh); // 默认不硬刷
        assert_eq!(cfg.seed_host, "127.0.0.1"); // 默认：种子服务只绑本机
        assert_eq!(cfg.seed_port, 8787);
        assert_eq!(cfg.relay_stall_seconds, 15);
        assert_eq!(cfg.relay_max_relaunch, 3);
        assert_eq!(cfg.list_max_pages, 50);
        assert_eq!(cfg.cred_refresh_attempts, 3);
        assert!(cfg.page_sleep_max_ms >= cfg.page_sleep_min_ms);
        // 停滞阈值下限：停留上限 4s + 8s = 12s < 15s → 取 15
        assert_eq!(effective_dwell_max_secs(&cfg), 4);
        assert_eq!(cfg.sysproxy_service, "Wi-Fi"); // 默认
        assert!(!cfg.set_sysproxy);
    }
}
