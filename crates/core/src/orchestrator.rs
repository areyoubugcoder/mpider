//! 自动化编排（服务层的"心脏"）—— 把领取任务（定时巡检批次 / 历史抓取的下一页）、
//! RPA 开微信、脚本注入接力抓凭证、列表采集（第一页 / 按「最后更新时间」翻页）、集中续期、结果上报
//! 串成一条自动流水。
//!
//! 一条任务(job)的处理流程：
//! 0. **凭证复用**（2026-09-04，Python 无对应）：长链能解析出 `__biz` 的，先查热数据
//!    （`credentials` 表，[`Store::credential_is_fresh`]：有 key、未过 TTL、未被实测失效）——
//!    凭证仍可用的号**不进接力队列**，跳过起代理 / RPA 直接采；只有短链和凭证不可用的号才走
//!    下面 1–4 的接力抓凭证。整批都能复用时**完全不碰**代理、系统代理与微信窗口。
//!    复用的凭证若采集时被微信判 `ret=-3`，与翻页途中过期同样处理：进「集中续期」（此时再按需
//!    起代理 + RPA 换新凭证），续上后从中断 offset 续采。
//! 1. 落库(jobs，含任务级/链接级「最后更新时间」) + 写**进程内接力队列**([`RelayQueue`])。
//! 2. 起抓凭证代理（`capture_start`：MITM + 系统代理，通过注入的闭包）。
//! 3. RPA 打开"种子链接"（本机种子入口服务 `http://<seed_host>:<seed_port>/`）→ 微信内置浏览器加载它 →
//!    代理注入接力脚本跳到本批首条任务文章 → 逐条 `/s` 抓 key/uin/token + 注入接力自动翻页。
//! 4. 等接力队列清空 / 各号凭证新鲜（`capture_wait` 超时兜底）。期间**看门狗**盯着代理层的
//!    心跳（最后一次 `/s` 请求时间，[`RelayQueue::last_request_at`]）：点种子后一直没请求 →
//!    判"点空"重点一次；接力中长时间无请求 → 在途那条先放回队尾重试、再卡就隔离，然后重点
//!    种子续跑剩余队列（有进展的重拉不占配额）；命中验证页 → 整批中止。对齐参考项目
//!    wx-shortlink-worker 的状态机（`core/state_machine.py` 停滞检测循环）。
//! 5. 逐号采列表（`collect_list`）：该号**没有**「最后更新时间」只采第 1 页；**有**则翻页直到
//!    某页出现早于它的文章（页间随机等待）。途中凭证过期的号**先记下 offset 放一边**，本轮其它
//!    号采完后，把过期号**集中一次续期**（[`crate::credrefresh::CredentialRefresher`]：一次接力
//!    把整批过期号一起续掉，≥3 次重试），再各自从中断的 offset 续采；最多 `refresh_rounds` 轮。
//!    **每个号列表采完就当场上报**（[`Orchestrator::report_account`]）：一条 [`ReportPayload`] POST 到用户
//!    配置的 HTTP 服务（`report.rs`；未启用则跳过），不等整批；按时间比对后没有新文章也报空数组；列表
//!    **没正常获取到**的号（没打开 / 没凭证 / 续期失败 / 限流 / 异常）**不上报**。上报走 `ReportFn`。
//! 6. 汇总落库：本地结果 `{"urls": [...本次各号各页文章链接，不去重], "truncated", "remaining_links",
//!    "accounts": [{biz, links, urls, finished, reported, report_error}...]}` 落 `jobs.result_json`；
//!    各号上报的 ack 合成任务级 `report_ack`（有一个号上报失败即「服务端错误」；未启用上报为 `None`）
//!    → jobs 置 reported / error。
//! 7. 收尾：关内置浏览器 + 停代理 + 清队列（即便出错也走；没开过的不关、没起过的不停）。
//!
//! 依赖全部**可注入**（report/rpa/capture start-stop/collect/sleep/relay），便于用假对象
//! 脱网单测，也便于真机入口注入真实实现（见 `runner.rs`）。

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::applog::{self, Stage};
use crate::collector::{
    is_account_blocked_err, is_credential_expired_err, is_rate_limited_err, CollectOutcome,
    CollectedArticle, ListPlan, ListRequest,
};
use crate::credrefresh::{CredentialRefresher, RefreshConfig, RefreshTarget};
use crate::model::{
    AccountReport, Job, ReportAck, JOB_OUTCOME_ERROR, JOB_OUTCOME_OK, JOB_OUTCOME_SERVER_ERROR,
    JOB_OUTCOME_TIMEOUT,
};
use crate::phases;
use crate::proxy_addon::parse_s_url;
use crate::relay::{RelayQueue, StallAction};
use crate::report::{merge_acks, ReportArticle, ReportPayload};
use crate::rng;
use crate::rpa::{self, WeChatController};
use crate::runstate;
use crate::seedserver;
use crate::store::Store;
use crate::sweep::Sweep;

type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// 一次任务的聚合结果 = 本地结果 `result`（GUI/CLI 同用；落 `jobs.result_json`）。
///
/// `urls` 是本次各号各页采到的文章链接按采集顺序平铺（**不去重**，去重由本地库
/// `UNIQUE(biz,mid,idx)` 与接收方各自负责）；`accounts` 是同一批结果**按公众号分组**的视图。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Report {
    /// 本次采到的全部文章链接（按号、按页顺序平铺；不去重）。
    #[serde(default)]
    pub urls: Vec<String>,
    /// 按公众号分组的结果：任务里每条链接按 `__biz` 归组（短链没打开过的按链接本身单独一组），
    /// **任务里每个号都有一条**——没打开 / 没凭证 / 没采完的号 `urls` 为空、`finished=false`。
    /// 只有 `finished=true` 的号上报过（采完当场报），`reported` 记是否被接受。
    #[serde(default)]
    pub accounts: Vec<AccountReport>,
    /// 本批没跑完（命中验证页 / 重拉耗尽 / 等待超时 / 有号续期失败或采集异常）：
    /// `remaining_links` 里是没打开或没采完的任务链接。
    #[serde(default)]
    pub truncated: bool,
    /// 未打开的链接（含两次停滞后被隔离的）+ 没采完的号对应的任务链接。`truncated=false` 时为空。
    #[serde(default)]
    pub remaining_links: Vec<String>,
    /// —— 以下为进程内诊断字段，**不进回报载荷**（`serde(skip)`），供本地巡检据此写回各号状态 ——
    /// 接力 / 续期被看门狗中止的原因（验证页 / 重拉耗尽）。
    #[serde(skip)]
    pub abort_reason: Option<String>,
    /// 命中验证页的那条链接（中止原因为验证页时有值）。
    #[serde(skip)]
    pub verify_url: Option<String>,
    /// 采集阶段撞到限流信号（已触发整机退避，剩余号没采）。
    #[serde(skip)]
    pub rate_limited: bool,
    /// 环境故障：整个接力等待期间**没有任何文章请求**（RPA 点不开种子 / 窗口被关 / 代理没抓到流量）。
    /// 与号本身无关——巡检据此暂停一段时间而不是把这批号计为失败。
    #[serde(skip)]
    pub env_failure: bool,
    /// 本次成功采完（含翻页）的号。
    #[serde(skip)]
    pub finished_bizs: Vec<String>,
    /// 本次采到的**新增**文章数（各号 `CollectOutcome::new` 之和）。
    #[serde(skip)]
    pub new_articles: usize,
    /// 接力结束时**从未被打开**的任务链接（等待到上限 / 中止时队列里还没轮到的）。
    /// 巡检据此区分「链接本身打不开」与「这批没轮到它」：后者不算该号失败、不换链接。
    #[serde(skip)]
    pub unopened_links: Vec<String>,
    /// 两次停滞后被看门狗隔离的链接（页面白屏 / 不跳转），巡检视为该号取样链接不可用。
    #[serde(skip)]
    pub isolated_links: Vec<String>,
    /// 进入采集阶段时凭证可用的号（复用 ∪ 本批接力抓到）。取样链接打开了但号不在这里 =
    /// 那篇文章打不开换凭证（已删除 / 设为隐私 / 白屏），巡检换下一篇再接力。
    #[serde(skip)]
    pub captured_bizs: Vec<String>,
    /// 本次任务的本地 jobs 表 id（0 = 未入库）；巡检据此把批次结果写进 `run_log`。
    #[serde(skip)]
    pub job_id: i64,
    /// 本次任务开始时刻（`process_job` 入口；`run_log.started_at`）。
    #[serde(skip)]
    pub started_at: f64,
    /// 接力等待是按超时收场的：等到上限仍有链接没打开 / 点空、停滞重拉耗尽 / 环境故障。
    /// 任务列表据此把 `truncated` 分成「超时」与「其它错误」。
    #[serde(skip)]
    pub timed_out: bool,
    /// 各号逐条上报合成的任务级结果（未启用上报 / 一个号都没上报为 `None`）；任一号非 2xx / 网络错误
    /// 即任务列表的「服务端错误」。
    #[serde(skip)]
    pub report_ack: Option<ReportAck>,
}

impl Report {
    /// 任务列表的结果分类 + 错误原因（`jobs.outcome` / `jobs.feedback`）。
    /// 优先级：回报失败（服务端错误）> 接力中止 / 未跑完（按 `timed_out` 分超时或错误）> 限流 > 正常。
    pub fn classify(&self) -> (&'static str, Option<String>) {
        if let Some(ack) = &self.report_ack {
            if !ack.ok {
                return (
                    JOB_OUTCOME_SERVER_ERROR,
                    Some(format!(
                        "上报失败：{}",
                        ack.error.as_deref().unwrap_or("上报服务未返回成功状态")
                    )),
                );
            }
        }
        if let Some(reason) = &self.abort_reason {
            let kind = if self.timed_out {
                JOB_OUTCOME_TIMEOUT
            } else {
                JOB_OUTCOME_ERROR
            };
            return (kind, Some(reason.clone()));
        }
        if self.rate_limited {
            return (
                JOB_OUTCOME_ERROR,
                Some("采集撞到限流信号，已整机退避，剩余号未采".to_string()),
            );
        }
        if self.truncated {
            if self.timed_out {
                return (
                    JOB_OUTCOME_TIMEOUT,
                    Some(format!(
                        "等待凭证到上限，{} 条链接未打开，交回 {} 条",
                        self.unopened_links.len(),
                        self.remaining_links.len()
                    )),
                );
            }
            return (
                JOB_OUTCOME_ERROR,
                Some(format!(
                    "本批未跑完（续期失败 / 采集异常），交回 {} 条链接",
                    self.remaining_links.len()
                )),
            );
        }
        (JOB_OUTCOME_OK, None)
    }
}

/// 上报：一条 [`ReportPayload`]（一个号）→ 上报结果（是否 2xx + 状态码；未启用上报返回 `skipped`）。
pub type ReportFn = Arc<dyn Fn(ReportPayload) -> BoxFut<ReportAck> + Send + Sync>;
/// 起代理（含系统代理）：成功/失败。
pub type CaptureStartFn = Arc<dyn Fn() -> BoxFut<anyhow::Result<()>> + Send + Sync>;
/// 停代理（含系统代理复位）。
pub type CaptureStopFn = Arc<dyn Fn() -> BoxFut<()> + Send + Sync>;
/// 采某号文章列表（按 [`ListRequest`] 的计划：第一页 / 翻页 / 续采）。
pub type CollectFn =
    Arc<dyn Fn(ListRequest) -> BoxFut<anyhow::Result<CollectOutcome>> + Send + Sync>;
/// 休眠。
pub type SleepFn = Arc<dyn Fn(Duration) -> BoxFut<()> + Send + Sync>;

/// 默认休眠实现（tokio）。
pub fn default_sleep() -> SleepFn {
    Arc::new(|d| Box::pin(async move { tokio::time::sleep(d).await }))
}

/// 从长链接里解析出涉及的 `__biz`（短链无 __biz，靠抓取时发现）。保序去重。
pub fn bizs_from_links(links: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for u in links {
        if let Some(art) = parse_s_url(u) {
            if seen.insert(art.biz.clone()) {
                out.push(art.biz);
            }
        }
    }
    out
}

/// 编排配置。
#[derive(Clone, Debug)]
pub struct OrchestratorConfig {
    pub relay_enabled: bool,
    pub capture_wait_secs: u64,
    pub cred_ttl_secs: i64,
    /// 主循环空转时（历史下一页没到点、巡检没到点）的轮询间隔（秒）。
    pub poll_secs: u64,
    /// 点种子后多少秒内没有任何心跳（入口页主文档或 `/s` 文章请求）判定"点空"，重点一次
    /// （占 `max_relaunch` 配额）。入口页加载即计一次心跳，故正常点开后此计时会立即复位。
    pub launch_timeout_secs: u64,
    /// 接力进行中多少秒无 `/s` 请求判定停滞。要大于"停留上限 + 一页加载时间"。
    pub stall_secs: u64,
    /// 无进展重拉（点空 / 窗口被关）的最大次数；有进展的重拉（在途放回队尾 / 隔离）不占配额。
    pub max_relaunch: u32,
    /// 列表 getmsg 每页条数。
    pub list_page_count: i64,
    /// 有「最后更新时间」时单号单次最多翻页数（安全阀；到顶按已完成处理并告警）。
    pub list_max_pages: usize,
    /// 翻页之间随机等待的下限 / 上限（毫秒）。
    pub page_sleep_min_ms: u64,
    pub page_sleep_max_ms: u64,
    /// 一次集中续期最多尝试几轮（产品要求 ≥ 3）。
    pub refresh_attempts: u32,
    /// 续期每轮等新 key 的上限（秒）。
    pub refresh_wait_secs: u64,
    /// 同一批任务里最多做几次「集中续期 → 续采」循环（每次续期约给 30 分钟）。
    pub refresh_rounds: u32,
    /// 续期时 RPA 点的固定种子入口（真机 = 本机种子服务地址 `seedserver::seed_url`；`None` 点取样链接）。
    pub refresh_seed_url: Option<String>,
    /// 等凭证 / 等续期的上限按**每条链接**再给的秒数：实际等待 = `max(固定上限, 链接数 × 本值)`。
    /// 巡检一批几十条链接时，固定的 120s / 60s 不够接力跑完（每跳停留 2.5–4s）。
    pub capture_wait_per_link_secs: u64,
    /// 撞限流信号后重试同一页前的等待（毫秒），见 [`ListPlan::rate_retry_wait_ms`]。
    pub rate_retry_wait_ms: u64,
    /// 全局 `getmsg` 闸门：任意两次列表请求之间的随机间隔区间（毫秒），见 [`ListPlan::gap_min_ms`]。
    pub list_gap_min_ms: u64,
    pub list_gap_max_ms: u64,
    /// 每号列表请求预算（当前微信号近 24 小时 `getmsg` 上限；0 = 不限）：达到后历史页与巡检都不领，
    /// 等最早一次请求滑出窗口（`runstate::list_budget_check`）。库级默认 0（单测 / 示例不受影响），
    /// GUI 运行配置默认 180（`RealRunConfig`）。
    pub list_daily_budget: i64,
    /// 抓取历史文章：每页条数 / 页间隔（秒）/ 为巡检保留的预算（见 [`crate::history`]）。
    pub history_page_count: i64,
    pub history_gap_secs: u64,
    pub history_budget_reserve: i64,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            relay_enabled: true,
            capture_wait_secs: 120,
            cred_ttl_secs: 30 * 60,
            poll_secs: 5,
            launch_timeout_secs: 10,
            stall_secs: 15,
            max_relaunch: 3,
            list_page_count: 10,
            list_max_pages: 50,
            page_sleep_min_ms: 3000,
            page_sleep_max_ms: 8000,
            refresh_attempts: 3,
            refresh_wait_secs: 60,
            refresh_rounds: 3,
            refresh_seed_url: None,
            capture_wait_per_link_secs: 8,
            rate_retry_wait_ms: 15_000,
            list_gap_min_ms: 8_000,
            list_gap_max_ms: 20_000,
            list_daily_budget: 0,
            history_page_count: 10,
            history_gap_secs: 60,
            history_budget_reserve: 30,
        }
    }
}

impl OrchestratorConfig {
    /// 等接力抓凭证的实际上限（秒）：固定上限与「链接数 × 每条秒数」取大。
    pub fn capture_wait_for(&self, n_links: usize) -> u64 {
        self.capture_wait_secs
            .max((n_links as u64).saturating_mul(self.capture_wait_per_link_secs))
    }

    /// 续期每轮等新 key 的实际上限（秒）：同上。
    pub fn refresh_wait_for(&self, n_targets: usize) -> u64 {
        self.refresh_wait_secs
            .max((n_targets as u64).saturating_mul(self.capture_wait_per_link_secs))
    }
}

/// `wait_captures` 的结果：`None` 正常完成 / 等到上限；`Some(原因)` 被看门狗中止。
struct WaitOutcome {
    aborted: Option<String>,
    /// 整个等待期间没有任何文章请求（点空重拉耗尽 / 窗口被关 / 等到上限都没心跳）——环境故障。
    env_failure: bool,
    /// 按超时收场：等到上限 / 点空、停滞重拉耗尽（验证页中止与正常完成为 false）。
    timed_out: bool,
}

/// 采集阶段里一个"还没采完"的号（首采 offset=0；过期中断后记下续采 offset）。
#[derive(Clone, Debug)]
struct PendingAccount {
    biz: String,
    offset: i64,
}

/// 采集阶段的汇总。
struct CollectSummary {
    /// 全部文章链接（按号、按页顺序平铺；不去重）。
    urls: Vec<String>,
    /// 按号分组的文章链接（`biz` → 本次各页链接，不去重）；回报逐号提交用。
    collected: HashMap<String, Vec<String>>,
    /// 没采完的号（续期失败 / 采集异常 / 续期轮次用尽）。
    unfinished: Vec<String>,
    /// 用了几次集中续期。
    refresh_rounds_used: u32,
    /// 采集途中撞到限流信号（已触发整机退避；未采的号全部计入 `unfinished`）。
    rate_limited: bool,
    /// 采完（含翻页到底 / 翻页达上限按完成处理）的号。
    finished: Vec<String>,
    /// 新增文章数。
    new_articles: usize,
    /// 各号**采完当场上报**的结果（`biz` → ack；未启用上报为 `skipped`）。
    acks: HashMap<String, ReportAck>,
}

/// 一条任务期间「抓凭证基础设施」的开关状态：代理是否已起、内置浏览器是否被 RPA 打开过。
/// 凭证复用后这两样都是**按需**才开（整批复用时全程不开），收尾据此决定要不要停 / 关。
#[derive(Debug, Default)]
struct CaptureState {
    /// `capture_start` 已成功（收尾要 `capture_stop`）。
    proxy_on: bool,
    /// RPA 打开过内置浏览器（收尾要 `close_browser`）。
    browser_on: bool,
}

/// 凭证复用划分结果：哪些号直接用热数据里的凭证采，哪些链接还得接力抓。
#[derive(Debug, Default, PartialEq, Eq)]
struct ReusePlan {
    /// 热数据里凭证仍可用的号（保序去重）——不进接力队列，直接采。
    reuse_bizs: Vec<String>,
    /// 仍需接力抓凭证的链接（短链 + 凭证不可用的长链），保持任务里的顺序。
    capture_links: Vec<String>,
}

/// 编排器。所有外部交互都通过注入的字段完成（便于脱网单测 / 真机换真实现）。
pub struct Orchestrator {
    pub store: Arc<Store>,
    pub relay: RelayQueue,
    pub rpa: Arc<dyn WeChatController>,
    pub report: ReportFn,
    pub capture_start: CaptureStartFn,
    pub capture_stop: CaptureStopFn,
    pub collect_list: CollectFn,
    pub sleep: SleepFn,
    pub cfg: OrchestratorConfig,
    /// 本地定时巡检（`None` = 历史模式，只推进历史任务）。主循环向它要下一批合成任务。
    pub sweep: Option<Arc<Sweep>>,
}

impl Orchestrator {
    /// 记一条关键节点日志（入库 + 推给 GUI；见 [`crate::applog`]）。
    fn emit(&self, stage: Stage, msg: String) {
        applog::info(stage, msg);
    }

    /// 记一条告警（入库 + 推给 GUI）。
    fn warn(&self, stage: Stage, msg: String) {
        applog::warn(stage, msg);
    }

    /// 处理一条巡检任务（批次 / 单号重新巡检），返回聚合结果 [`Report`]。
    /// 任务带 `local_id`（已落库）直接复用那一行；否则新建一行。
    pub async fn process_job(&self, job: Job) -> Report {
        let job_id = match job.local_id {
            Some(id) => id,
            None => self.store.create_job_for(&job).unwrap_or(0),
        };
        // 任务期间所有环节的日志自动带上 job id（收尾时清）。
        applog::bus().set_job(Some(job_id));
        // 任务列表：起点计在这里（含起代理 / 点种子 / 等凭证 / 采集 / 回报 / 收尾全程）。
        let started_at = crate::model::now();
        let _ = self.store.set_job_started(job_id, started_at);
        // 阶段计时：领取任务 / 合成批次的耗时（prelude）在这里并入，之后各转折点 mark。
        phases::begin(job_id);
        let since_desc = match (job.last_updated_at, job.link_since.len()) {
            (None, 0) => "，无最后发布时间：各号只采首页".to_string(),
            (t, n) => format!(
                "，最后发布时间：任务级={}，链接级 {n} 条 → 按时间翻页",
                t.map(applog::format_local).unwrap_or_else(|| "无".into())
            ),
        };
        self.emit(
            Stage::Sweep,
            format!(
                "▶ 巡检批次开始：{} 个号（本地 job#{job_id}）{since_desc}",
                job.links.len()
            ),
        );
        if job.is_empty() {
            let _ = self.store.set_job_status(job_id, "error", "空任务");
            let _ = self.store.finish_job(
                job_id,
                crate::model::now(),
                JOB_OUTCOME_ERROR,
                Some("空任务"),
                None,
                &phases::finish(job_id),
            );
            self.warn(Stage::Task, "任务不含任何链接，按空任务跳过".to_string());
            applog::bus().set_job(None);
            return Report::default();
        }

        let mut cap = CaptureState::default();
        let mut result = self.run_job(&job, job_id, &mut cap).await;
        result.job_id = job_id;
        result.started_at = started_at;
        // 限流分析：run_log 由 sweep::finish_batch 按逐号判定后记（单号重新巡检由 runner 记）。

        // 7) 收尾（即便上面出错也执行）：只关开过的浏览器、只停起过的代理。
        if cap.browser_on {
            phases::mark("关闭浏览器");
            let (ok, msg) = self.rpa.close_browser();
            if ok {
                self.emit(
                    Stage::Rpa,
                    if msg.is_empty() {
                        "关闭微信内置浏览器".to_string()
                    } else {
                        format!("关闭微信内置浏览器：{msg}")
                    },
                );
            } else {
                self.warn(Stage::Rpa, format!("关闭微信内置浏览器失败：{msg}"));
            }
        }
        if cap.proxy_on {
            phases::mark("停止代理");
            (self.capture_stop)().await;
            self.emit(Stage::Proxy, "抓凭证代理已停止，系统代理已复位".to_string());
        }
        self.relay.clear();
        // 任务列表：终点计在收尾之后（耗时 = 整条任务占用代理 / 微信窗口的真实时长）。
        let finished_at = crate::model::now();
        let (outcome, error) = result.classify();
        let _ = self.store.finish_job(
            job_id,
            finished_at,
            outcome,
            error.as_deref(),
            result
                .report_ack
                .as_ref()
                .and_then(|a| a.http_status)
                .map(i64::from),
            &phases::finish(job_id),
        );
        self.emit(
            Stage::Sweep,
            format!(
                "任务结束（本地 job#{job_id}）：{}，耗时 {:.0}s{}",
                match outcome {
                    JOB_OUTCOME_OK => "正常",
                    JOB_OUTCOME_TIMEOUT => "超时",
                    JOB_OUTCOME_SERVER_ERROR => "服务端错误",
                    _ => "错误",
                },
                (finished_at - started_at).max(0.0),
                error
                    .as_deref()
                    .map(|e| format!("：{e}"))
                    .unwrap_or_default()
            ),
        );
        // 飞书：只报异常（超时 / 服务端错误 / 错误）。正常结束不推——任务与巡检批次都很密，
        // 每条都报会把真正的预警淹没；要看运行情况去 GUI 任务列表 / 日志页。
        if outcome != JOB_OUTCOME_OK {
            let what = format!("巡检批次（{} 个号，本地 job#{job_id}）", job.links.len());
            let verdict = match outcome {
                JOB_OUTCOME_TIMEOUT => "超时".to_string(),
                JOB_OUTCOME_SERVER_ERROR => "服务端错误（上报失败）".to_string(),
                _ => "错误".to_string(),
            };
            crate::notify::notify(
                crate::notify::Kind::Task,
                format!(
                    "{what}：{verdict}，耗时 {:.0}s{}{}",
                    (finished_at - started_at).max(0.0),
                    error
                        .as_deref()
                        .map(|e| format!("\n原因：{}", applog::redact(e)))
                        .unwrap_or_default(),
                    if result.truncated {
                        format!("\n未完成链接 {} 条", result.remaining_links.len())
                    } else {
                        String::new()
                    }
                ),
            );
        }
        applog::bus().set_job(None);
        result
    }

    /// 任务主体（落库之后、收尾之前）：凭证复用划分 → （按需）接力抓凭证 → 采集 → 上报。
    /// `cap` 记录本任务开过哪些基础设施，交给 [`Self::process_job`] 收尾。
    async fn run_job(&self, job: &Job, job_id: i64, cap: &mut CaptureState) -> Report {
        let expected = bizs_from_links(&job.links);
        // 本批「抓凭证窗口」的起点（epoch 秒）。短链批的收尾判据只认「本批窗口内当场抓到/刷新
        // 凭证的号」（`store.bizs_captured_since(window_start)`），而**不是**「30 分钟内还新鲜
        // 的全部号」——否则短链批会把此前遗留仍新鲜的无关号一并带上、误报。
        // 对齐参考项目 wx-shortlink-worker：等代理 on_captured 真观察到才收尾，而非一 drain 就返回。
        let window_start = crate::model::now();

        // 0) 凭证复用：热数据里凭证仍可用的号不进接力队列，直接采。
        phases::mark("凭证复用判定");
        let plan = self.plan_reuse(&job.links);
        let capture_expected = bizs_from_links(&plan.capture_links);

        let mut aborted: Option<String> = None;
        let mut env_failure = false;
        let mut timed_out = false;
        let mut verify_url: Option<String> = None;
        let mut remaining: Vec<String> = Vec::new();
        let mut isolated: Vec<String> = Vec::new();
        if plan.capture_links.is_empty() {
            self.emit(
                Stage::Capture,
                "本批各号凭证均可复用，跳过接力抓凭证（不起代理、不开微信窗口）".to_string(),
            );
        } else {
            let seed = plan.capture_links[0].clone();
            // 1) 写接力队列（relay 关时只放种子一条）
            let _ = self.store.set_job_status(job_id, "capturing", "");
            if self.cfg.relay_enabled {
                self.relay.set(&plan.capture_links);
            } else {
                self.relay.set(std::slice::from_ref(&seed));
            }

            // 2) 起代理
            phases::mark("启动代理");
            if let Err(e) = self.ensure_capture(cap).await {
                let _ = self
                    .store
                    .set_job_status(job_id, "error", &format!("代理启动失败：{e}"));
                self.warn(
                    Stage::Proxy,
                    "抓凭证代理启动失败，本批（含可复用凭证的号）整体记为未完成".to_string(),
                );
                return Report {
                    urls: Vec::new(),
                    truncated: true,
                    remaining_links: job.links.clone(),
                    abort_reason: Some(format!("代理启动失败：{e}")),
                    unopened_links: job.links.clone(),
                    ..Default::default()
                };
            }

            // 3) RPA 打开种子链接（先记时间：点击期间就可能有心跳）。
            //    人工模式且待命页在线：队列已在 capture_start 接上，待命页的长轮询会自己取首条跳走，不点、不打扰人。
            let launched_at = Instant::now();
            let resident = self.resident_takes_over();
            let rpa_ok = if resident { false } else { self.launch(&seed) };
            cap.browser_on = true;

            // 4) 等接力抓凭证（看门狗：点空重点 / 停滞重排 / 验证页中止）。上限按本批链接数伸缩；
            //    人工模式（mac / 关闭 RPA）且待命页不在线时再抬到 MANUAL_WAIT_SECS 下限并推全局提醒，
            //    让人来得及去微信点开一篇文章。
            let mut wait_secs = self.cfg.capture_wait_for(plan.capture_links.len());
            let manual_alert = if !rpa_ok && self.rpa.manual() && !resident {
                wait_secs = wait_secs.max(rpa::MANUAL_WAIT_SECS);
                let labels: Vec<String> = capture_expected
                    .iter()
                    .map(|b| self.store.account_label(b))
                    .collect();
                rpa::manual_open_alert(&labels, wait_secs)
            } else {
                None
            };
            let outcome = self
                .wait_captures(
                    &capture_expected,
                    window_start,
                    &seed,
                    rpa_ok,
                    launched_at,
                    wait_secs,
                )
                .await;
            // 等待结束（凭证到位 / 超时 / 中止）提醒即失效，别让「请打开一篇文章」挂在界面上误导。
            if let Some(id) = manual_alert {
                runstate::alert_ack(id);
            }
            aborted = outcome.aborted;
            env_failure = outcome.env_failure;
            timed_out = outcome.timed_out || env_failure;
            verify_url = self.relay.verify_hit();
            remaining = self.relay.pending_links();
            isolated = self.relay.timeouts();
            if let Some(reason) = &aborted {
                applog::error(
                    Stage::Capture,
                    format!(
                        "⛔ 接力中止：{reason}；剩余 {} 条本轮稍后重试",
                        remaining.len(),
                    ),
                );
            } else if !remaining.is_empty() {
                self.warn(
                    Stage::Capture,
                    format!(
                        "等待凭证到上限（{wait_secs}s），接力队列仍剩 {} 条未打开，本轮稍后重试",
                        remaining.len(),
                    ),
                );
            } else {
                self.emit(
                    Stage::Capture,
                    format!(
                        "接力完成：本批任务链接已全部打开，{} 个号凭证就绪",
                        self.captured_bizs_since(&capture_expected).len()
                    ),
                );
            }
            if !isolated.is_empty() {
                self.warn(
                    Stage::Capture,
                    format!(
                        "两次停滞被隔离 {} 条链接（记为未完成）：{isolated:?}",
                        isolated.len()
                    ),
                );
            }
        }

        // 5) 逐号采列表（第 1 页 / 按最后更新时间翻页），过期号集中续期后续采
        let _ = self.store.set_job_status(job_id, "collecting", "");
        // 待采的号 = 复用的号 ∪ 接力抓到的号（按任务链接顺序：`expected` 已含两者的长链号）。
        let captured = self.captured_bizs_since(&expected);
        let links_by_biz = self.links_by_biz(job);
        let since_by_biz = self.since_by_biz(job, &links_by_biz, &captured);
        // 命中验证页时不再做集中续期（续期也是接力，会再撞验证页）。
        let allow_refresh = aborted.is_none();
        phases::mark("采集文章列表");
        let summary = self
            .collect_all(
                job,
                job_id,
                &captured,
                &since_by_biz,
                &links_by_biz,
                allow_refresh,
                cap,
                crate::collector::ListSource::Sweep,
            )
            .await;

        // 6) 聚合落库（各号已在采完时当场上报，这里只合成任务级 ack、不再整批出网）
        let accounts = self.account_reports(job, &links_by_biz, &summary);
        let mut result = Report {
            urls: summary.urls,
            accounts,
            truncated: false,
            remaining_links: Vec::new(),
            abort_reason: aborted.clone(),
            verify_url,
            rate_limited: summary.rate_limited,
            env_failure,
            finished_bizs: summary.finished,
            new_articles: summary.new_articles,
            unopened_links: remaining.clone(),
            isolated_links: isolated.clone(),
            captured_bizs: captured.clone(),
            job_id,
            started_at: window_start,
            timed_out,
            report_ack: None,
        };
        let mut remaining_links = remaining;
        for u in isolated {
            if !remaining_links.contains(&u) {
                remaining_links.push(u);
            }
        }
        // 没采完的号：其任务链接一并记入 remaining_links。
        for biz in &summary.unfinished {
            for u in links_by_biz.get(biz).into_iter().flatten() {
                if !remaining_links.contains(u) {
                    remaining_links.push(u.clone());
                }
            }
        }
        if aborted.is_some() || !remaining_links.is_empty() {
            result.truncated = true;
            result.remaining_links = remaining_links;
        }
        // 各号采完时已各自上报（`report_account`）；这里按号合成任务级 ack 并汇总一行。
        let acks: Vec<(String, ReportAck)> = result
            .accounts
            .iter()
            .filter_map(|a| {
                let biz = a.biz.as_deref()?;
                let ack = summary.acks.get(biz)?;
                Some((self.store.account_label(biz), ack.clone()))
            })
            .collect();
        let ack = merge_acks(&acks);
        let reported_ok = result.accounts.iter().filter(|a| a.reported).count();
        let not_finished = result.accounts.iter().filter(|a| !a.finished).count();
        let empty = result
            .accounts
            .iter()
            .filter(|a| a.reported && a.urls.is_empty())
            .count();
        // 各号状态由 sweep 模块据 Report 写回 accounts；上报（若启用）已在采完时各自发出。
        {
            let status = match &ack {
                Some(a) if !a.ok => "error",
                _ => "done",
            };
            let result_value = serde_json::to_value(&result).unwrap_or_default();
            let _ = self.store.set_job_result(job_id, &result_value, status);
        }
        if ack.is_some() {
            self.emit(
                Stage::Report,
                format!(
                    "上报汇总（本地 job#{job_id}）：{reported_ok}/{} 个号已上报（共 {} 条文章链接{}）{}{}",
                    result.accounts.len(),
                    result.urls.len(),
                    if empty > 0 {
                        format!("，其中 {empty} 个号无新文章报了空数组")
                    } else {
                        String::new()
                    },
                    if not_finished > 0 {
                        format!("；{not_finished} 个号列表未正常获取，不上报")
                    } else {
                        String::new()
                    },
                    match &ack {
                        Some(a) if !a.ok => format!(
                            "；上报失败：{}（任务记为 error）",
                            a.error.as_deref().unwrap_or("上报服务未返回成功状态")
                        ),
                        _ => String::new(),
                    }
                ),
            );
        }
        result.report_ack = ack;
        self.emit(
            Stage::Sweep,
            format!(
                "✅ 巡检批次完成：{} 个号 / {} 条链接{}{}{}",
                captured.len(),
                result.urls.len(),
                if plan.reuse_bizs.is_empty() {
                    String::new()
                } else {
                    format!("（复用凭证 {} 个号）", plan.reuse_bizs.len())
                },
                if summary.refresh_rounds_used > 0 {
                    format!("（集中续期 {} 次）", summary.refresh_rounds_used)
                } else {
                    String::new()
                },
                if result.truncated {
                    format!("（未跑完，剩 {} 条）", result.remaining_links.len())
                } else {
                    String::new()
                },
            ),
        );
        result
    }

    /// 凭证复用划分：长链解析出 `__biz` 且热数据里凭证仍可用（[`Store::credential_is_fresh`]）
    /// 的号直接采；其余链接（短链、凭证不可用 / 从未抓过的号）进接力队列。同一号多条链接
    /// 按该号统一处理。透出一行「复用 / 需接力」概况。
    fn plan_reuse(&self, links: &[String]) -> ReusePlan {
        let mut plan = ReusePlan::default();
        let mut seen: HashSet<String> = HashSet::new();
        let mut need_capture_bizs: Vec<String> = Vec::new();
        let mut short_links = 0usize;
        for u in links {
            let Some(art) = parse_s_url(u) else {
                short_links += 1;
                plan.capture_links.push(u.clone());
                continue;
            };
            let fresh = self
                .store
                .credential_is_fresh(&art.biz, self.cfg.cred_ttl_secs)
                .unwrap_or(false);
            if fresh {
                if seen.insert(art.biz.clone()) {
                    plan.reuse_bizs.push(art.biz);
                }
            } else {
                if seen.insert(art.biz.clone()) {
                    need_capture_bizs.push(art.biz);
                }
                plan.capture_links.push(u.clone());
            }
        }
        if !plan.reuse_bizs.is_empty() {
            let detail = plan
                .reuse_bizs
                .iter()
                .map(|b| {
                    let left = self
                        .store
                        .get_credential(b)
                        .ok()
                        .flatten()
                        .map(|c| {
                            (c.captured_at + self.cfg.cred_ttl_secs as f64 - crate::model::now())
                                .max(0.0)
                        })
                        .unwrap_or(0.0);
                    format!(
                        "{}（约剩 {} 分钟）",
                        self.store.account_label(b),
                        (left / 60.0).round() as i64
                    )
                })
                .collect::<Vec<_>>()
                .join("、");
            self.emit(
                Stage::Capture,
                format!(
                    "♻ 凭证复用：{} 个号凭证仍可用，直接采集：{detail}",
                    plan.reuse_bizs.len()
                ),
            );
        }
        if !plan.capture_links.is_empty() {
            let mut parts = Vec::new();
            if !need_capture_bizs.is_empty() {
                parts.push(format!(
                    "{} 个号凭证不可用（未抓过 / 已过期 / 已失效）：{}",
                    need_capture_bizs.len(),
                    need_capture_bizs
                        .iter()
                        .map(|b| self.store.account_label(b))
                        .collect::<Vec<_>>()
                        .join("、")
                ));
            }
            if short_links > 0 {
                parts.push(format!("短链 {short_links} 条（无法预知号）"));
            }
            self.emit(
                Stage::Capture,
                format!(
                    "需接力抓凭证 {} 条链接：{}",
                    plan.capture_links.len(),
                    parts.join("；")
                ),
            );
        }
        plan
    }

    /// 按需起抓凭证代理（MITM + 系统代理）：已起过则直接返回。成功后置 `cap.proxy_on`，
    /// 收尾据此停代理。凭证复用后代理不再在任务开头无条件起，而是在**真要接力**（首抓 /
    /// 集中续期）时才起。
    async fn ensure_capture(&self, cap: &mut CaptureState) -> anyhow::Result<()> {
        if cap.proxy_on {
            return Ok(());
        }
        self.emit(
            Stage::Proxy,
            "启动抓凭证代理（MITM + 系统代理）…".to_string(),
        );
        match (self.capture_start)().await {
            Ok(()) => {
                cap.proxy_on = true;
                Ok(())
            }
            Err(e) => {
                applog::error(
                    Stage::Proxy,
                    format!("抓凭证代理启动失败：{e}；本批记为未完成"),
                );
                Err(e)
            }
        }
    }

    /// 本批「抓凭证窗口」内当场抓到/刷新过有效凭证的号数（`window_start` 起）——短链批收尾判据。
    fn captured_in_window(&self, window_start: f64) -> usize {
        self.store
            .bizs_captured_since(window_start)
            .map(|v| v.len())
            .unwrap_or(0)
    }

    /// 人工模式（`rpa.manual()`）且待命页不在线时把等待秒数抬到 `rpa::MANUAL_WAIT_SECS` 下限（要等人）；
    /// 自动模式、或待命页在线（机器自己接力）原样返回。
    fn manual_floor(&self, secs: u64) -> u64 {
        if self.rpa.manual() && !seedserver::resident_alive() {
            secs.max(rpa::MANUAL_WAIT_SECS)
        } else {
            secs
        }
    }

    /// 人工模式下待命页是否在线、本批交给它自动接力（透出一行结果）。自动点击模式恒为 false。
    fn resident_takes_over(&self) -> bool {
        if !(self.rpa.manual() && seedserver::resident_alive()) {
            return false;
        }
        phases::mark("接力抓凭证");
        self.emit(
            Stage::Rpa,
            "微信内置浏览器待命页在线，本批任务已交给它自动接力".to_string(),
        );
        true
    }

    /// RPA 点种子拉起内置浏览器（透出结果行）。返回 RPA 是否报告成功——失败（NoOp / mac 人工
    /// 模式）时看门狗不会自动重点，只按 `capture_wait_secs` 等（人工模式下限 `rpa::MANUAL_WAIT_SECS`，
    /// 见 `run` 第 4 步）。
    fn launch(&self, seed: &str) -> bool {
        // 阶段：点种子 → 之后总是进入接力等待（看门狗重点时会再次出现这对阶段）。
        phases::mark("打开微信浏览器");
        self.emit(Stage::Rpa, "点击种子链接，打开微信内置浏览器…".to_string());
        let (ok, msg) = self.rpa.open_seed(seed);
        if ok {
            self.emit(Stage::Rpa, format!("微信内置浏览器已打开：{msg}"));
        } else {
            self.warn(Stage::Rpa, format!("打开微信内置浏览器失败：{msg}"));
        }
        phases::mark("接力抓凭证");
        ok
    }

    /// 等接力队列清空且凭证到位；到 deadline 兜底返回。
    ///
    /// 完成判据分两种：
    /// - **长链批**（`expected` 非空）：接力跑完 **且** 每个号凭证都新鲜。
    /// - **短链批**（`expected` 为空）：URL 无 `__biz`，无法预知号；接力跑完 **且** 本批窗口
    ///   （`window_start` 起）内至少当场抓到一个有效凭证。否则一 drain 就返回，会在文章页
    ///   `getappmsgext`（带 key 的凭证请求）发出前就收尾，导致短链号被漏采。
    ///
    /// **看门狗**（仅接力开启、队列未清空、且 RPA 可自动点击时）：以代理层"最后一次 `/s`
    /// 请求"为心跳——
    /// - 点种子后 `launch_timeout_secs` 内没有请求：判"点空"，重点一次（占 `max_relaunch`）。
    /// - 接力中 `stall_secs` 无请求：在途那条第一次放回队尾、第二次隔离（`RelayQueue::requeue_inflight`），
    ///   然后重点种子续跑剩余队列（有进展，不占配额）；没有在途（窗口被关）则整体重点（占配额）。
    /// - 配额耗尽 → 中止；命中人机验证页 → 立即整批中止。
    ///
    /// 等待期间每约 5 秒透一次倒计时，让「运行中」不像卡死——凭证抓不到时最长等满
    /// `capture_wait_secs`。
    async fn wait_captures(
        &self,
        expected: &[String],
        window_start: f64,
        seed: &str,
        rpa_ok: bool,
        launched_at: Instant,
        wait_secs: u64,
    ) -> WaitOutcome {
        let deadline = Instant::now() + Duration::from_secs(wait_secs);
        let mut last_tick = u64::MAX;
        let first_launch = launched_at;
        let mut launched_at = launched_at;
        let mut relaunch_used: u32 = 0;
        let watchdog_on = self.cfg.relay_enabled && rpa_ok;
        loop {
            let pending = self.relay.pending_count();
            let fresh_cnt = expected
                .iter()
                .filter(|b| {
                    self.store
                        .credential_is_fresh(b, self.cfg.cred_ttl_secs)
                        .unwrap_or(false)
                })
                .count();
            let done = if expected.is_empty() {
                pending == 0 && self.captured_in_window(window_start) >= 1
            } else {
                pending == 0 && fresh_cnt == expected.len()
            };
            if done {
                return WaitOutcome {
                    aborted: None,
                    env_failure: false,
                    timed_out: false,
                };
            }
            // 验证页：整批中止（参考项目：命中验证码不再往下跳，剩余记为未完成）。
            if let Some(url) = self.relay.verify_hit() {
                return WaitOutcome {
                    aborted: Some(format!("命中人机验证页（{url}）")),
                    env_failure: false,
                    timed_out: false,
                };
            }
            // 代理层请求中止（登录的微信号与激活的不一致等）。
            if let Some(reason) = self.relay.abort_reason() {
                return WaitOutcome {
                    aborted: Some(reason),
                    env_failure: false,
                    timed_out: false,
                };
            }
            let now = Instant::now();
            // 整个等待期间一次文章请求都没有 = 环境故障（RPA 点不开 / 窗口被关 / 代理没流量）。
            let no_heartbeat_at_all = self
                .relay
                .last_request_at()
                .map(|t| t < first_launch)
                .unwrap_or(true);
            if now >= deadline {
                return WaitOutcome {
                    aborted: None,
                    env_failure: no_heartbeat_at_all,
                    timed_out: true,
                };
            }

            // 看门狗：只在接力还有活干时盯心跳。
            if watchdog_on && pending > 0 {
                let beat = self.relay.last_request_at();
                let since_launch = beat.map(|t| t < launched_at).unwrap_or(true);
                let idle = beat.filter(|t| *t >= launched_at).map(|t| now - t);
                let mut relaunch: Option<(String, bool)> = None; // (原因, 占配额?)
                if since_launch {
                    if now - launched_at >= Duration::from_secs(self.cfg.launch_timeout_secs) {
                        relaunch = Some(("点种子后无任何文章请求（疑似点空）".to_string(), true));
                    }
                } else if idle.unwrap_or_default() >= Duration::from_secs(self.cfg.stall_secs) {
                    match self.relay.requeue_inflight() {
                        StallAction::Requeued(u) => {
                            relaunch =
                                Some((format!("接力停滞，在途链接放回队尾重试一次：{u}"), false))
                        }
                        StallAction::Isolated(u) => {
                            relaunch = Some((format!("接力再次停滞，隔离该链接：{u}"), false))
                        }
                        StallAction::NoInflight => {
                            relaunch =
                                Some(("接力停滞且无在途链接（窗口被关？）".to_string(), true))
                        }
                    }
                }
                if let Some((reason, counted)) = relaunch {
                    if counted {
                        relaunch_used += 1;
                        if relaunch_used > self.cfg.max_relaunch {
                            return WaitOutcome {
                                aborted: Some(format!(
                                    "{reason}；重拉 {} 次仍无进展",
                                    self.cfg.max_relaunch
                                )),
                                env_failure: no_heartbeat_at_all,
                                timed_out: true,
                            };
                        }
                    }
                    self.warn(
                        Stage::Capture,
                        format!(
                            "⚠ {reason}；重点种子续跑（剩余 {pending} 条{}）",
                            if counted {
                                format!("，重拉 {relaunch_used}/{}", self.cfg.max_relaunch)
                            } else {
                                String::new()
                            }
                        ),
                    );
                    launched_at = Instant::now();
                    if !self.launch(seed) {
                        warn!("重点种子失败，等待上限兜底");
                    }
                }
            }

            let remain = (deadline - now).as_secs();
            // 每 5 秒透一次（remain 每秒变一次，200ms 轮询里靠 last_tick 去重）
            // 倒计时是瞬态行：只推给 GUI，不入库（见 applog::progress）。
            if remain != last_tick && remain.is_multiple_of(5) {
                if expected.is_empty() {
                    applog::progress(
                        Stage::Capture,
                        format!(
                            "等待凭证… 剩余 {remain}s（接力队列 {pending} 条，本批已抓 {} 号）",
                            self.captured_in_window(window_start)
                        ),
                    );
                } else {
                    applog::progress(
                        Stage::Capture,
                        format!(
                            "等待凭证… 剩余 {remain}s（接力队列 {pending} 条，已就绪 {fresh_cnt}/{} 号）",
                            expected.len()
                        ),
                    );
                }
                last_tick = remain;
            }
            (self.sleep)(Duration::from_millis(200)).await;
        }
    }

    /// 本批要采/回报的号：链接里解析到的号（`expected`）∪ **接力队列各项实际解析到的号**
    /// （`relay.resolved_bizs()`），保序去重。
    ///
    /// **只认下发任务链接解析到的号**——种子入口页不在接力队列里、散页/误开也不会被
    /// `advance_with_biz` 登记，故种子号与遗留新鲜号都**天然被排除**。短链批 `expected` 为空时
    /// 全靠 resolved_bizs；长链批仍保证 `expected`（下发目标）在列，即便没抓到凭证也据实处理。
    fn captured_bizs_since(&self, expected: &[String]) -> Vec<String> {
        let mut out = expected.to_vec();
        let mut seen: HashSet<String> = out.iter().cloned().collect();
        for biz in self.relay.resolved_bizs() {
            if seen.insert(biz.clone()) {
                out.push(biz);
            }
        }
        out
    }

    /// 任务链接按号分组（长链直接解析 `__biz`；短链靠接力时登记的 URL→biz）。保序。
    fn links_by_biz(&self, job: &Job) -> HashMap<String, Vec<String>> {
        let resolved: HashMap<String, String> = self.relay.resolved_pairs().into_iter().collect();
        let mut out: HashMap<String, Vec<String>> = HashMap::new();
        for u in &job.links {
            let biz = parse_s_url(u)
                .map(|a| a.biz)
                .or_else(|| resolved.get(u).cloned());
            if let Some(b) = biz {
                let v = out.entry(b).or_default();
                if !v.contains(u) {
                    v.push(u.clone());
                }
            }
        }
        out
    }

    /// 每个号生效的「最后更新时间」：该号各条任务链接的 since（链接级优先、任务级兜底）取
    /// **最早**者（多翻不漏）；都没有 → `None`（只采第 1 页）。待采的号（`captured`）若在
    /// 任务链接里解析不到（短链没登记到 URL→biz 对），按任务级 `last_updated_at` 兜底——
    /// 否则会静默退化成只采首页。
    fn since_by_biz(
        &self,
        job: &Job,
        links_by_biz: &HashMap<String, Vec<String>>,
        captured: &[String],
    ) -> HashMap<String, Option<f64>> {
        let mut out = HashMap::new();
        for (biz, links) in links_by_biz {
            let since = links
                .iter()
                .filter_map(|u| job.since_for_link(u))
                .fold(None, |acc: Option<f64>, s| {
                    Some(acc.map_or(s, |a| a.min(s)))
                });
            out.insert(biz.clone(), since);
        }
        for biz in captured {
            if !out.contains_key(biz) {
                if job.last_updated_at.is_some() {
                    self.warn(
                        Stage::List,
                        format!(
                            "{} 没有对应到任务链接，按任务级最后发布时间翻页",
                            self.store.account_label(biz)
                        ),
                    );
                }
                out.insert(biz.clone(), job.last_updated_at);
            }
        }
        out
    }

    /// 把本次结果按**公众号**分组（本地 `jobs.result_json` 的按号视图）：任务里每条链接按解析 / 接力
    /// 登记到的 `__biz` 归组，连号都对不上的（短链没打开过）单独一组；**任务里每个号都有一条**，没打开 /
    /// 没凭证 / 没采完的号 `urls` 为空、`finished=false`（这些号**没有**上报）；采完的号带上当场上报的结果
    /// （`reported` / `report_error`）。保持任务链接顺序。
    fn account_reports(
        &self,
        job: &Job,
        links_by_biz: &HashMap<String, Vec<String>>,
        summary: &CollectSummary,
    ) -> Vec<AccountReport> {
        let mut biz_of: HashMap<&str, &str> = HashMap::new();
        for (b, ls) in links_by_biz {
            for u in ls {
                biz_of.insert(u.as_str(), b.as_str());
            }
        }
        let mut out: Vec<AccountReport> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        for u in &job.links {
            let biz = biz_of.get(u.as_str()).map(|b| b.to_string());
            let key = match &biz {
                Some(b) => format!("biz:{b}"),
                None => format!("url:{u}"),
            };
            let i = *index.entry(key).or_insert_with(|| {
                out.push(AccountReport::default());
                out.len() - 1
            });
            let acc = &mut out[i];
            if !acc.links.contains(u) {
                acc.links.push(u.clone());
            }
            if acc.biz.is_none() {
                acc.biz = biz;
            }
        }
        for acc in &mut out {
            if let Some(b) = &acc.biz {
                acc.urls = summary.collected.get(b).cloned().unwrap_or_default();
                acc.finished = summary.finished.contains(b);
                if let Some(ack) = summary.acks.get(b) {
                    acc.reported = ack.ok;
                    acc.report_error = if ack.ok || ack.skipped {
                        None
                    } else {
                        Some(
                            ack.error
                                .clone()
                                .unwrap_or_else(|| "上报服务未返回成功状态".to_string()),
                        )
                    };
                }
            }
        }
        out
    }

    /// **一个号列表采完就当场上报**（不等整批）：组一条 [`ReportPayload`] 交给 `ReportFn`（HTTP 实现
    /// POST 到用户配置的地址；未启用上报返回 `skipped`）。`items` 为空（按时间比对后没有新文章）也报；
    /// 调用方保证只对**采完**的号调用——列表没正常获取到的号不上报。返回该号的 ack（合成任务级结果与落库用）。
    async fn report_account(
        &self,
        job: &Job,
        job_id: i64,
        biz: &str,
        items: &[CollectedArticle],
    ) -> ReportAck {
        let label = self.store.account_label(biz);
        let nickname = self
            .store
            .get_account(biz)
            .ok()
            .flatten()
            .and_then(|a| a.nickname)
            .filter(|n| !n.is_empty());
        let articles = items
            .iter()
            .map(|it| ReportArticle {
                url: it.url.clone(),
                title: it.title.clone(),
                published_at: it.published_at,
                is_new: it.is_new,
            })
            .collect::<Vec<_>>();
        let n = articles.len();
        let payload = ReportPayload::new(job_id, job.kind.as_str(), biz, nickname, articles);
        let ack = (self.report)(payload).await;
        if ack.skipped {
            // 未启用上报：不刷日志（每个号都会走到这里）。
        } else if ack.ok {
            self.emit(
                Stage::Report,
                format!(
                    "上报 {label}：{}",
                    if n == 0 {
                        "无新文章，报空数组".to_string()
                    } else {
                        format!("{n} 条文章链接")
                    }
                ),
            );
        } else {
            applog::error(
                Stage::Report,
                format!(
                    "上报 {label} 失败：{}",
                    ack.error.as_deref().unwrap_or("上报服务未返回成功状态")
                ),
            );
        }
        ack
    }

    /// 给续期挑一条该号**已抓到的**文章长链：优先本次采到的（随机取一条能解析出 `__biz` 的），
    /// 其次库里该号任意一条带 `content_url` 的文章，最后退到任务链接本身。
    fn sample_url_for(
        &self,
        biz: &str,
        collected: &HashMap<String, Vec<String>>,
        links_by_biz: &HashMap<String, Vec<String>>,
    ) -> Option<String> {
        let from_run: Vec<&String> = collected
            .get(biz)
            .into_iter()
            .flatten()
            .filter(|u| parse_s_url(u).is_some_and(|a| a.biz == biz))
            .collect();
        if let Some(u) = rng::pick(&from_run) {
            return Some((*u).clone());
        }
        let from_db: Vec<String> = self
            .store
            .list_articles(Some(biz), 50, 0, false)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|a| a.content_url)
            .filter(|u| parse_s_url(u).is_some())
            .collect();
        if let Some(u) = rng::pick(&from_db) {
            return Some(u.clone());
        }
        links_by_biz.get(biz).and_then(|v| rng::pick(v)).cloned()
    }

    /// 采集阶段：逐号采列表；**采集途中**过期的号先放一边，本轮其它号采完后**集中一次续期**，
    /// 再从各自中断的 offset 续采；最多 `refresh_rounds` 次续期。返回平铺链接 + 没采完的号。
    ///
    /// 首采前就没有新鲜凭证的号（接力阶段没抓到 / 被限流）**不走续期**——接力看门狗已尽力，
    /// 再点一次多半还是拿不到；直接记为没采完。`allow_refresh=false`（接力命中验证页中止）时
    /// 过期号也一律记为没采完。
    ///
    /// 续期是接力，需要抓凭证代理在跑：整批复用凭证的任务此前没起过代理，这里在**第一次
    /// 续期前**按需起（[`Self::ensure_capture`]）；起不来则过期号记为没采完。
    ///
    /// **每个号采完当场上报**（[`Self::report_account`]；未启用上报则跳过），
    /// 结果记在返回值 `acks`；没采完的号不上报。
    #[allow(clippy::too_many_arguments)]
    async fn collect_all(
        &self,
        job: &Job,
        job_id: i64,
        captured: &[String],
        since_by_biz: &HashMap<String, Option<f64>>,
        links_by_biz: &HashMap<String, Vec<String>>,
        allow_refresh: bool,
        cap: &mut CaptureState,
        source: crate::collector::ListSource,
    ) -> CollectSummary {
        let mut urls: Vec<String> = Vec::new();
        let mut collected: HashMap<String, Vec<String>> = HashMap::new();
        let mut unfinished: Vec<String> = Vec::new();
        let mut finished: Vec<String> = Vec::new();
        let mut new_articles = 0usize;
        let mut rate_limited = false;
        let mut acks: HashMap<String, ReportAck> = HashMap::new();
        let mut items: HashMap<String, Vec<CollectedArticle>> = HashMap::new();
        let mut pending: Vec<PendingAccount> = captured
            .iter()
            .map(|b| PendingAccount {
                biz: b.clone(),
                offset: 0,
            })
            .collect();
        let mut rounds_used = 0u32;
        let hand_back = if self.sweep_job_running() {
            "本轮稍后重试"
        } else {
            "记为未完成"
        };

        loop {
            let mut expired: Vec<PendingAccount> = Vec::new();
            let mut queue = pending.drain(..);
            while let Some(p) = queue.next() {
                let since = since_by_biz.get(&p.biz).copied().flatten();
                if !self
                    .store
                    .credential_is_fresh(&p.biz, self.cfg.cred_ttl_secs)
                    .unwrap_or(false)
                {
                    if rounds_used == 0 && p.offset == 0 {
                        // 首采前就没凭证：接力阶段没抓到（可能没接力到/被限流），记为未完成。
                        self.warn(
                            Stage::List,
                            format!(
                                "跳过 {}：没有新鲜凭证（接力没打开到该号 / 被限流），{hand_back}",
                                self.store.account_label(&p.biz)
                            ),
                        );
                        unfinished.push(p.biz.clone());
                    } else {
                        self.warn(
                            Stage::List,
                            format!(
                                "{} 凭证已不新鲜，待集中续期",
                                self.store.account_label(&p.biz)
                            ),
                        );
                        expired.push(p);
                    }
                    continue;
                }
                let plan = ListPlan {
                    since,
                    start_offset: p.offset,
                    count: self.cfg.list_page_count,
                    max_pages: self.cfg.list_max_pages,
                    page_sleep_min_ms: self.cfg.page_sleep_min_ms,
                    page_sleep_max_ms: self.cfg.page_sleep_max_ms,
                    rate_retry_wait_ms: self.cfg.rate_retry_wait_ms,
                    gap_min_ms: self.cfg.list_gap_min_ms,
                    gap_max_ms: self.cfg.list_gap_max_ms,
                    source,
                    paginate: false,
                };
                let req = ListRequest {
                    biz: p.biz.clone(),
                    plan,
                };
                // 每页的「开始获取 / 获取成功」由 collector 逐页记录；这里只记该号的汇总。
                let label = self.store.account_label(&p.biz);
                match (self.collect_list)(req).await {
                    Ok(o) => {
                        collected
                            .entry(p.biz.clone())
                            .or_default()
                            .extend(o.urls.iter().cloned());
                        urls.extend(o.urls.iter().cloned());
                        // 文章明细（上报用）：collector 逐篇给了就用；测试桩只给 urls 时按 URL 兜底。
                        let entry = items.entry(p.biz.clone()).or_default();
                        if o.items.len() == o.urls.len() {
                            entry.extend(o.items.iter().cloned());
                        } else {
                            entry.extend(o.urls.iter().map(|u| CollectedArticle {
                                url: u.clone(),
                                is_new: false,
                                ..Default::default()
                            }));
                        }
                        new_articles += o.new;
                        self.emit(
                            Stage::List,
                            format!(
                                "{label} {}：{}",
                                if since.is_some() {
                                    if p.offset > 0 {
                                        "续采完成"
                                    } else {
                                        "按最后发布时间翻页完成"
                                    }
                                } else {
                                    "首页采集完成"
                                },
                                o.feedback
                            ),
                        );
                        if o.cred_expired {
                            expired.push(PendingAccount {
                                biz: p.biz.clone(),
                                offset: o.next_offset.unwrap_or(p.offset),
                            });
                        } else {
                            if let Some(off) = o.next_offset {
                                // 翻页安全阀到顶：按完成处理但告警（避免 since 太早时无止境翻）。
                                self.warn(
                                    Stage::List,
                                    format!(
                                        "{label} 翻页达上限 {} 页（offset={off}），按完成处理",
                                        self.cfg.list_max_pages
                                    ),
                                );
                            }
                            finished.push(p.biz.clone());
                            // 该号列表已正常获取完：当场上报（含续采后的全部页；无新文章报空数组）。
                            let all_items = items.get(&p.biz).cloned().unwrap_or_default();
                            let ack = self.report_account(job, job_id, &p.biz, &all_items).await;
                            acks.insert(p.biz.clone(), ack);
                        }
                    }
                    Err(e) if is_credential_expired_err(&e) => {
                        self.warn(Stage::List, format!("{label} 凭证过期：{e}；待集中续期"));
                        expired.push(p);
                    }
                    Err(e) if is_rate_limited_err(&e) || is_account_blocked_err(&e) => {
                        // 限流是整机级的：本号与本批剩余号全部停采，触发整机退避；退避结束后
                        // 巡检从断点续跑（本批剩余号本轮稍后重试）。
                        // 账号级封禁（ret=-6/-12）走小时级阶梯：换 key 无用，续期也一并跳过。
                        let blocked = is_account_blocked_err(&e);
                        let secs = if blocked {
                            runstate::block_trigger(format!("{label}：{e}"))
                        } else {
                            runstate::cooldown_trigger(format!("{label}：{e}"))
                        };
                        rate_limited = true;
                        unfinished.push(p.biz.clone());
                        let rest: Vec<String> = queue.by_ref().map(|q| q.biz).collect();
                        unfinished.extend(rest.iter().cloned());
                        unfinished.extend(expired.drain(..).map(|q| q.biz));
                        applog::error(
                            Stage::List,
                            format!(
                                "⛔ {label} {}：{e}；整机退避 {}，本批剩余 {} 个号停采（{hand_back}）",
                                if blocked {
                                    "微信号被限制"
                                } else {
                                    "被限流"
                                },
                                if secs >= 3600 {
                                    format!("{} 小时", secs / 3600)
                                } else {
                                    format!("{} 分钟", secs / 60)
                                },
                                rest.len()
                            ),
                        );
                        break;
                    }
                    Err(e) => {
                        applog::error(Stage::List, format!("{label} 采集异常：{e}；{hand_back}"));
                        unfinished.push(p.biz.clone());
                    }
                }
            }
            drop(queue);
            if expired.is_empty() {
                break;
            }
            if !allow_refresh {
                self.warn(
                    Stage::Refresh,
                    format!("接力已中止，不再续期；{} 个号未采完", expired.len()),
                );
                unfinished.extend(expired.into_iter().map(|p| p.biz));
                break;
            }
            if rounds_used >= self.cfg.refresh_rounds {
                self.warn(
                    Stage::Refresh,
                    format!(
                        "集中续期已用满 {} 轮，{} 个号未采完",
                        self.cfg.refresh_rounds,
                        expired.len()
                    ),
                );
                unfinished.extend(expired.into_iter().map(|p| p.biz));
                break;
            }
            // 续期要经代理抓新 key：整批复用凭证时代理还没起，此时再起。
            phases::mark("集中续期");
            if self.ensure_capture(cap).await.is_err() {
                self.warn(
                    Stage::Refresh,
                    format!("抓凭证代理起不来，无法续期；{} 个号未采完", expired.len()),
                );
                unfinished.extend(expired.into_iter().map(|p| p.biz));
                break;
            }
            rounds_used += 1;

            // 集中续期：每个过期号随机取一条已抓链接作为接力目标，一次开浏览器整批续。
            // 续期器会打开内置浏览器（结束时自己关一次），收尾再兜底关一次。
            cap.browser_on = true;
            let mut targets = Vec::new();
            let mut no_sample = Vec::new();
            for p in &expired {
                match self.sample_url_for(&p.biz, &collected, links_by_biz) {
                    Some(u) => targets.push(RefreshTarget {
                        biz: p.biz.clone(),
                        sample_url: u,
                    }),
                    None => no_sample.push(p.biz.clone()),
                }
            }
            if !no_sample.is_empty() {
                self.warn(
                    Stage::Refresh,
                    format!("无可用取样链接、无法续期，记为未完成：{no_sample:?}"),
                );
                unfinished.extend(no_sample.iter().cloned());
            }
            self.emit(
                Stage::Refresh,
                format!(
                    "🔑 本轮 {} 个号凭证过期，集中续期（第 {rounds_used}/{} 轮）",
                    targets.len(),
                    self.cfg.refresh_rounds
                ),
            );
            let refresher = CredentialRefresher {
                store: self.store.clone(),
                relay: self.relay.clone(),
                rpa: self.rpa.clone(),
                sleep: self.sleep.clone(),
                cfg: RefreshConfig {
                    max_attempts: self.cfg.refresh_attempts.max(1),
                    wait_secs: self.manual_floor(self.cfg.refresh_wait_for(targets.len())),
                    launch_timeout_secs: self.cfg.launch_timeout_secs,
                    cred_ttl_secs: self.cfg.cred_ttl_secs,
                    seed_url: self.cfg.refresh_seed_url.clone(),
                },
            };
            let rep = refresher.refresh(&targets).await;
            unfinished.extend(rep.failed.iter().cloned());
            if let Some(reason) = &rep.aborted {
                applog::error(Stage::Refresh, format!("⛔ 续期中止：{reason}"));
                // 中止：本轮没续上的全部交回。
                unfinished.extend(
                    expired
                        .iter()
                        .filter(|p| !rep.refreshed.contains(&p.biz) && !rep.failed.contains(&p.biz))
                        .map(|p| p.biz.clone()),
                );
                break;
            }
            // 续上的号从中断 offset 续采。
            phases::mark("续采文章列表");
            pending = expired
                .into_iter()
                .filter(|p| rep.refreshed.contains(&p.biz))
                .collect();
            if pending.is_empty() {
                break;
            }
        }

        // 去重保序（同一号可能被记两次）。
        let mut seen = HashSet::new();
        unfinished.retain(|b| seen.insert(b.clone()));
        finished.retain(|b| !unfinished.contains(b));
        CollectSummary {
            urls,
            collected,
            unfinished,
            refresh_rounds_used: rounds_used,
            rate_limited,
            finished,
            new_articles,
            acks,
        }
    }

    /// 抓取历史文章用的运行参数（预算 / 节流项与主循环同源）。
    fn history_config(&self) -> crate::history::HistoryConfig {
        crate::history::HistoryConfig {
            page_count: self.cfg.history_page_count,
            gap_secs: self.cfg.history_gap_secs,
            budget_reserve: self.cfg.history_budget_reserve,
            budget: self.cfg.list_daily_budget,
            cred_ttl_secs: self.cfg.cred_ttl_secs,
            rate_retry_wait_ms: self.cfg.rate_retry_wait_ms,
            gap_min_ms: self.cfg.list_gap_min_ms,
            gap_max_ms: self.cfg.list_gap_max_ms,
        }
    }

    /// 单号接力换 key（历史抓取用）：起代理 → 用该号库里最新一篇长链做取样，交给
    /// [`CredentialRefresher`] 点种子接力等新 key → 关浏览器、停代理、清队列。不采列表。
    /// 返回是否拿到了新鲜凭证。
    pub async fn refresh_credential_for(&self, biz: &str) -> anyhow::Result<bool> {
        let sample = self
            .store
            .latest_article_urls(biz, 5)?
            .into_iter()
            .find(|u| crate::sweep::sample_matches(u, biz));
        let Some(url) = sample else {
            anyhow::bail!("库里没有该号可用的文章链接，无法打开文章换凭证");
        };
        let mut cap = CaptureState::default();
        self.ensure_capture(&mut cap).await?;
        // 续期器内部会 RPA 点种子拉起浏览器：收尾要关。
        cap.browser_on = true;
        let refresher = CredentialRefresher {
            store: self.store.clone(),
            relay: self.relay.clone(),
            rpa: self.rpa.clone(),
            sleep: self.sleep.clone(),
            cfg: RefreshConfig {
                max_attempts: self.cfg.refresh_attempts.max(1),
                wait_secs: self.manual_floor(self.cfg.refresh_wait_for(1)),
                launch_timeout_secs: self.cfg.launch_timeout_secs,
                cred_ttl_secs: self.cfg.cred_ttl_secs,
                seed_url: self.cfg.refresh_seed_url.clone(),
            },
        };
        let rep = refresher
            .refresh(&[RefreshTarget {
                biz: biz.to_string(),
                sample_url: url,
            }])
            .await;
        if cap.browser_on {
            let (ok, msg) = self.rpa.close_browser();
            if !ok {
                self.warn(Stage::Rpa, format!("关闭微信内置浏览器失败：{msg}"));
            }
        }
        if cap.proxy_on {
            (self.capture_stop)().await;
            self.emit(Stage::Proxy, "抓凭证代理已停止，系统代理已复位".to_string());
        }
        self.relay.clear();
        if let Some(reason) = rep.aborted {
            anyhow::bail!("续期中止：{reason}");
        }
        Ok(rep.refreshed.iter().any(|b| b == biz))
    }

    /// 历史抓取：抓一页（调用方已持执行单元锁）。接力换 key 走 [`Self::refresh_credential_for`]。
    pub async fn history_tick(&self, job: crate::model::HistoryJob) -> crate::model::HistoryJob {
        let hcfg = self.history_config();
        let relay =
            |biz: String| -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + Send + '_>> {
                Box::pin(async move { self.refresh_credential_for(&biz).await })
            };
        crate::history::tick(
            &self.store,
            &hcfg,
            job,
            crate::history::TickDeps {
                collect: &self.collect_list,
                relay: &relay,
                report: &self.report,
            },
        )
        .await
    }

    /// 当前正在处理的是否巡检任务（只影响日志措辞：「记为未完成」vs「本轮稍后重试」）。
    fn sweep_job_running(&self) -> bool {
        self.sweep.as_ref().is_some_and(|s| s.batch_in_progress())
    }

    /// 长驻主循环：历史抓取到点的下一页 / 巡检批次处理；都没有时按 `poll_secs` 休眠。`stop` 置真时退出。
    ///
    /// **严格串行（务必保持）**：每轮**取一个执行单元 → 完整跑完**（历史一页 `history::tick` /
    /// 巡检一批 `process_job`）后才回到 `while` 再取下一个。`stop` 只在两个单元之间检查，所以停止
    /// 请求会等当前单元收尾后才生效（不打断进行中的整链）。不要在此加并发预取，会破坏该保证。
    ///
    /// **两种模式、一个循环**（巡检与历史抓取互斥，由 GUI 命令层保证只会有一种）：
    /// - 巡检模式（`sweep` 为 `Some`，GUI「开始巡检」）：每轮先看整机退避 / 每号预算，再向巡检要下一批；
    ///   `stop`（「停止巡检」）在当前批结束后退出。
    /// - 历史模式（`sweep` 为 `None`，GUI 开始 / 继续历史抓取时拉起）：只推进历史任务；没有活动的
    ///   历史任务（完成 / 取消 / 失败）就自行退出，不空转。
    pub async fn run_forever(&self, stop: Arc<AtomicBool>) {
        applog::info(
            Stage::Task,
            format!(
                "🟢 主循环已启动：{}（空闲时每 {}s 检查一次）…",
                if self.sweep.is_some() {
                    "定时巡检批次"
                } else {
                    "历史文章抓取"
                },
                self.cfg.poll_secs.max(1)
            ),
        );
        if let Some(sw) = &self.sweep {
            sw.on_start(&self.store);
        }
        runstate::set_list_daily_budget(self.cfg.list_daily_budget);
        let mut cooldown_logged: Option<u64> = None;
        let mut budget_logged: Option<u64> = None;
        while !stop.load(Ordering::Relaxed) {
            // 历史模式：没有活动的历史任务就退出（巡检 / 历史互斥，循环不该空占着整链锁）。
            if self.sweep.is_none() {
                match self.store.history_active() {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        self.emit(Stage::List, "历史抓取没有活动任务，主循环退出".to_string());
                        break;
                    }
                    Err(e) => {
                        self.warn(Stage::List, format!("读取历史抓取任务失败：{e}"));
                        break;
                    }
                }
            }
            // 0) 整机限流退避：退避没结束不领任何任务（同一 IP / 同一微信号）。
            if let Some(remain) = runstate::cooldown_remaining_secs() {
                if cooldown_logged.is_none() {
                    let snap = runstate::cooldown_snapshot();
                    self.warn(
                        Stage::Sweep,
                        format!(
                            "⏸ 整机限流退避中（第 {} 级，{}），{} 分钟后恢复领任务；原因：{}",
                            snap.level,
                            applog::format_local(snap.until.unwrap_or_default()),
                            remain.div_ceil(60),
                            snap.reason
                        ),
                    );
                }
                if cooldown_logged != Some(remain / 30) {
                    applog::progress(
                        Stage::Sweep,
                        format!("限流退避中… 剩余 {} 分 {} 秒", remain / 60, remain % 60),
                    );
                    cooldown_logged = Some(remain / 30);
                }
                if let Some(sw) = &self.sweep {
                    sw.sync_status();
                }
                (self.sleep)(Duration::from_secs(remain.clamp(1, 30))).await;
                continue;
            }
            if cooldown_logged.is_some() {
                cooldown_logged = None;
                self.emit(Stage::Sweep, "▶ 限流退避结束，恢复领任务".to_string());
            }
            // 0.5) 每号预算：当前微信号近 24 小时 `getmsg` 已达 `list_daily_budget` → 历史页与
            // 巡检都不领，等窗口内最早一次请求滑出 24 小时。实测微信号在累计约 200 次处被 `ret=-6`，
            // 预算是唯一能防住它的闸（8–20s 间隔防不了「打得太多」）。
            if let Some(h) = runstate::list_budget_check(&self.store, self.cfg.list_daily_budget) {
                let remain = (h.resume_at - crate::model::now()).max(1.0) as u64;
                if budget_logged.is_none() {
                    let msg = format!(
                        "⏸ 当前微信号近 24 小时列表请求已达预算（{}/{}），暂停领取任务，{} 恢复（{} 分钟后）；换微信号登录后新号从 0 计",
                        h.used,
                        h.budget,
                        applog::format_local(h.resume_at),
                        remain.div_ceil(60)
                    );
                    self.warn(Stage::Task, msg.clone());
                    crate::notify::notify(crate::notify::Kind::Sweep, msg);
                }
                if budget_logged != Some(remain / 30) {
                    applog::progress(
                        Stage::Task,
                        format!(
                            "列表请求预算已用完（{}/{}）… {} 分 {} 秒后腾出额度",
                            h.used,
                            h.budget,
                            remain / 60,
                            remain % 60
                        ),
                    );
                    budget_logged = Some(remain / 30);
                }
                if let Some(sw) = &self.sweep {
                    sw.sync_status();
                }
                (self.sleep)(Duration::from_secs(remain.clamp(1, 30))).await;
                continue;
            }
            if budget_logged.is_some() {
                budget_logged = None;
                self.emit(
                    Stage::Task,
                    "▶ 列表请求预算已腾出额度，恢复领任务".to_string(),
                );
            }
            // 执行单元守卫：历史一页 / 巡检一批处理期间持有，单元之间空闲释放——单号「重新巡检」
            // 只需抢到它就能在空闲期立即执行；被它占着时本轮跳过，稍后再来。
            let Some(_unit) = runstate::try_acquire_unit() else {
                (self.sleep)(Duration::from_secs(self.cfg.poll_secs.clamp(1, 5))).await;
                continue;
            };
            // 1) 历史抓取：下一页到点就抓一页（一页一个单元）。
            match crate::history::due(&self.store) {
                Ok(Some(job)) => {
                    phases::prelude("历史抓取下一页", 0);
                    self.history_tick(job).await;
                    continue;
                }
                Ok(None) => {}
                Err(e) => self.warn(Stage::List, format!("读取历史抓取任务失败：{e}")),
            }
            // 2) 本地巡检：向它要下一批（到点 / 不在暂停时才有）。
            if let Some(sw) = &self.sweep {
                let t0 = Instant::now();
                if let Some(batch) = sw.next_batch(&self.store) {
                    phases::prelude("合成巡检批次", t0.elapsed().as_millis() as i64);
                    let report = self.process_job(batch.job.clone()).await;
                    sw.finish_batch(&self.store, &batch, &report);
                    continue;
                }
            }
            drop(_unit);
            // 3) 都没有：休眠 poll_secs 再来（历史抓取下一页更早到点就只睡到那时）。
            let mut secs = self.cfg.poll_secs.max(1);
            if let Some(at) = crate::history::next_due_at(&self.store) {
                let wait = (at - crate::model::now()).ceil().max(1.0) as u64;
                secs = secs.min(wait);
            }
            (self.sleep)(Duration::from_secs(secs)).await;
        }
        if let Some(sw) = &self.sweep {
            sw.on_stop();
        }
    }
}

#[cfg(test)]
mod tests {
    /// 任务列表的结果分类：回报失败 > 中止（按 timed_out 分超时 / 错误）> 限流 > 未跑完 > 正常。
    #[test]
    fn test_report_classify() {
        use super::*;
        let ok = Report::default();
        assert_eq!(ok.classify(), (JOB_OUTCOME_OK, None));

        // 回报非 2xx：服务端错误，即便采集本身正常
        let srv = Report {
            report_ack: Some(ReportAck::from_status(502)),
            ..Default::default()
        };
        let (k, e) = srv.classify();
        assert_eq!(k, JOB_OUTCOME_SERVER_ERROR);
        assert!(e.unwrap().contains("HTTP 502"));
        // 网络错误同样是服务端错误
        let net = Report {
            report_ack: Some(ReportAck::failed("connection refused")),
            ..Default::default()
        };
        assert_eq!(net.classify().0, JOB_OUTCOME_SERVER_ERROR);
        // 回报 2xx 不影响分类
        let fine = Report {
            report_ack: Some(ReportAck::from_status(200)),
            ..Default::default()
        };
        assert_eq!(fine.classify(), (JOB_OUTCOME_OK, None));

        // 看门狗重拉耗尽（timed_out）→ 超时，原因照抄
        let relaunch = Report {
            truncated: true,
            abort_reason: Some("点空；重拉 3 次仍无进展".into()),
            timed_out: true,
            ..Default::default()
        };
        assert_eq!(
            relaunch.classify(),
            (JOB_OUTCOME_TIMEOUT, Some("点空；重拉 3 次仍无进展".into()))
        );
        // 验证页中止（非超时）→ 错误
        let verify = Report {
            truncated: true,
            abort_reason: Some("命中人机验证页".into()),
            ..Default::default()
        };
        assert_eq!(verify.classify().0, JOB_OUTCOME_ERROR);
        // 等到上限仍有未打开 → 超时，原因带条数
        let wait = Report {
            truncated: true,
            timed_out: true,
            unopened_links: vec!["a".into(), "b".into()],
            remaining_links: vec!["a".into(), "b".into()],
            ..Default::default()
        };
        let (k, e) = wait.classify();
        assert_eq!(k, JOB_OUTCOME_TIMEOUT);
        assert!(e.unwrap().contains("2 条链接未打开"));
        // 限流退避 → 错误
        let rl = Report {
            truncated: true,
            rate_limited: true,
            ..Default::default()
        };
        assert_eq!(rl.classify().0, JOB_OUTCOME_ERROR);
        // 续期失败等未跑完 → 错误
        let trunc = Report {
            truncated: true,
            remaining_links: vec!["x".into()],
            ..Default::default()
        };
        assert_eq!(trunc.classify().0, JOB_OUTCOME_ERROR);
    }

    use super::*;
    use std::collections::HashMap as Map;
    use std::net::SocketAddr;
    use std::sync::Mutex;

    use serde_json::Value;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use crate::capture;
    use crate::model::{ArticleFields, Job};
    use crate::report::{HttpReporter, ReportConfig};

    #[test]
    fn test_bizs_from_links() {
        let links = vec![
            "https://mp.weixin.qq.com/s?__biz=AAA==&mid=1&idx=1&sn=x".to_string(),
            "https://mp.weixin.qq.com/s?__biz=BBB==&mid=2&idx=1".to_string(),
            "https://mp.weixin.qq.com/s/shortnoBiz".to_string(),
            "https://mp.weixin.qq.com/s?__biz=AAA==&mid=9&idx=1".to_string(),
        ];
        assert_eq!(
            bizs_from_links(&links),
            vec!["AAA==".to_string(), "BBB==".to_string()]
        );
    }

    /// 模拟 getmsg 采集：给该号写一条示例文章，返回其链接（记录被调用的请求）。
    fn sim_collect(store: Arc<Store>, calls: Arc<Mutex<Vec<ListRequest>>>) -> CollectFn {
        Arc::new(move |req: ListRequest| {
            let store = store.clone();
            let calls = calls.clone();
            Box::pin(async move {
                calls.lock().unwrap().push(req.clone());
                let biz = req.biz.clone();
                let url = format!("https://mp.weixin.qq.com/s?__biz={biz}&mid=999&idx=1&sn=s");
                let fields = ArticleFields {
                    title: Some(format!("{biz} 的第一页示例文章")),
                    content_url: Some(url.clone()),
                    published_at: Some(1_787_000_000.0),
                    ..Default::default()
                };
                let _ = store.upsert_article(&biz, "999", 1, &fields);
                Ok(CollectOutcome {
                    total: 1,
                    new: 1,
                    pages: 1,
                    feedback: "采到1篇(模拟)".into(),
                    urls: vec![url],
                    ..Default::default()
                })
            })
        })
    }

    // -- 模拟 RPA：喂真实 capture 逻辑一个"假 /s flow" --
    // open_seed 时读共享接力队列的每条链接，模拟 MITM：抓凭证(首见该号注入带 key 的请求) +
    // 抓文章骨架 + 接力推进队列。全部走**真实** capture::capture_request 与 RelayQueue.advance。
    struct SimRpa {
        store: Arc<Store>,
        relay: RelayQueue,
    }
    impl WeChatController for SimRpa {
        fn open_seed(&self, _seed: &str) -> (bool, String) {
            let rows = self.relay.rows();
            let mut seen_biz: HashSet<String> = HashSet::new();
            for row in &rows {
                if let Some(art) = parse_s_url(&row.url) {
                    if seen_biz.insert(art.biz.clone()) {
                        // 模拟抓到该号带 session 的请求（key/uin/pass_ticket）
                        let cred_url = format!(
                            "https://mp.weixin.qq.com/mp/getappmsgext?__biz={}&uin=U&key=K&pass_ticket=P",
                            art.biz
                        );
                        capture::capture_request(&self.store, &cred_url, None, None);
                    }
                }
                // 抓 /s 文章骨架 + 接力推进（登记该链接解析到的 biz，供回报按队列各项聚合）
                capture::capture_request(&self.store, &row.url, None, None);
                let biz = parse_s_url(&row.url).map(|a| a.biz);
                self.relay
                    .advance_with_biz(Some(&row.url), None, biz.as_deref());
            }
            (true, "模拟：已接力打开本批并抓到各号凭证".to_string())
        }
        fn close_browser(&self) -> (bool, String) {
            (true, "模拟关闭".to_string())
        }
    }

    // -- 本地 mock 上报接收端（只收 POST，记录请求）--
    #[derive(Clone, Debug)]
    struct Recorded {
        method: String,
        path: String,
        body: String,
    }

    async fn read_request(stream: &mut tokio::net::TcpStream) -> Option<Recorded> {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        let header_end = loop {
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
            let n = stream.read(&mut tmp).await.ok()?;
            if n == 0 {
                return None;
            }
            buf.extend_from_slice(&tmp[..n]);
        };
        let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let mut lines = head.lines();
        let req_line = lines.next().unwrap_or("");
        let mut parts = req_line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let path = parts.next().unwrap_or("").to_string();
        let mut headers: Map<String, String> = Map::new();
        for l in lines {
            if let Some((k, v)) = l.split_once(':') {
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
            }
        }
        let cl: usize = headers
            .get("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        while buf.len() < header_end + cl {
            let n = stream.read(&mut tmp).await.ok()?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        let body =
            String::from_utf8_lossy(&buf[header_end..(header_end + cl).min(buf.len())]).to_string();
        Some(Recorded { method, path, body })
    }

    async fn spawn_mock_receiver() -> (SocketAddr, Arc<Mutex<Vec<Recorded>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let recorded = Arc::new(Mutex::new(Vec::<Recorded>::new()));
        let rec = recorded.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let rec = rec.clone();
                tokio::spawn(async move {
                    if let Some(req) = read_request(&mut stream).await {
                        rec.lock().unwrap().push(req);
                        let resp =
                            "HTTP/1.1 204 No Content\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
                        let _ = stream.write_all(resp.as_bytes()).await;
                        let _ = stream.flush().await;
                    }
                });
            }
        });
        (addr, recorded)
    }

    /// ⭐ headless e2e（hermetic 无微信）：
    /// 一批两条链接的巡检任务 → `process_job` → 模拟 RPA 驱动真实抓凭证+接力+入库 →
    /// 模拟 getmsg 采第一页 → 每号当场 POST 一条通用载荷到本地 mock 接收端。
    #[tokio::test]
    async fn test_headless_e2e_full_chain() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let relay = RelayQueue::new();

        // 两个不同公众号的长链接（无最后发布时间 → 各采第 1 页），先落库一行任务再交给 process_job
        let job = Job {
            links: vec![
                "https://mp.weixin.qq.com/s?__biz=AAA==&mid=100&idx=1&sn=s1".to_string(),
                "https://mp.weixin.qq.com/s?__biz=BBB==&mid=200&idx=1&sn=s2".to_string(),
            ],
            ..Default::default()
        };
        let job_id = store.create_job_for(&job).unwrap();
        let job = Job {
            local_id: Some(job_id),
            ..job
        };
        let (addr, recorded) = spawn_mock_receiver().await;

        // 真实 HTTP 上报客户端
        let reporter = HttpReporter::new(ReportConfig {
            enabled: true,
            url: format!("http://{addr}/hook"),
            token: "t0k".into(),
            timeout_secs: 5,
        })
        .unwrap();

        let calls = Arc::new(Mutex::new(Vec::new()));
        let orch = Orchestrator {
            store: store.clone(),
            relay: relay.clone(),
            rpa: Arc::new(SimRpa {
                store: store.clone(),
                relay: relay.clone(),
            }),
            report: Arc::new(move |payload| {
                let r = reporter.clone();
                Box::pin(async move { r.report(&payload).await })
            }),
            // MITM 由 SimRpa 模拟，capture_start/stop 空转
            capture_start: Arc::new(|| Box::pin(async { Ok(()) })),
            capture_stop: Arc::new(|| Box::pin(async {})),
            collect_list: sim_collect(store.clone(), calls.clone()),
            sleep: Arc::new(|_d| Box::pin(async {})),
            cfg: OrchestratorConfig {
                relay_enabled: true,
                capture_wait_secs: 5,
                cred_ttl_secs: 1800,
                poll_secs: 1,
                ..Default::default()
            },
            sweep: None,
        };

        // 跑这条任务（复用刚落库那一行）
        let rep = orch.process_job(job).await;
        assert_eq!(rep.job_id, job_id);

        // 断言：两个号都抓到了新鲜凭证
        assert!(store.credential_is_fresh("AAA==", 1800).unwrap());
        assert!(store.credential_is_fresh("BBB==", 1800).unwrap());
        // 接力队列已清空
        assert_eq!(relay.pending_count(), 0);
        // 两个号都入库了文章（/s 骨架 + 模拟 getmsg）
        assert!(!store
            .list_articles(Some("AAA=="), 50, 0, false)
            .unwrap()
            .is_empty());
        assert!(!store
            .list_articles(Some("BBB=="), 50, 0, false)
            .unwrap()
            .is_empty());
        // 无最后更新时间 → 计划 since=None（只采第 1 页）
        let reqs = calls.lock().unwrap().clone();
        assert_eq!(reqs.len(), 2);
        assert!(reqs
            .iter()
            .all(|r| r.plan.since.is_none() && r.plan.start_offset == 0));
        // 同一行落库为 reported（没有新建第二行）
        let jobs = store.recent_jobs(5).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, job_id);
        assert_eq!(jobs[0].status, "done");

        // 逐号上报被 mock 收到：两个号 → 两次 POST，通用载荷
        let posts: Vec<Recorded> = recorded.lock().unwrap().clone();
        assert_eq!(posts.len(), 2, "接收端应收到两个号各一条上报");
        let mut bodies: Vec<Value> = posts
            .iter()
            .map(|r| {
                assert_eq!(r.method, "POST");
                assert_eq!(r.path, "/hook");
                serde_json::from_str(&r.body).unwrap()
            })
            .collect();
        bodies.sort_by_key(|b| b["account"]["biz"].as_str().unwrap().to_string());
        for (b, biz) in bodies.iter().zip(["AAA==", "BBB=="]) {
            assert_eq!(b["source"], "mpider");
            assert_eq!(b["job_id"], job_id);
            assert_eq!(b["job_kind"], "sweep");
            assert_eq!(b["account"]["biz"], biz);
            let arts = b["articles"].as_array().unwrap();
            assert_eq!(arts.len(), 1);
            assert!(arts[0]["url"].as_str().unwrap().contains(biz));
            // 没有 task_id / mp_id / truncated / remaining_links
            assert!(b.get("task_id").is_none() && b.get("mp_id").is_none());
            assert!(b.get("truncated").is_none());
        }
        // 本地 jobs.result_json 仍是完整结果（平铺 urls + 按号 accounts）
        let local = store.get_job(jobs[0].id).unwrap().unwrap().result.unwrap();
        assert_eq!(local["truncated"], false);
        assert_eq!(local["remaining_links"].as_array().unwrap().len(), 0);
        assert_eq!(local["urls"].as_array().unwrap().len(), 2);
        let accounts = local["accounts"].as_array().unwrap();
        assert_eq!(accounts.len(), 2);
        assert!(accounts[0].get("mp_id").is_none());
        assert_eq!(accounts[0]["biz"], "AAA==");
        assert_eq!(accounts[0]["finished"], true);
        assert_eq!(accounts[0]["reported"], true);
    }

    // -- 短链批：expected 为空 + 凭证异步到达 —— 验证 wait_captures 会等到再收集 --
    // 模拟真机时序：短链 /s/<hash> 一打开就 drain 接力（/s 响应到达），但带 key 的
    // getappmsgext 是文章页 JS 稍后才发的——旧逻辑一 drain 就返回、漏采该号。
    struct SimRpaShort {
        store: Arc<Store>,
        relay: RelayQueue,
    }
    impl WeChatController for SimRpaShort {
        fn open_seed(&self, _seed: &str) -> (bool, String) {
            // /s 响应到达即 drain 接力（此刻还没抓到凭证）；短链解析到 SHORTBIZ== 一并登记。
            for row in self.relay.rows() {
                self.relay
                    .advance_with_biz(Some(&row.url), None, Some("SHORTBIZ=="));
            }
            // getappmsgext 稍后才发：延时任务模拟凭证异步到达。
            let store = self.store.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(80)).await;
                let cred = "https://mp.weixin.qq.com/mp/getappmsgext?__biz=SHORTBIZ==&uin=U&key=K&pass_ticket=P";
                capture::capture_request(&store, cred, None, None);
            });
            (true, "模拟：短链已打开，凭证稍后到达".to_string())
        }
        fn close_browser(&self) -> (bool, String) {
            (true, "模拟关闭".to_string())
        }
    }

    fn short_orch(
        store: Arc<Store>,
        relay: RelayQueue,
        calls: Arc<Mutex<Vec<ListRequest>>>,
    ) -> Orchestrator {
        Orchestrator {
            store: store.clone(),
            relay: relay.clone(),
            rpa: Arc::new(SimRpaShort {
                store: store.clone(),
                relay: relay.clone(),
            }),
            report: Arc::new(|_| Box::pin(async { ReportAck::accepted() })),
            capture_start: Arc::new(|| Box::pin(async { Ok(()) })),
            capture_stop: Arc::new(|| Box::pin(async {})),
            collect_list: sim_collect(store, calls),
            // 真实 sleep：让延时的凭证捕获任务有机会跑到。
            sleep: default_sleep(),
            cfg: OrchestratorConfig {
                relay_enabled: true,
                capture_wait_secs: 5,
                cred_ttl_secs: 1800,
                poll_secs: 1,
                ..Default::default()
            },
            sweep: None,
        }
    }

    /// 人工模式（NoOp / mac）：等待上限抬到 `rpa::MANUAL_WAIT_SECS`；能自动点的控制器原样。
    #[test]
    fn manual_floor_only_applies_to_manual_controllers() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let relay = RelayQueue::new();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let auto = short_orch(store.clone(), relay.clone(), calls.clone());
        assert_eq!(auto.manual_floor(5), 5);
        assert_eq!(auto.cfg.capture_wait_for(1), 8);
        let manual = Orchestrator {
            rpa: Arc::new(crate::rpa::NoOpController),
            ..short_orch(store, relay, calls)
        };
        assert_eq!(manual.manual_floor(5), rpa::MANUAL_WAIT_SECS);
        assert_eq!(manual.manual_floor(3600), 3600);

        // 待命页在线：人工模式也不抬等待（机器自己接力），并由待命页接管本批；自动点击模式不受影响。
        let _g = seedserver::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        seedserver::set_resident_mode(true);
        seedserver::note_resident_poll();
        assert_eq!(manual.manual_floor(5), 5);
        assert!(manual.resident_takes_over());
        assert!(!auto.resident_takes_over());
        seedserver::set_resident_mode(false);
        seedserver::reset_resident_poll();
        assert_eq!(manual.manual_floor(5), rpa::MANUAL_WAIT_SECS);
        assert!(!manual.resident_takes_over());
    }

    #[tokio::test]
    async fn test_shortlink_waits_for_async_credential() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let relay = RelayQueue::new();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let orch = short_orch(store.clone(), relay, calls.clone());

        // 短链任务（/s/<hash>，无 __biz → expected 为空）；任务级最后更新时间应透传到该号
        let job = Job {
            links: vec!["https://mp.weixin.qq.com/s/XBsCJgA6ZlMtLA1H6rPHlA".to_string()],
            last_updated_at: Some(1_700_000_000.0),
            ..Default::default()
        };
        let report = orch.process_job(job).await;

        // 关键：等到了异步凭证 → 该号被采集（旧逻辑一 drain 就返回、漏采）。
        assert!(store.credential_is_fresh("SHORTBIZ==", 1800).unwrap());
        let reqs = calls.lock().unwrap().clone();
        assert_eq!(reqs.len(), 1, "短链号应被采集");
        assert_eq!(reqs[0].biz, "SHORTBIZ==");
        // 短链靠接力登记的 URL→biz 把任务级 since 映射到号上
        assert_eq!(reqs[0].plan.since, Some(1_700_000_000.0));
        assert_eq!(report.urls.len(), 1);
        assert!(!report.truncated);
    }

    // -- 待采的号在任务链接里对应不到时，按任务级最后发布时间兜底，不得退化成只采首页 --
    #[tokio::test]
    async fn test_since_by_biz_falls_back_to_task_level_for_unmapped_biz() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let orch = short_orch(store, RelayQueue::new(), Arc::new(Mutex::new(Vec::new())));
        let job = Job {
            links: vec!["https://mp.weixin.qq.com/s?__biz=AAA==&mid=1&idx=1&sn=x".to_string()],
            last_updated_at: Some(1_700_000_000.0),
            ..Default::default()
        };
        let links_by_biz = orch.links_by_biz(&job);
        assert_eq!(links_by_biz.len(), 1);
        // captured 里多了一个任务链接映射不到的号 OTHER==
        let captured = vec!["AAA==".to_string(), "OTHER==".to_string()];
        let since = orch.since_by_biz(&job, &links_by_biz, &captured);
        assert_eq!(since.get("AAA=="), Some(&Some(1_700_000_000.0)));
        assert_eq!(
            since.get("OTHER=="),
            Some(&Some(1_700_000_000.0)),
            "映射不到的号应按任务级 since 兜底"
        );
        // 任务级也没有 → None（只采首页）
        let job2 = Job {
            links: job.links.clone(),
            ..Default::default()
        };
        let since2 = orch.since_by_biz(&job2, &links_by_biz, &captured);
        assert_eq!(since2.get("OTHER=="), Some(&None));
    }

    // -- 短链批：此前遗留仍新鲜的无关号**不得**被带进本批采集/回报 --
    #[tokio::test]
    async fn test_shortlink_report_excludes_stale_fresh_accounts() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let relay = RelayQueue::new();

        // 预置一个「此前遗留、仍新鲜」的无关号（captured_at = 现在，早于本批 window_start）。
        let leftover = crate::model::CredentialFields {
            key: Some("LEFTOVERKEY".into()),
            uin: Some("U".into()),
            ..Default::default()
        };
        store
            .upsert_account("LEFTOVER==", Some("遗留无关号"), None)
            .unwrap();
        store.upsert_credential("LEFTOVER==", &leftover).unwrap();

        let calls = Arc::new(Mutex::new(Vec::new()));
        let orch = short_orch(store.clone(), relay, calls.clone());

        // 保证本批 window_start 严格晚于遗留号的 captured_at。
        tokio::time::sleep(Duration::from_millis(20)).await;

        let job = Job {
            links: vec!["https://mp.weixin.qq.com/s/AS9Hu9lDqIZWDuan2lXzNw".to_string()],
            ..Default::default()
        };
        let report = orch.process_job(job).await;

        // 遗留号仍新鲜——但**不因新鲜**被带进本批；本批只采当场抓到的 SHORTBIZ==。
        assert!(
            store.credential_is_fresh("LEFTOVER==", 1800).unwrap(),
            "遗留号本应仍新鲜"
        );
        let reqs = calls.lock().unwrap().clone();
        assert_eq!(reqs.len(), 1, "只应采集本批当场抓到的 1 个号");
        assert_eq!(reqs[0].biz, "SHORTBIZ==");
        assert!(
            !report.urls.iter().any(|u| u.contains("LEFTOVER==")),
            "遗留无关号不得进入本批回报"
        );
    }

    /// 历史模式（sweep=None）且没有活动的历史任务：主循环立即退出，不起代理、不休眠空转。
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn test_run_forever_exits_when_no_history_job() {
        let _g = crate::runstate::COOLDOWN_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let store = Arc::new(Store::open_in_memory().unwrap());
        let slept = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicBool::new(false));
        let orch = Orchestrator {
            store: store.clone(),
            relay: RelayQueue::new(),
            rpa: Arc::new(crate::rpa::NoOpController),
            report: Arc::new(|_| Box::pin(async { ReportAck::accepted() })),
            capture_start: {
                let started = started.clone();
                Arc::new(move || {
                    started.store(true, Ordering::SeqCst);
                    Box::pin(async { Ok(()) })
                })
            },
            capture_stop: Arc::new(|| Box::pin(async {})),
            collect_list: Arc::new(|_| Box::pin(async { Ok(CollectOutcome::default()) })),
            sleep: {
                let slept = slept.clone();
                Arc::new(move |_d| {
                    slept.store(true, Ordering::SeqCst);
                    Box::pin(async {})
                })
            },
            cfg: OrchestratorConfig {
                poll_secs: 1,
                ..Default::default()
            },
            sweep: None,
        };
        orch.run_forever(Arc::new(AtomicBool::new(false))).await;
        assert!(!started.load(Ordering::SeqCst));
        assert!(!slept.load(Ordering::SeqCst));
    }

    /// 用 `processing` 标志圈出 process_job 的执行窗口；巡检每批一个号、两个号排队，第二批只有在
    /// 第一批收尾（processing 复位）后才会开始；一轮跑完主循环休眠时置 stop 收尾。
    /// 两个号库里都没有文章，取样走「批量添加」记下的种子链接。
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn test_run_forever_serial_sweep_batches() {
        use std::sync::atomic::AtomicUsize;

        // 主循环会读全局退避状态：与会触发退避的测试串行（同步锁跨 await 只在单测里，可接受）。
        let _g = crate::runstate::COOLDOWN_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let store = Arc::new(Store::open_in_memory().unwrap());
        let relay = RelayQueue::new();
        let stop = Arc::new(AtomicBool::new(false));
        let processing = Arc::new(AtomicBool::new(false));
        let starts = Arc::new(AtomicUsize::new(0));
        for (biz, name) in [("AAA==", "甲"), ("BBB==", "乙")] {
            store.upsert_account(biz, Some(name), None).unwrap();
            store
                .set_seed_url(
                    biz,
                    &format!("https://mp.weixin.qq.com/s?__biz={biz}&mid=1&idx=1&sn=seed"),
                )
                .unwrap();
        }

        // capture_start/stop 圈出「处理中」窗口；再次进入时上一批必须已收尾。
        let capture_start: CaptureStartFn = {
            let processing = processing.clone();
            let starts = starts.clone();
            Arc::new(move || {
                let (processing, starts) = (processing.clone(), starts.clone());
                Box::pin(async move {
                    assert!(
                        !processing.swap(true, Ordering::SeqCst),
                        "批次处理中不应再领下一批（应串行）"
                    );
                    starts.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            })
        };
        let capture_stop: CaptureStopFn = {
            let processing = processing.clone();
            Arc::new(move || {
                let processing = processing.clone();
                Box::pin(async move {
                    processing.store(false, Ordering::SeqCst);
                })
            })
        };
        let calls = Arc::new(Mutex::new(Vec::<ListRequest>::new()));
        let sweep = crate::sweep::Sweep::new(crate::sweep::SweepConfig {
            batch_size: 1,
            batch_gap_secs: 0,
            fail_pause_secs: 0,
            daily_budget: 0,
            ..Default::default()
        });
        let orch = Orchestrator {
            store: store.clone(),
            relay: relay.clone(),
            rpa: Arc::new(SimRpa {
                store: store.clone(),
                relay: relay.clone(),
            }),
            report: Arc::new(|_| Box::pin(async { ReportAck::accepted() })),
            capture_start,
            capture_stop,
            collect_list: sim_collect(store.clone(), calls.clone()),
            // 一轮跑完（轮间停留）才会休眠：此时置 stop 退出。
            sleep: {
                let stop = stop.clone();
                Arc::new(move |_d| {
                    stop.store(true, Ordering::SeqCst);
                    Box::pin(async {})
                })
            },
            cfg: OrchestratorConfig {
                relay_enabled: true,
                capture_wait_secs: 5,
                cred_ttl_secs: 1800,
                poll_secs: 1,
                ..Default::default()
            },
            sweep: Some(sweep),
        };

        orch.run_forever(stop.clone()).await;
        // 两批都跑过（各起一次代理），收尾后处理窗口已关闭；两个号都巡检成功、各采到 1 篇。
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        assert!(!processing.load(Ordering::SeqCst));
        assert_eq!(store.count_jobs(None).unwrap(), 2);
        assert_eq!(calls.lock().unwrap().len(), 2);
        for biz in ["AAA==", "BBB=="] {
            let a = store.get_account(biz).unwrap().unwrap();
            assert_eq!(a.sweep_status.as_deref(), Some("ok"), "{a:?}");
        }
    }

    // -- 看门狗：点空重拉 / 停滞重排 / 验证页中止 --

    /// 可编排行为的模拟 RPA：每次 open_seed 按脚本执行一步（闭包序列），并计数。
    struct ScriptedRpa {
        calls: Arc<std::sync::atomic::AtomicUsize>,
        steps: Vec<Box<dyn Fn() + Send + Sync>>,
    }
    impl WeChatController for ScriptedRpa {
        fn open_seed(&self, _seed: &str) -> (bool, String) {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(step) = self.steps.get(n) {
                step();
            }
            (true, format!("模拟点击 #{}", n + 1))
        }
        fn close_browser(&self) -> (bool, String) {
            (true, "模拟关闭".to_string())
        }
    }

    /// 模拟"接力全部跑完"：逐条 advance + 写凭证（登记各链接解析到的 biz）。
    fn drain_all(store: &Arc<Store>, relay: &RelayQueue) {
        drain_all_with_key(store, relay, "K");
    }

    /// 同上，可指定 key（续期测试里用新 key 区分）。
    fn drain_all_with_key(store: &Arc<Store>, relay: &RelayQueue, key: &str) {
        relay.touch();
        for row in relay.rows() {
            let biz = parse_s_url(&row.url).map(|a| a.biz);
            if let Some(b) = &biz {
                let cred = format!(
                    "https://mp.weixin.qq.com/mp/getappmsgext?__biz={b}&uin=U&key={key}&pass_ticket=P"
                );
                capture::capture_request(store, &cred, None, None);
            }
            relay.advance_with_biz(Some(&row.url), None, biz.as_deref());
        }
    }

    fn orch_with(
        store: Arc<Store>,
        relay: RelayQueue,
        rpa: ScriptedRpa,
        cfg: OrchestratorConfig,
    ) -> Orchestrator {
        Orchestrator {
            store,
            relay,
            rpa: Arc::new(rpa),
            report: Arc::new(|_| Box::pin(async { ReportAck::accepted() })),
            capture_start: Arc::new(|| Box::pin(async { Ok(()) })),
            capture_stop: Arc::new(|| Box::pin(async {})),
            collect_list: Arc::new(|_| Box::pin(async { Ok(CollectOutcome::default()) })),
            sleep: Arc::new(|_d| Box::pin(async {})),
            cfg,
            sweep: None,
        }
    }

    fn job(links: &[&str]) -> Job {
        Job {
            links: links.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    const L0: &str = "https://mp.weixin.qq.com/s?__biz=AAA==&mid=1&idx=1&sn=a";
    const L1: &str = "https://mp.weixin.qq.com/s?__biz=AAA==&mid=2&idx=1&sn=b";
    const LB: &str = "https://mp.weixin.qq.com/s?__biz=BBB==&mid=3&idx=1&sn=c";

    #[tokio::test]
    async fn test_watchdog_relaunches_when_seed_click_missed() {
        // 第一次点空（无任何请求）→ launch_timeout=0 立刻重点 → 第二次跑完。
        let store = Arc::new(Store::open_in_memory().unwrap());
        let relay = RelayQueue::new();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (s2, r2) = (store.clone(), relay.clone());
        let rpa = ScriptedRpa {
            calls: calls.clone(),
            steps: vec![Box::new(|| {}), Box::new(move || drain_all(&s2, &r2))],
        };
        let cfg = OrchestratorConfig {
            capture_wait_secs: 5,
            launch_timeout_secs: 0,
            ..Default::default()
        };
        let orch = orch_with(store.clone(), relay, rpa, cfg);
        let report = orch.process_job(job(&[L0, L1])).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2, "点空后应重点一次");
        assert!(!report.truncated);
        assert!(store.credential_is_fresh("AAA==", 1800).unwrap());
    }

    #[tokio::test]
    async fn test_watchdog_stall_requeues_inflight_and_continues() {
        // 入口页注入 L0（在途）后卡住 → stall_secs=0 → L0 放回队尾 → 重点 → 第二次跑完全部。
        let store = Arc::new(Store::open_in_memory().unwrap());
        let relay = RelayQueue::new();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (r1, s2, r2) = (relay.clone(), store.clone(), relay.clone());
        let rpa = ScriptedRpa {
            calls: calls.clone(),
            steps: vec![
                Box::new(move || {
                    r1.touch();
                    r1.bootstrap_next(); // 在途 L0，然后没有下文
                }),
                Box::new(move || drain_all(&s2, &r2)),
            ],
        };
        let cfg = OrchestratorConfig {
            capture_wait_secs: 5,
            stall_secs: 0,
            ..Default::default()
        };
        let orch = orch_with(store, relay.clone(), rpa, cfg);
        let report = orch.process_job(job(&[L0, L1])).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(!report.truncated, "有进展的重拉不占配额、最终跑完");
        assert!(report.remaining_links.is_empty());
    }

    #[tokio::test]
    async fn test_watchdog_isolates_after_second_stall_and_reports_remaining() {
        // L0 两次卡住 → 隔离；剩余 L1 跑完；结果 truncated 且 remaining_links 含 L0。
        let store = Arc::new(Store::open_in_memory().unwrap());
        let relay = RelayQueue::new();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (r1, r2, s3, r3) = (relay.clone(), relay.clone(), store.clone(), relay.clone());
        let rpa = ScriptedRpa {
            calls: calls.clone(),
            steps: vec![
                Box::new(move || {
                    r1.touch();
                    r1.bootstrap_next(); // 在途 L0 卡住
                }),
                Box::new(move || {
                    // 重点后先跑 L1，再到队尾的 L0 又卡住
                    r2.touch();
                    r2.bootstrap_next();
                    r2.advance(Some(L1), None);
                }),
                Box::new(move || drain_all(&s3, &r3)),
            ],
        };
        let cfg = OrchestratorConfig {
            capture_wait_secs: 5,
            stall_secs: 0,
            ..Default::default()
        };
        let orch = orch_with(store, relay.clone(), rpa, cfg);
        let report = orch.process_job(job(&[L0, L1])).await;
        assert!(report.truncated);
        assert_eq!(report.remaining_links, vec![L0.to_string()]);
    }

    #[tokio::test]
    async fn test_watchdog_aborts_on_verify_page() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let relay = RelayQueue::new();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let r1 = relay.clone();
        let rpa = ScriptedRpa {
            calls: calls.clone(),
            steps: vec![Box::new(move || {
                r1.touch();
                r1.bootstrap_next();
                r1.mark_verify(L0);
            })],
        };
        let cfg = OrchestratorConfig {
            capture_wait_secs: 5,
            ..Default::default()
        };
        let orch = orch_with(store, relay.clone(), rpa, cfg);
        let report = orch.process_job(job(&[L0, L1])).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1, "验证页不重拉");
        assert!(report.truncated);
        assert_eq!(report.remaining_links, vec![L0.to_string(), L1.to_string()]);
        assert_eq!(relay.pending_count(), 0, "收尾已清队列");
    }

    #[tokio::test]
    async fn test_watchdog_gives_up_after_max_relaunch() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let relay = RelayQueue::new();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let rpa = ScriptedRpa {
            calls: calls.clone(),
            steps: vec![],
        };
        let cfg = OrchestratorConfig {
            capture_wait_secs: 5,
            launch_timeout_secs: 0,
            max_relaunch: 2,
            ..Default::default()
        };
        let orch = orch_with(store, relay, rpa, cfg);
        let report = orch.process_job(job(&[L0])).await;
        assert_eq!(calls.load(Ordering::SeqCst), 3, "首点 + 2 次重拉");
        assert!(report.truncated);
        assert_eq!(report.remaining_links, vec![L0.to_string()]);
    }

    // -- 最后更新时间翻页 + 过期号集中续期后从原 offset 续采 --

    /// 采集脚本：`(请求, 该号第几次被调用) -> 预设结果`。
    type ScriptFn =
        Arc<dyn Fn(&ListRequest, usize) -> anyhow::Result<CollectOutcome> + Send + Sync>;

    /// 可编排的采集闭包：按 (biz, 调用序号) 返回预设结果，并记录请求。
    fn scripted_collect(calls: Arc<Mutex<Vec<ListRequest>>>, script: ScriptFn) -> CollectFn {
        Arc::new(move |req: ListRequest| {
            let calls = calls.clone();
            let script = script.clone();
            Box::pin(async move {
                let n = {
                    let mut g = calls.lock().unwrap();
                    let n = g.iter().filter(|r| r.biz == req.biz).count();
                    g.push(req.clone());
                    n
                };
                script(&req, n)
            })
        })
    }

    #[tokio::test]
    async fn test_since_paging_batch_refresh_and_resume() {
        // 两个号：AAA 链接级 since=100，BBB 走任务级 since=200。
        // 每号第 1 次采集：采到 1 条后凭证过期（next_offset=10）；集中续期一次后各自从 10 续采完成。
        let store = Arc::new(Store::open_in_memory().unwrap());
        let relay = RelayQueue::new();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (s1, r1) = (store.clone(), relay.clone());
        let (s2, r2) = (store.clone(), relay.clone());
        let rpa = ScriptedRpa {
            calls: calls.clone(),
            steps: vec![
                // 首点：接力跑完、两号抓到 key=K
                Box::new(move || drain_all(&s1, &r1)),
                // 集中续期：一次开浏览器，队列里两条取样链接都跑完，各自拿到新 key
                Box::new(move || {
                    assert_eq!(r2.rows().len(), 2, "两个过期号应在同一次续期里");
                    drain_all_with_key(&s2, &r2, "K2");
                }),
            ],
        };
        let creq = Arc::new(Mutex::new(Vec::new()));
        let store_c = store.clone();
        let script: ScriptFn = Arc::new(move |req, n| {
            let biz = req.biz.clone();
            let u = |m: i32| format!("https://mp.weixin.qq.com/s?__biz={biz}&mid={m}&idx=1&sn=x");
            if n == 0 {
                assert_eq!(req.plan.start_offset, 0);
                // 采到 1 条后过期（真实路径里 collector 会打点实测失效）
                store_c.mark_credential_invalid(&biz).unwrap();
                Ok(CollectOutcome {
                    total: 1,
                    pages: 1,
                    urls: vec![u(1)],
                    next_offset: Some(10),
                    cred_expired: true,
                    feedback: "过期(模拟)".into(),
                    ..Default::default()
                })
            } else {
                assert_eq!(req.plan.start_offset, 10, "应从中断 offset 续采");
                Ok(CollectOutcome {
                    total: 2,
                    pages: 2,
                    urls: vec![u(2), u(1)], // 故意重复 mid=1：回报不去重
                    reached_since: true,
                    feedback: "续采完成(模拟)".into(),
                    ..Default::default()
                })
            }
        });
        let mut orch = orch_with(
            store.clone(),
            relay.clone(),
            rpa,
            OrchestratorConfig {
                capture_wait_secs: 5,
                refresh_attempts: 3,
                refresh_wait_secs: 5,
                ..Default::default()
            },
        );
        orch.collect_list = scripted_collect(creq.clone(), script);

        let mut link_since = std::collections::HashMap::new();
        link_since.insert(L0.to_string(), 100.0);
        let job = Job {
            links: vec![L0.to_string(), LB.to_string()],
            last_updated_at: Some(200.0),
            link_since,
            ..Default::default()
        };
        let report = orch.process_job(job).await;

        // 首点 + 1 次集中续期 = 2 次开浏览器（不是每号一次）
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(!report.truncated, "{report:?}");
        // 链接平铺、按采集顺序、不去重：AAA#1, BBB#1, AAA#2, AAA#1, BBB#2, BBB#1
        assert_eq!(report.urls.len(), 6);
        assert!(report.urls[0].contains("AAA==") && report.urls[0].contains("mid=1"));
        assert!(report.urls[1].contains("BBB=="));
        assert_eq!(
            report
                .urls
                .iter()
                .filter(|u| u.contains("AAA==&mid=1"))
                .count(),
            2
        );
        // since 映射：AAA 链接级 100，BBB 任务级 200
        let reqs = creq.lock().unwrap().clone();
        assert_eq!(reqs.len(), 4);
        assert!(reqs
            .iter()
            .filter(|r| r.biz == "AAA==")
            .all(|r| r.plan.since == Some(100.0)));
        assert!(reqs
            .iter()
            .filter(|r| r.biz == "BBB==")
            .all(|r| r.plan.since == Some(200.0)));
        // 续期后热表：换 key 计数 2（K → K2），且新鲜
        let c = store.get_credential("AAA==").unwrap().unwrap();
        assert_eq!(c.refresh_count, 2);
        assert!(c.invalidated_at.is_none());
        // 任务落库带 since
        let jobs = store.recent_jobs(1).unwrap();
        let row = store.get_job(jobs[0].id).unwrap().unwrap();
        assert_eq!(row.last_updated_at, Some(200.0));
        assert_eq!(row.link_since.get(L0), Some(&100.0));
    }

    #[tokio::test]
    async fn test_refresh_failure_marks_account_truncated() {
        // AAA 过期后续期 3 轮都拿不到新 key → 该号交回（truncated + remaining_links=其任务链接）；
        // BBB 正常采完，其链接照常回报。
        let store = Arc::new(Store::open_in_memory().unwrap());
        let relay = RelayQueue::new();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (s1, r1) = (store.clone(), relay.clone());
        let rpa = ScriptedRpa {
            calls: calls.clone(),
            // 首点跑完；之后每次续期都点空（无请求）
            steps: vec![Box::new(move || drain_all(&s1, &r1))],
        };
        let creq = Arc::new(Mutex::new(Vec::new()));
        let store_c = store.clone();
        let script: ScriptFn = Arc::new(move |req, _n| {
            if req.biz == "AAA==" {
                store_c.mark_credential_invalid("AAA==").unwrap();
                Ok(CollectOutcome {
                    urls: vec![L1.to_string()],
                    next_offset: Some(10),
                    cred_expired: true,
                    ..Default::default()
                })
            } else {
                Ok(CollectOutcome {
                    urls: vec![LB.to_string()],
                    reached_since: true,
                    ..Default::default()
                })
            }
        });
        let mut orch = orch_with(
            store.clone(),
            relay.clone(),
            rpa,
            OrchestratorConfig {
                capture_wait_secs: 5,
                refresh_attempts: 3,
                refresh_wait_secs: 0,
                launch_timeout_secs: 0,
                ..Default::default()
            },
        );
        orch.collect_list = scripted_collect(creq.clone(), script);
        let job = Job {
            links: vec![L0.to_string(), LB.to_string()],
            last_updated_at: Some(1.0),
            ..Default::default()
        };
        let report = orch.process_job(job).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1 + 3, "首点 + 续期 3 次重试");
        assert!(report.truncated);
        assert_eq!(report.remaining_links, vec![L0.to_string()]);
        // 已采到的部分不丢：AAA 第一页 + BBB
        assert_eq!(report.urls, vec![L1.to_string(), LB.to_string()]);
    }
    // -- 凭证复用：热数据里凭证仍可用的号不接力，直接采 --

    /// 给某号预置一份新鲜凭证（模拟此前任务抓到过）。
    fn seed_fresh_credential(store: &Store, biz: &str, key: &str) {
        let cred = format!(
            "https://mp.weixin.qq.com/mp/getappmsgext?__biz={biz}&uin=U&key={key}&pass_ticket=P"
        );
        capture::capture_request(store, &cred, None, None);
    }

    /// 计数版 capture_start / capture_stop：断言代理只在真要接力时才起。
    fn counting_capture(
        orch: &mut Orchestrator,
    ) -> (
        Arc<std::sync::atomic::AtomicUsize>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (st, sp) = (starts.clone(), stops.clone());
        orch.capture_start = Arc::new(move || {
            st.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        });
        orch.capture_stop = Arc::new(move || {
            sp.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {})
        });
        (starts, stops)
    }

    #[tokio::test]
    async fn test_reuse_fresh_credential_skips_proxy_and_rpa() {
        // AAA 凭证新鲜 → 两条 AAA 长链都不接力：不起代理、不点 RPA、直接采 AAA 一次。
        let store = Arc::new(Store::open_in_memory().unwrap());
        seed_fresh_credential(&store, "AAA==", "OLD");
        let relay = RelayQueue::new();
        let rpa_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let rpa = ScriptedRpa {
            calls: rpa_calls.clone(),
            steps: vec![],
        };
        let creq = Arc::new(Mutex::new(Vec::new()));
        let mut orch = orch_with(
            store.clone(),
            relay.clone(),
            rpa,
            OrchestratorConfig {
                capture_wait_secs: 5,
                ..Default::default()
            },
        );
        orch.collect_list = sim_collect(store.clone(), creq.clone());
        let (starts, stops) = counting_capture(&mut orch);

        let report = orch.process_job(job(&[L0, L1])).await;
        assert_eq!(rpa_calls.load(Ordering::SeqCst), 0, "复用凭证不应点 RPA");
        assert_eq!(starts.load(Ordering::SeqCst), 0, "复用凭证不应起代理");
        assert_eq!(stops.load(Ordering::SeqCst), 0, "没起过的代理不该停");
        assert!(relay.rows().is_empty(), "接力队列不应有任何链接");
        assert!(!report.truncated, "{report:?}");
        assert_eq!(report.urls.len(), 1);
        let reqs = creq.lock().unwrap().clone();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].biz, "AAA==");
        // 凭证没被换掉（沿用旧 key）
        let c = store.get_credential("AAA==").unwrap().unwrap();
        assert_eq!(c.key.as_deref(), Some("OLD"));
        assert_eq!(c.refresh_count, 1);
    }

    #[tokio::test]
    async fn test_partial_reuse_only_relays_links_without_credential() {
        // AAA 新鲜、BBB 没抓过：只有 LB 进接力队列，RPA 点一次；两号都采到并回报。
        let store = Arc::new(Store::open_in_memory().unwrap());
        seed_fresh_credential(&store, "AAA==", "OLD");
        let relay = RelayQueue::new();
        let rpa_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (s1, r1) = (store.clone(), relay.clone());
        let rpa = ScriptedRpa {
            calls: rpa_calls.clone(),
            steps: vec![Box::new(move || {
                let rows: Vec<String> = r1.rows().into_iter().map(|r| r.url).collect();
                assert_eq!(rows, vec![LB.to_string()], "只有缺凭证的链接进接力队列");
                drain_all(&s1, &r1);
            })],
        };
        let creq = Arc::new(Mutex::new(Vec::new()));
        let mut orch = orch_with(
            store.clone(),
            relay.clone(),
            rpa,
            OrchestratorConfig {
                capture_wait_secs: 5,
                ..Default::default()
            },
        );
        orch.collect_list = sim_collect(store.clone(), creq.clone());
        let (starts, stops) = counting_capture(&mut orch);

        let report = orch.process_job(job(&[L0, LB, L1])).await;
        assert_eq!(rpa_calls.load(Ordering::SeqCst), 1);
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert_eq!(stops.load(Ordering::SeqCst), 1);
        assert!(!report.truncated, "{report:?}");
        let bizs: Vec<String> = creq.lock().unwrap().iter().map(|r| r.biz.clone()).collect();
        assert_eq!(bizs, vec!["AAA==".to_string(), "BBB==".to_string()]);
        assert_eq!(report.urls.len(), 2);
        assert_eq!(
            store
                .get_credential("AAA==")
                .unwrap()
                .unwrap()
                .key
                .as_deref(),
            Some("OLD")
        );
        assert!(store.credential_is_fresh("BBB==", 1800).unwrap());
    }

    #[tokio::test]
    async fn test_reused_credential_rejected_triggers_lazy_proxy_and_refresh() {
        // AAA 热数据显示新鲜，但真采时微信回 ret=-3 → 集中续期：此时才起代理、RPA 点种子换新 key，
        // 续上后从中断 offset 续采，最终完整回报。
        let store = Arc::new(Store::open_in_memory().unwrap());
        seed_fresh_credential(&store, "AAA==", "OLD");
        let relay = RelayQueue::new();
        let rpa_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (s1, r1) = (store.clone(), relay.clone());
        let rpa = ScriptedRpa {
            calls: rpa_calls.clone(),
            steps: vec![Box::new(move || drain_all_with_key(&s1, &r1, "NEW"))],
        };
        let creq = Arc::new(Mutex::new(Vec::new()));
        let store_c = store.clone();
        let script: ScriptFn = Arc::new(move |req, n| {
            let biz = req.biz.clone();
            let u = |m: i32| format!("https://mp.weixin.qq.com/s?__biz={biz}&mid={m}&idx=1&sn=x");
            if n == 0 {
                store_c.mark_credential_invalid(&biz).unwrap();
                Ok(CollectOutcome {
                    urls: vec![u(1)],
                    next_offset: Some(10),
                    cred_expired: true,
                    ..Default::default()
                })
            } else {
                assert_eq!(req.plan.start_offset, 10, "续上后应从中断 offset 续采");
                Ok(CollectOutcome {
                    urls: vec![u(2)],
                    reached_since: true,
                    ..Default::default()
                })
            }
        });
        let mut orch = orch_with(
            store.clone(),
            relay.clone(),
            rpa,
            OrchestratorConfig {
                capture_wait_secs: 5,
                refresh_attempts: 3,
                refresh_wait_secs: 5,
                ..Default::default()
            },
        );
        orch.collect_list = scripted_collect(creq.clone(), script);
        let (starts, stops) = counting_capture(&mut orch);

        let job = Job {
            links: vec![L0.to_string()],
            last_updated_at: Some(1.0),
            ..Default::default()
        };
        let report = orch.process_job(job).await;
        assert_eq!(rpa_calls.load(Ordering::SeqCst), 1, "只有续期这一次点 RPA");
        assert_eq!(starts.load(Ordering::SeqCst), 1, "代理在续期前才起");
        assert_eq!(stops.load(Ordering::SeqCst), 1, "起过就要停");
        assert!(!report.truncated, "{report:?}");
        assert_eq!(report.urls.len(), 2);
        assert_eq!(creq.lock().unwrap().len(), 2);
        let c = store.get_credential("AAA==").unwrap().unwrap();
        assert_eq!(c.key.as_deref(), Some("NEW"));
        assert!(c.invalidated_at.is_none());
    }

    #[tokio::test]
    async fn test_invalidated_credential_is_not_reused() {
        // 热数据里有 key 但已被实测失效（invalidated_at）→ 不复用，走接力重新抓。
        let store = Arc::new(Store::open_in_memory().unwrap());
        seed_fresh_credential(&store, "AAA==", "OLD");
        store.mark_credential_invalid("AAA==").unwrap();
        let relay = RelayQueue::new();
        let rpa_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (s1, r1) = (store.clone(), relay.clone());
        let rpa = ScriptedRpa {
            calls: rpa_calls.clone(),
            steps: vec![Box::new(move || drain_all_with_key(&s1, &r1, "NEW"))],
        };
        let mut orch = orch_with(
            store.clone(),
            relay.clone(),
            rpa,
            OrchestratorConfig {
                capture_wait_secs: 5,
                ..Default::default()
            },
        );
        let (starts, _) = counting_capture(&mut orch);
        let report = orch.process_job(job(&[L0])).await;
        assert_eq!(rpa_calls.load(Ordering::SeqCst), 1);
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert!(!report.truncated, "{report:?}");
        assert_eq!(
            store
                .get_credential("AAA==")
                .unwrap()
                .unwrap()
                .key
                .as_deref(),
            Some("NEW")
        );
    }

    // -- 定时巡检 / 整机限流退避 --

    fn fresh_account(store: &Store, biz: &str, nick: &str, pub_at: f64) -> String {
        let url = format!("http://mp.weixin.qq.com/s?__biz={biz}&mid=m1&idx=1&sn=abc");
        store.upsert_account(biz, Some(nick), None).unwrap();
        store
            .upsert_article(
                biz,
                "m1",
                1,
                &crate::model::ArticleFields {
                    content_url: Some(url.clone()),
                    published_at: Some(pub_at),
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .upsert_credential(
                biz,
                &crate::model::CredentialFields {
                    key: Some(format!("KEY-{biz}")),
                    uin: Some("U".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        url
    }

    /// 定时巡检：主循环向 sweep 要批次，走同一条 `process_job`，采完的号同样当场上报；
    /// 批次跑完各号写回 accounts（ok + last_published_at），一轮完成后进入停留。凭证新鲜 →
    /// 复用路径，不碰代理 / RPA（capture_start 被调用即 panic）。
    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // 就是要整段串行持锁（退避是进程级全局）
    async fn test_run_forever_sweeps_when_idle() {
        use std::sync::atomic::AtomicUsize;
        let _g = crate::runstate::COOLDOWN_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::runstate::cooldown_reset();

        let store = Arc::new(Store::open_in_memory().unwrap());
        fresh_account(&store, "SW==", "巡检号", 1000.0);
        let stop = Arc::new(AtomicBool::new(false));
        let reports = Arc::new(AtomicUsize::new(0));
        let calls: Arc<Mutex<Vec<ListRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let sweep = crate::sweep::Sweep::new(crate::sweep::SweepConfig {
            batch_size: 5,
            // 单测里批次要连着出：不设批次间隔 / 预算
            batch_gap_secs: 0,
            fail_pause_secs: 0,
            daily_budget: 0,
            ..Default::default()
        });
        let orch = Orchestrator {
            store: store.clone(),
            relay: RelayQueue::new(),
            rpa: Arc::new(crate::rpa::NoOpController),
            report: {
                let r = reports.clone();
                Arc::new(move |payload: ReportPayload| {
                    assert_eq!(payload.job_kind, "sweep");
                    assert_eq!(payload.account.biz, "SW==");
                    r.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async { ReportAck::accepted() })
                })
            },
            capture_start: Arc::new(|| {
                Box::pin(async { panic!("凭证复用路径不得起代理") })
            }),
            capture_stop: Arc::new(|| Box::pin(async {})),
            collect_list: {
                let calls = calls.clone();
                let store2 = store.clone();
                Arc::new(move |req: ListRequest| {
                    calls.lock().unwrap().push(req.clone());
                    let store2 = store2.clone();
                    Box::pin(async move {
                        // 模拟采到一篇更新的文章
                        let url = format!(
                            "http://mp.weixin.qq.com/s?__biz={}&mid=m2&idx=1&sn=def",
                            req.biz
                        );
                        store2
                            .upsert_article(
                                &req.biz,
                                "m2",
                                1,
                                &crate::model::ArticleFields {
                                    content_url: Some(url.clone()),
                                    published_at: Some(5000.0),
                                    ..Default::default()
                                },
                            )
                            .unwrap();
                        Ok(CollectOutcome {
                            urls: vec![url],
                            new: 1,
                            total: 1,
                            pages: 1,
                            ..Default::default()
                        })
                    })
                })
            },
            // 批次跑完 → 一轮完成进入停留 → 主循环第一次休眠即置 stop 退出。
            sleep: {
                let stop = stop.clone();
                Arc::new(move |_d| {
                    stop.store(true, Ordering::SeqCst);
                    Box::pin(async {})
                })
            },
            cfg: OrchestratorConfig {
                poll_secs: 1,
                ..Default::default()
            },
            sweep: Some(sweep.clone()),
        };
        orch.run_forever(stop).await;

        // 采集计划：since = 库里该号最新发布时间（1000），只采到早于它的就停
        let reqs = calls.lock().unwrap().clone();
        assert_eq!(reqs.len(), 1, "{reqs:?}");
        assert_eq!(reqs[0].biz, "SW==");
        assert_eq!(reqs[0].plan.since, Some(1000.0));
        // 巡检批次也逐号上报
        assert_eq!(reports.load(Ordering::SeqCst), 1);
        // 写回 accounts：ok + last_published_at = 新采到的 5000
        let a = store.get_account("SW==").unwrap().unwrap();
        assert_eq!(a.sweep_status.as_deref(), Some("ok"));
        assert_eq!(a.last_published_at, Some(5000.0));
        assert!(a.sweep_checked_at.is_some());
        // 一轮完成进入停留
        let st = sweep.status();
        assert_eq!(st.phase, crate::runstate::SweepPhase::Waiting);
        assert_eq!(st.pass_new_articles, 1);
        assert_eq!(st.passes_completed, 1);
        // jobs 表：巡检任务 status=done
        let jobs = store.recent_jobs(5).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].status, "done");
        // 干净批次：退避未被触发
        assert!(crate::runstate::cooldown_remaining_secs().is_none());
    }

    /// 采集撞到限流信号：本号与本批剩余号全部停采（记为未完成），并触发整机退避。
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn test_rate_limited_collect_stops_batch_and_triggers_cooldown() {
        let _g = crate::runstate::COOLDOWN_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::runstate::cooldown_reset();

        let store = Arc::new(Store::open_in_memory().unwrap());
        let a_url = fresh_account(&store, "RA==", "甲", 1000.0);
        let b_url = fresh_account(&store, "RB==", "乙", 1000.0);
        let orch = Orchestrator {
            store: store.clone(),
            relay: RelayQueue::new(),
            rpa: Arc::new(crate::rpa::NoOpController),
            report: Arc::new(|_| Box::pin(async { ReportAck::accepted() })),
            capture_start: Arc::new(|| Box::pin(async { Ok(()) })),
            capture_stop: Arc::new(|| Box::pin(async {})),
            collect_list: Arc::new(|req: ListRequest| {
                Box::pin(async move {
                    if req.biz == "RA==" {
                        Err(crate::wechat::RateLimited("文章列表接口返回 HTTP 429".into()).into())
                    } else {
                        Ok(CollectOutcome::default())
                    }
                })
            }),
            sleep: Arc::new(|_d| Box::pin(async {})),
            cfg: OrchestratorConfig::default(),
            sweep: None,
        };
        let rep = orch
            .process_job(Job {
                links: vec![a_url.clone(), b_url.clone()],
                ..Default::default()
            })
            .await;
        assert!(rep.rate_limited);
        assert!(rep.truncated);
        assert!(rep.remaining_links.contains(&a_url));
        assert!(rep.remaining_links.contains(&b_url), "剩余号也停采交回");
        assert!(rep.finished_bizs.is_empty());
        let snap = crate::runstate::cooldown_snapshot();
        assert_eq!(snap.level, 1);
        assert!(snap.reason.contains("429"));
        assert!(crate::runstate::cooldown_remaining_secs().is_some());
        crate::runstate::cooldown_reset();
    }
    /// 按号视图：任务里每个号都有一条 accounts——采到的号带 urls；采集异常的号 urls 为空、
    /// finished=false；从没打开过的短链号 biz 为 None、urls 为空；**只有采完的号上报了**（当场各报一次），
    /// 没采完的不报、`reported=false`。
    #[tokio::test]
    async fn test_account_reports_cover_every_account_even_when_empty() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let relay = RelayQueue::new();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (s1, r1) = (store.clone(), relay.clone());
        let rpa = ScriptedRpa {
            calls: calls.clone(),
            steps: vec![Box::new(move || {
                // 只接力打开长链（短链 SHORT 一直没打开）
                r1.touch();
                for row in r1.rows() {
                    if let Some(a) = parse_s_url(&row.url) {
                        let cred = format!(
                            "https://mp.weixin.qq.com/mp/getappmsgext?__biz={}&uin=U&key=K&pass_ticket=P",
                            a.biz
                        );
                        capture::capture_request(&s1, &cred, None, None);
                        r1.advance_with_biz(Some(&row.url), None, Some(&a.biz));
                    }
                }
            })],
        };
        type Reported = Arc<Mutex<Vec<ReportPayload>>>;
        let reported: Reported = Arc::new(Mutex::new(Vec::new()));
        let rep2 = reported.clone();
        let mut orch = orch_with(
            store.clone(),
            relay,
            rpa,
            OrchestratorConfig {
                capture_wait_secs: 1,
                capture_wait_per_link_secs: 0,
                ..Default::default()
            },
        );
        orch.report = Arc::new(move |payload| {
            rep2.lock().unwrap().push(payload);
            Box::pin(async { ReportAck::accepted() })
        });
        // AAA 采到 1 篇；BBB 采集异常
        orch.collect_list = Arc::new(|req: ListRequest| {
            Box::pin(async move {
                if req.biz == "AAA==" {
                    Ok(CollectOutcome {
                        total: 1,
                        new: 1,
                        pages: 1,
                        feedback: "采到1篇".into(),
                        urls: vec![L1.to_string()],
                        items: vec![CollectedArticle {
                            url: L1.to_string(),
                            title: Some("标题一".into()),
                            published_at: Some(1_700_000_000),
                            is_new: true,
                        }],
                        ..Default::default()
                    })
                } else {
                    Err(anyhow::anyhow!("模拟采集异常"))
                }
            })
        });
        const SHORT: &str = "https://mp.weixin.qq.com/s/shortcode";
        let job = Job {
            links: vec![L0.to_string(), LB.to_string(), SHORT.to_string()],
            ..Default::default()
        };
        let report = orch.process_job(job).await;

        assert_eq!(report.accounts.len(), 3, "{:?}", report.accounts);
        let a = &report.accounts[0];
        assert_eq!(a.biz.as_deref(), Some("AAA=="));
        assert_eq!(a.urls, vec![L1.to_string()]);
        assert!(a.finished && a.reported && a.report_error.is_none());
        let b = &report.accounts[1];
        assert_eq!(b.biz.as_deref(), Some("BBB=="));
        assert!(b.urls.is_empty() && !b.finished && !b.reported);
        let c = &report.accounts[2];
        assert_eq!(c.biz, None);
        assert!(c.urls.is_empty() && !c.finished && !c.reported);
        assert_eq!(c.links, vec![SHORT.to_string()]);
        // 平铺 urls 与按号视图一致；没跑完（BBB 异常 + 短链没打开）→ truncated
        assert_eq!(report.urls, vec![L1.to_string()]);
        assert!(report.truncated);
        // 只有列表正常获取到的 AAA 上报了（一次调用、一条载荷）；BBB 采集异常、短链没打开都不报
        let calls = reported.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "{calls:?}");
        let p = &calls[0];
        assert_eq!(p.account.biz, "AAA==");
        assert_eq!(p.job_kind, "sweep");
        assert_eq!(p.job_id, report.job_id);
        assert_eq!(p.articles.len(), 1);
        assert_eq!(p.articles[0].title.as_deref(), Some("标题一"));
        assert!(p.articles[0].is_new);
        // 任务级 ack 由各号 ack 合成：唯一上报的号成功 → ok；任务落库 done（未跑完由 truncated 另表）
        assert!(report.report_ack.as_ref().is_some_and(|a| a.ok));
        let row = store.get_job(report.job_id).unwrap().unwrap();
        assert_eq!(row.status, "done");
        let accs = row.result.unwrap()["accounts"].clone();
        assert_eq!(accs[0]["reported"], true);
        assert_eq!(accs[1]["reported"], false);
    }

    /// 每个号列表采完**当场**上报（不等整批）：上报调用夹在两次采集之间；按时间比对后无新文章的号
    /// 也报空数组；采集异常的号不报；某个号上报失败 → 任务级 ack 失败、任务记 error，其余号照常。
    #[tokio::test]
    async fn test_each_account_reported_right_after_its_list_is_collected() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let relay = RelayQueue::new();
        // 三个号凭证都新鲜：整批复用，不起代理不开微信，只看采集与回报的交错顺序
        for biz in ["AAA==", "BBB==", "CCC=="] {
            let cred = crate::model::CredentialFields {
                key: Some("K".into()),
                uin: Some("U".into()),
                ..Default::default()
            };
            store.upsert_credential(biz, &cred).unwrap();
        }
        const LC: &str = "https://mp.weixin.qq.com/s?__biz=CCC==&mid=5&idx=1&sn=e";
        let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let ev_c = events.clone();
        let ev_r = events.clone();
        let mut orch = orch_with(
            store.clone(),
            relay,
            ScriptedRpa {
                calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                steps: vec![],
            },
            OrchestratorConfig::default(),
        );
        orch.collect_list = Arc::new(move |req: ListRequest| {
            ev_c.lock().unwrap().push(format!("collect {}", req.biz));
            Box::pin(async move {
                match req.biz.as_str() {
                    // AAA 采到 1 篇
                    "AAA==" => Ok(CollectOutcome {
                        total: 1,
                        new: 1,
                        pages: 1,
                        urls: vec![L1.to_string()],
                        ..Default::default()
                    }),
                    // BBB 首页有文章但都早于最后发布时间 → urls 为空，仍算采完
                    "BBB==" => Ok(CollectOutcome {
                        total: 3,
                        pages: 1,
                        skipped_older: 3,
                        reached_since: true,
                        ..Default::default()
                    }),
                    // CCC 采集异常
                    _ => Err(anyhow::anyhow!("模拟采集异常")),
                }
            })
        });
        orch.report = Arc::new(move |payload: ReportPayload| {
            let biz = payload.account.biz.clone();
            ev_r.lock()
                .unwrap()
                .push(format!("report {biz} urls={}", payload.articles.len()));
            // BBB 的上报被接收端拒绝
            Box::pin(async move {
                if biz == "BBB==" {
                    ReportAck::from_status(502)
                } else {
                    ReportAck::from_status(204)
                }
            })
        });
        let mut link_since = HashMap::new();
        link_since.insert(LB.to_string(), 100.0);
        let job = Job {
            links: vec![L0.to_string(), LB.to_string(), LC.to_string()],
            link_since,
            ..Default::default()
        };
        let report = orch.process_job(job).await;

        // 顺序：采 A → 报 A → 采 B → 报 B（空数组）→ 采 C（异常，不报）
        assert_eq!(
            events.lock().unwrap().clone(),
            vec![
                "collect AAA==",
                "report AAA== urls=1",
                "collect BBB==",
                "report BBB== urls=0",
                "collect CCC==",
            ]
        );
        let a = &report.accounts[0];
        assert!(a.reported && a.report_error.is_none());
        let b = &report.accounts[1];
        assert!(b.finished && !b.reported);
        assert!(b.report_error.as_deref().unwrap().contains("502"), "{b:?}");
        let c = &report.accounts[2];
        assert!(!c.finished && !c.reported && c.report_error.is_none());
        // 任务级：有号回报失败 → 服务端错误
        let ack = report.report_ack.clone().unwrap();
        assert!(!ack.ok);
        assert!(ack.error.as_deref().unwrap().contains("BBB=="), "{ack:?}");
        assert_eq!(report.classify().0, JOB_OUTCOME_SERVER_ERROR);
        let row = store.get_job(report.job_id).unwrap().unwrap();
        assert_eq!(row.status, "error");
        assert_eq!(row.outcome.as_deref(), Some(JOB_OUTCOME_SERVER_ERROR));
    }
}
