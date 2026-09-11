//! 抓取历史文章 —— 按公众号的**长任务**：从最新一页起用 `getmsg` 逐页往旧翻，按目标
//! （条数 / 时间范围 / 全部）停下。与批量添加、定时巡检共用一条主循环和同一份微信号预算，
//! 但**同一时刻只有一个号在抓**（`history_jobs` 里最多一条 running / paused），页与页之间按
//! 任务开始时快照的间隔等待。
//!
//! **一页一个执行单元**：主循环每轮先看 [`due`]（下一页到点了没），到点就拿单元锁调 [`tick`]
//! 抓一页、写回进度、释放锁；没到点就去做手动任务或巡检。这样间隔规则不会被别的单元打破，
//! 别的单元也不会被一个几十页的历史任务饿死。
//!
//! 状态机：`running` ⇄ `paused(budget|blocked|cooldown)`（`resume_at` 到点由 [`due`] 自动转回 running）、
//! `paused(credential|user)`（人工「继续」）、终态 `done` / `cancelled` / `failed`。
//! 进度落库（`next_offset` 等），重启从断点续跑。终态与暂停时**上报一次**（`job_kind: "history"`，
//! `articles` 为本任务累计范围内的文章）并写 `run_log(kind="history")`。
//!
//! 与 Python 基准的差异：Python 无对应功能。

use std::future::Future;
use std::pin::Pin;

use anyhow::{bail, Result};

use crate::applog::{self, Stage};
use crate::collector::{
    is_account_blocked_err, is_credential_expired_err, is_rate_limited_err, CollectOutcome,
    ListPlan, ListRequest, ListSource,
};
use crate::model::{
    now, Epoch, HistoryEstimate, HistoryJob, HistoryJobView, HistoryStatus, HistoryTarget,
    RunLogEvent, HISTORY_CANCELLED, HISTORY_DONE, HISTORY_FAILED, HISTORY_PAUSED,
    HISTORY_PAUSE_BLOCKED, HISTORY_PAUSE_BUDGET, HISTORY_PAUSE_COOLDOWN, HISTORY_PAUSE_CREDENTIAL,
    HISTORY_PAUSE_RESTART, HISTORY_PAUSE_USER, HISTORY_RUNNING,
};
use crate::orchestrator::{CollectFn, ReportFn};
use crate::report::{ReportArticle, ReportPayload};
use crate::runstate;
use crate::store::Store;

/// 每页条数上限（`getmsg` 的 `count`；服务端实测按 10 封顶，放大只是探路）。
pub const PAGE_COUNT_MAX: i64 = 50;
/// 连续失败（网络 / 接口错误）达到这个次数记 `failed`。
pub const MAX_CONSECUTIVE_ERRORS: i64 = 3;
/// 凭证过期（`ret=-3`）后下一页最短等待（秒）：下一 tick 先接力换 key。
const CRED_RETRY_DELAY_SECS: f64 = 5.0;

/// 历史抓取的运行参数（来自运行配置 `history_*` + 列表节流项）。
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryConfig {
    /// 每页条数（1..=[`PAGE_COUNT_MAX`]，默认 10）。
    pub page_count: i64,
    /// 页间隔（秒，默认 60；下限 = 列表闸门上限）。
    pub gap_secs: u64,
    /// 为巡检保留的预算（历史任务不吃掉当前微信号 24 小时预算的最后这些次）。
    pub budget_reserve: i64,
    /// 当前微信号 24 小时预算（0 = 不限）。
    pub budget: i64,
    pub cred_ttl_secs: i64,
    pub rate_retry_wait_ms: u64,
    pub gap_min_ms: u64,
    pub gap_max_ms: u64,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            page_count: 10,
            gap_secs: 60,
            budget_reserve: 30,
            budget: 0,
            cred_ttl_secs: 30 * 60,
            rate_retry_wait_ms: 15_000,
            gap_min_ms: 8_000,
            gap_max_ms: 20_000,
        }
    }
}

impl HistoryConfig {
    /// 生效的每页条数。
    pub fn page_count(&self) -> i64 {
        self.page_count.clamp(1, PAGE_COUNT_MAX)
    }

    /// 生效的页间隔（秒）：不小于列表闸门上限，否则闸门会把它拉长、间隔失去意义。
    pub fn gap_secs(&self) -> u64 {
        self.gap_secs.max(self.gap_max_ms.div_ceil(1000))
    }
}

type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 单号接力换 key：`Ok(true)` = 换到了新鲜凭证；`Ok(false)` / `Err` = 没换到（原因进 `last_error`）。
pub type RelayFn<'a> = dyn Fn(String) -> BoxFut<'a, Result<bool>> + Sync + 'a;

/// [`tick`] 的外部依赖（都是编排器已有的注入项）。
pub struct TickDeps<'a> {
    pub collect: &'a CollectFn,
    pub relay: &'a RelayFn<'a>,
    pub report: &'a ReportFn,
}

// -----------------------------------------------------------------------------
// 任务生命周期（GUI 命令）
// -----------------------------------------------------------------------------

/// 校验目标。
fn validate_target(target: &HistoryTarget) -> Result<()> {
    if let Some(c) = target.count {
        if c < 1 {
            bail!("条数至少为 1");
        }
    }
    if let (Some(s), Some(u)) = (target.since_ts, target.until_ts) {
        if s >= u {
            bail!("起始日期必须早于截止日期");
        }
    }
    Ok(())
}

/// 巡检与历史抓取互斥：巡检开关开着（含「停止巡检」后当前批次还在收尾）时不能开始 / 继续历史任务。
fn ensure_sweep_off() -> Result<()> {
    if runstate::sweep_on() {
        if runstate::sweep_stopping() {
            bail!("巡检正在停止：等当前批次结束后再抓取历史文章");
        }
        bail!("巡检进行中：先在控制面板「停止巡检」，等本批结束后再抓取历史文章");
    }
    Ok(())
}

/// 开始一条历史抓取任务：巡检未开启、有激活微信号、公众号存在、没有别的活动任务、目标合法。
/// 每页条数 / 页间隔按当时配置快照进任务，之后改设置不影响它。
pub fn start(
    store: &Store,
    biz: &str,
    target: &HistoryTarget,
    cfg: &HistoryConfig,
) -> Result<HistoryJob> {
    validate_target(target)?;
    if store.get_account(biz)?.is_none() {
        bail!("公众号不存在：{biz}");
    }
    if store.wx_active()?.is_none() {
        bail!("没有激活的微信号：先在控制面板框选种子并开始巡检，抓到凭证后微信号会自动登记");
    }
    if let Some(j) = store.history_active()? {
        bail!(
            "『{}』的历史抓取进行中，同一时刻只抓一个号",
            store.account_label(&j.biz)
        );
    }
    ensure_sweep_off()?;
    let t = now();
    let job = HistoryJob {
        id: 0,
        biz: biz.to_string(),
        status: HISTORY_RUNNING.to_string(),
        paused_reason: None,
        target_count: target.count,
        since_ts: target.since_ts,
        until_ts: target.until_ts,
        page_count: cfg.page_count(),
        gap_secs: cfg.gap_secs() as i64,
        next_offset: 0,
        pages: 0,
        fetched: 0,
        matched: 0,
        new_articles: 0,
        next_page_at: Some(t),
        resume_at: None,
        started_at: t,
        finished_at: None,
        last_error: None,
        reached_end: false,
        created_at: t,
        errors: 0,
        articles_json: "[]".to_string(),
    };
    let job = store.history_insert(&job)?;
    applog::info(
        Stage::List,
        format!(
            "📜 开始抓取 {} 的历史文章：{}，每页 {} 条，页间隔 {}s",
            store.account_label(biz),
            describe_target(target),
            job.page_count,
            job.gap_secs
        ),
    );
    Ok(job)
}

/// 目标的人类可读描述。
pub fn describe_target(t: &HistoryTarget) -> String {
    let mut parts = Vec::new();
    if let Some(c) = t.count {
        parts.push(format!("{c} 篇"));
    }
    match (t.since_ts, t.until_ts) {
        (Some(s), Some(u)) => parts.push(format!(
            "{} ~ {}",
            applog::format_local(s),
            applog::format_local(u)
        )),
        (Some(s), None) => parts.push(format!("{} 起", applog::format_local(s))),
        (None, Some(u)) => parts.push(format!("{} 以前", applog::format_local(u))),
        (None, None) => {}
    }
    if parts.is_empty() {
        "全部（翻到底）".to_string()
    } else {
        parts.join("，")
    }
}

fn active_for(store: &Store, biz: &str) -> Result<HistoryJob> {
    match store.history_active()? {
        Some(j) if j.biz == biz => Ok(j),
        Some(j) => bail!(
            "当前进行中的是『{}』的历史抓取，不是这个号",
            store.account_label(&j.biz)
        ),
        None => bail!("该号没有进行中的历史抓取任务"),
    }
}

/// 人工暂停（当前页结束后生效：tick 收尾写回时会保留这个状态）。
pub fn pause(store: &Store, biz: &str) -> Result<HistoryJob> {
    let mut job = active_for(store, biz)?;
    if job.status != HISTORY_RUNNING {
        bail!("任务已是暂停状态");
    }
    job.status = HISTORY_PAUSED.to_string();
    job.paused_reason = Some(HISTORY_PAUSE_USER.to_string());
    job.next_page_at = None;
    job.resume_at = None;
    store.history_update(&job)?;
    applog::info(
        Stage::List,
        format!("⏸ 已暂停 {} 的历史抓取", store.account_label(biz)),
    );
    Ok(job)
}

/// 人工继续：任何暂停原因都可（预算 / 受限 / 退避会在下一 tick 再自查）；巡检开着时拒绝。
pub fn resume(store: &Store, biz: &str) -> Result<HistoryJob> {
    let mut job = active_for(store, biz)?;
    if job.status != HISTORY_PAUSED {
        bail!("任务没有暂停");
    }
    ensure_sweep_off()?;
    job.status = HISTORY_RUNNING.to_string();
    job.paused_reason = None;
    job.resume_at = None;
    job.next_page_at = Some(now());
    job.errors = 0;
    store.history_update(&job)?;
    applog::info(
        Stage::List,
        format!("▶ 继续 {} 的历史抓取", store.account_label(biz)),
    );
    Ok(job)
}

/// 应用启动时：上次退出时还在跑的历史任务转为暂停（原因 `restart`），不自动抢微信窗口，
/// 由用户点「继续」恢复。返回被暂停的任务（没有则 `None`）。
pub fn pause_on_restart(store: &Store) -> Result<Option<HistoryJob>> {
    let Some(mut job) = store.history_active()? else {
        return Ok(None);
    };
    if job.status != HISTORY_RUNNING {
        return Ok(None);
    }
    job.status = HISTORY_PAUSED.to_string();
    job.paused_reason = Some(HISTORY_PAUSE_RESTART.to_string());
    job.next_page_at = None;
    job.resume_at = None;
    store.history_update(&job)?;
    applog::info(
        Stage::List,
        format!(
            "⏸ {} 的历史抓取在上次退出时仍在进行，已暂停；在公众号列表点「继续」恢复",
            store.account_label(&job.biz)
        ),
    );
    Ok(Some(job))
}

/// 取消（当前页结束后生效）。
pub fn cancel(store: &Store, biz: &str) -> Result<HistoryJob> {
    let mut job = active_for(store, biz)?;
    job.status = HISTORY_CANCELLED.to_string();
    job.paused_reason = None;
    job.next_page_at = None;
    job.resume_at = None;
    job.finished_at = Some(now());
    store.history_update(&job)?;
    applog::info(
        Stage::List,
        format!(
            "⏹ 已取消 {} 的历史抓取（已抓 {} 页 {} 篇）",
            store.account_label(biz),
            job.pages,
            job.fetched
        ),
    );
    Ok(job)
}

// -----------------------------------------------------------------------------
// 视图 / 状态 / 估算
// -----------------------------------------------------------------------------

/// GUI 视图（多带昵称与目标对象）。
pub fn view(store: &Store, job: &HistoryJob) -> HistoryJobView {
    HistoryJobView {
        id: job.id,
        biz: job.biz.clone(),
        nickname: store.account_label(&job.biz),
        status: job.status.clone(),
        paused_reason: job.paused_reason.clone(),
        target: job.target(),
        page_count: job.page_count,
        gap_secs: job.gap_secs.max(0) as u64,
        pages: job.pages,
        fetched: job.fetched,
        matched: job.matched,
        new_articles: job.new_articles,
        next_offset: job.next_offset,
        next_page_at: job.next_page_at,
        resume_at: job.resume_at,
        started_at: job.started_at,
        finished_at: job.finished_at,
        last_error: job.last_error.clone(),
        reached_end: job.reached_end,
    }
}

/// 当前微信号近 24 小时已发的列表请求数与窗口内最早一次的时刻。
fn budget_used(store: &Store) -> (i64, Option<Epoch>) {
    let uin_hash = store.current_uin_hash().ok().flatten();
    store
        .list_calls_window(uin_hash.as_deref(), runstate::LIST_BUDGET_WINDOW_SECS)
        .unwrap_or((0, None))
}

/// 状态栏 / 公众号列表轮询用。
pub fn status(store: &Store, cfg: &HistoryConfig) -> HistoryStatus {
    let job = store
        .history_active()
        .ok()
        .flatten()
        .map(|j| view(store, &j));
    let (used, _) = budget_used(store);
    HistoryStatus {
        job,
        budget_used: used,
        budget: cfg.budget.max(0),
        budget_reserve: cfg.budget_reserve.max(0),
    }
}

/// 开始前的估算。
pub fn estimate(store: &Store, target: &HistoryTarget, cfg: &HistoryConfig) -> HistoryEstimate {
    let page_count = cfg.page_count();
    let gap = cfg.gap_secs() as i64;
    let pages = target
        .count
        .filter(|c| *c > 0)
        .map(|c| (c + page_count - 1) / page_count)
        .unwrap_or(0);
    let seconds = pages * gap;
    let (used, _) = budget_used(store);
    let budget = cfg.budget.max(0);
    let budget_available = if budget > 0 {
        (budget - used - cfg.budget_reserve.max(0)).max(0)
    } else {
        -1
    };
    let exceeds_budget = budget > 0 && pages > 0 && pages > budget_available;
    let mut note = if pages > 0 {
        format!(
            "预计请求约 {pages} 页，每页间隔 {gap} 秒，约需 {}",
            fmt_duration(seconds)
        )
    } else {
        "翻到底为止，页数无法预估".to_string()
    };
    if budget > 0 {
        note.push_str(&format!(
            "；当前微信号剩余可用预算 {budget_available} 次（已为巡检保留 {} 次）",
            cfg.budget_reserve.max(0)
        ));
        if exceeds_budget || pages == 0 {
            note.push_str("；预算用完会自动暂停，额度腾出后继续，可能分多日完成");
        }
    }
    if target.until_ts.is_some() {
        note.push_str("；截止日期越早，跳过的页越多，同样计入预算");
    }
    HistoryEstimate {
        pages,
        seconds,
        budget_available,
        exceeds_budget,
        note,
    }
}

fn fmt_duration(secs: i64) -> String {
    if secs >= 3600 {
        format!("{:.1} 小时", secs as f64 / 3600.0)
    } else if secs >= 60 {
        format!("{} 分钟", secs / 60)
    } else {
        format!("{secs} 秒")
    }
}

// -----------------------------------------------------------------------------
// 调度
// -----------------------------------------------------------------------------

/// 到点的任务：running 且 `next_page_at` 已到；或 paused 且 `resume_at` 已到（自动转回 running）。
pub fn due(store: &Store) -> Result<Option<HistoryJob>> {
    let Some(mut job) = store.history_active()? else {
        return Ok(None);
    };
    let t = now();
    if job.status == HISTORY_RUNNING {
        return Ok(if job.next_page_at.is_none_or(|at| at <= t) {
            Some(job)
        } else {
            None
        });
    }
    if job.status == HISTORY_PAUSED && job.resume_at.is_some_and(|at| at <= t) {
        applog::info(
            Stage::List,
            format!(
                "▶ {} 的历史抓取自动继续（{}）",
                store.account_label(&job.biz),
                match job.paused_reason.as_deref() {
                    Some(HISTORY_PAUSE_BUDGET) => "预算已腾出额度",
                    Some(HISTORY_PAUSE_BLOCKED) => "微信号限制期已过",
                    Some(HISTORY_PAUSE_COOLDOWN) => "整机退避已结束",
                    _ => "暂停期已过",
                }
            ),
        );
        job.status = HISTORY_RUNNING.to_string();
        job.paused_reason = None;
        job.resume_at = None;
        job.next_page_at = Some(t);
        store.history_update(&job)?;
        return Ok(Some(job));
    }
    Ok(None)
}

/// 下一次需要看历史任务的时刻（主循环据此缩短休眠）；没有活动任务返回 `None`。
pub fn next_due_at(store: &Store) -> Option<Epoch> {
    let job = store.history_active().ok().flatten()?;
    if job.status == HISTORY_RUNNING {
        Some(job.next_page_at.unwrap_or_else(now))
    } else {
        job.resume_at
    }
}

// -----------------------------------------------------------------------------
// 抓一页
// -----------------------------------------------------------------------------

/// 一页结果按目标过滤后的结论（纯函数，见 [`apply_page`]）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageDecision {
    /// 本页范围内的篇数 / 其中新入库的。
    pub matched: i64,
    pub new_articles: i64,
    /// 已达目标 / 到底，任务完成。
    pub done: bool,
    /// 完成原因（日志用）。
    pub reason: &'static str,
}

/// 把一页采集结果按目标过滤并累计进任务（不写库）。规则：`published_at > until` 跳过不计；
/// `< since` 判停（本页范围内的仍计）；条数达到即停；`next_offset` 为空（到底）即停。
/// 没有发布时间的文章按范围内计（无法判断）。
pub fn apply_page(job: &mut HistoryJob, out: &CollectOutcome) -> PageDecision {
    let mut d = PageDecision::default();
    job.pages += 1;
    job.fetched += out.total as i64;
    let mut saw_older = false;
    let mut articles: Vec<ReportArticle> =
        serde_json::from_str(&job.articles_json).unwrap_or_default();
    for it in &out.items {
        let p = it.published_at.map(|t| t as f64);
        if let (Some(u), Some(p)) = (job.until_ts, p) {
            if p > u {
                continue;
            }
        }
        if let (Some(s), Some(p)) = (job.since_ts, p) {
            if p < s {
                saw_older = true;
                continue;
            }
        }
        d.matched += 1;
        if it.is_new {
            d.new_articles += 1;
        }
        articles.push(ReportArticle {
            url: it.url.clone(),
            title: it.title.clone(),
            published_at: it.published_at,
            is_new: it.is_new,
        });
    }
    job.matched += d.matched;
    job.new_articles += d.new_articles;
    job.articles_json = serde_json::to_string(&articles).unwrap_or_else(|_| "[]".into());
    match out.next_offset {
        Some(o) => job.next_offset = o,
        None => job.reached_end = true,
    }
    if job.target_count.is_some_and(|c| job.matched >= c) {
        d.done = true;
        d.reason = "已达目标条数";
    } else if saw_older {
        d.done = true;
        d.reason = "已翻到起始日期之前";
    } else if job.reached_end {
        d.done = true;
        d.reason = "没有更多历史文章";
    }
    d
}

/// 暂停：写原因 / 自动恢复时刻，下一页不再排。
fn set_paused(job: &mut HistoryJob, reason: &str, resume_at: Option<Epoch>, err: Option<String>) {
    job.status = HISTORY_PAUSED.to_string();
    job.paused_reason = Some(reason.to_string());
    job.resume_at = resume_at;
    job.next_page_at = None;
    if err.is_some() {
        job.last_error = err;
    }
}

fn set_finished(job: &mut HistoryJob, status: &str, err: Option<String>) {
    job.status = status.to_string();
    job.paused_reason = None;
    job.resume_at = None;
    job.next_page_at = None;
    job.finished_at = Some(now());
    if err.is_some() {
        job.last_error = err;
    }
}

/// 写回进度。tick 期间用户可能已「暂停 / 取消」（GUI 命令直接改库），收尾时以库里的这两种状态为准，
/// 只把进度字段合并过去——即「当前页结束后生效」。
fn persist(store: &Store, mut job: HistoryJob) -> HistoryJob {
    if let Ok(Some(cur)) = store.history_get(job.id) {
        let user_paused = cur.status == HISTORY_PAUSED
            && cur.paused_reason.as_deref() == Some(HISTORY_PAUSE_USER);
        if cur.status == HISTORY_CANCELLED || user_paused {
            job.status = cur.status;
            job.paused_reason = cur.paused_reason;
            job.resume_at = None;
            job.next_page_at = None;
            job.finished_at = cur.finished_at;
        }
    }
    if let Err(e) = store.history_update(&job) {
        applog::warn(Stage::List, format!("历史抓取进度写库失败：{e}"));
    }
    job
}

/// 终态 / 暂停时上报一次（`job_kind: "history"`，`articles` 为累计范围内的文章）并写 `run_log`。
pub async fn report_job(store: &Store, job: &HistoryJob, report: &ReportFn) {
    let label = store.account_label(&job.biz);
    let nickname = store
        .get_account(&job.biz)
        .ok()
        .flatten()
        .and_then(|a| a.nickname)
        .filter(|n| !n.is_empty());
    let articles: Vec<ReportArticle> = serde_json::from_str(&job.articles_json).unwrap_or_default();
    let n = articles.len();
    let payload = ReportPayload::new(job.id, "history", &job.biz, nickname, articles);
    let ack = report(payload).await;
    if ack.skipped {
        // 未启用上报。
    } else if ack.ok {
        applog::info(
            Stage::Report,
            format!("上报 {label} 历史抓取（{}）：{n} 条文章链接", job.status),
        );
    } else {
        applog::error(
            Stage::Report,
            format!(
                "上报 {label} 历史抓取失败：{}",
                ack.error.as_deref().unwrap_or("上报服务未返回成功状态")
            ),
        );
    }
    let _ = store.append_run_log(&RunLogEvent {
        job_id: None,
        kind: "history".to_string(),
        pass_no: None,
        started_at: job.started_at,
        finished_at: now(),
        accounts: 1,
        ok: (job.status == HISTORY_DONE) as i64,
        failed: (job.status == HISTORY_FAILED) as i64,
        retry: 0,
        deferred: (job.status == HISTORY_PAUSED) as i64,
        new_articles: job.new_articles,
        urls: job.matched,
        list_calls: job.pages,
        rate_limited: job.paused_reason.as_deref() == Some(HISTORY_PAUSE_COOLDOWN),
        blocked: job.paused_reason.as_deref() == Some(HISTORY_PAUSE_BLOCKED),
        verify_hit: false,
        env_failure: false,
        truncated: false,
        note: Some(format!(
            "{}{}",
            job.status,
            job.paused_reason
                .as_ref()
                .map(|r| format!("({r})"))
                .unwrap_or_default()
        )),
    });
}

/// 抓**一页**（调用方已持执行单元锁）。返回写回后的任务。
pub async fn tick(
    store: &Store,
    cfg: &HistoryConfig,
    mut job: HistoryJob,
    deps: TickDeps<'_>,
) -> HistoryJob {
    if job.status != HISTORY_RUNNING {
        return job;
    }
    let label = store.account_label(&job.biz);
    let t = now();

    // 0) 整机退避中：等它结束再继续（主循环通常不会在退避中走到这里，兜底）。
    if let Some(remain) = runstate::cooldown_remaining_secs() {
        set_paused(
            &mut job,
            HISTORY_PAUSE_COOLDOWN,
            Some(t + remain as f64),
            None,
        );
        applog::warn(
            Stage::List,
            format!(
                "⏸ {label} 历史抓取暂停：整机退避中，{} 分钟后自动继续",
                remain.div_ceil(60)
            ),
        );
        let job = persist(store, job);
        report_job(store, &job, deps.report).await;
        return job;
    }
    // 1) 没有激活微信号：抓不到凭证，等人工处理。
    if store.wx_active().ok().flatten().is_none() {
        set_paused(
            &mut job,
            HISTORY_PAUSE_CREDENTIAL,
            None,
            Some("没有激活的微信号".to_string()),
        );
        runstate::alert_push(
            "history_paused",
            format!(
                "{label} 的历史抓取已暂停：没有激活的微信号，请在「微信号管理」激活后点「继续」"
            ),
        );
        let job = persist(store, job);
        report_job(store, &job, deps.report).await;
        return job;
    }
    // 2) 预算：已用 + 保留额度 ≥ 预算即暂停，等最早一次请求滑出 24 小时窗口。
    let budget = cfg.budget.max(0);
    let reserve = cfg.budget_reserve.max(0);
    let (used, oldest) = budget_used(store);
    runstate::set_list_calls_24h(used);
    if budget > 0 && used + reserve >= budget {
        let resume_at = oldest.unwrap_or(t) + runstate::LIST_BUDGET_WINDOW_SECS as f64;
        set_paused(&mut job, HISTORY_PAUSE_BUDGET, Some(resume_at), None);
        let msg = format!(
            "{label} 的历史抓取已暂停：当前微信号近 24 小时列表请求 {used}/{budget}（为巡检保留 {reserve} 次），{} 腾出额度后自动继续",
            applog::format_local(resume_at)
        );
        applog::warn(Stage::List, format!("⏸ {msg}"));
        runstate::alert_push("budget", msg);
        let job = persist(store, job);
        report_job(store, &job, deps.report).await;
        return job;
    }
    // 3) 凭证不新鲜：先接力换 key。
    if !store
        .credential_is_fresh(&job.biz, cfg.cred_ttl_secs)
        .unwrap_or(false)
    {
        applog::info(
            Stage::List,
            format!("{label} 历史抓取：凭证不可用，先接力打开该号文章换 key…"),
        );
        let refreshed = (deps.relay)(job.biz.clone()).await;
        let err = match refreshed {
            Ok(true) => None,
            Ok(false) => Some("接力后仍没拿到新凭证".to_string()),
            Err(e) => Some(e.to_string()),
        };
        if let Some(e) = err {
            set_paused(&mut job, HISTORY_PAUSE_CREDENTIAL, None, Some(e.clone()));
            let msg = format!("{label} 的历史抓取已暂停：换凭证失败（{e}）；排查后点「继续」");
            applog::error(Stage::List, format!("⏸ {msg}"));
            runstate::alert_push("history_paused", msg);
            let job = persist(store, job);
            report_job(store, &job, deps.report).await;
            return job;
        }
    }
    // 4) 抓一页。
    let plan = ListPlan {
        since: None,
        start_offset: job.next_offset,
        count: job.page_count.clamp(1, PAGE_COUNT_MAX),
        max_pages: 1,
        page_sleep_min_ms: 0,
        page_sleep_max_ms: 0,
        rate_retry_wait_ms: cfg.rate_retry_wait_ms,
        gap_min_ms: cfg.gap_min_ms,
        gap_max_ms: cfg.gap_max_ms,
        source: ListSource::History,
        paginate: true,
    };
    applog::info(
        Stage::List,
        format!(
            "📜 {label} 历史抓取第 {} 页（offset={}，已抓 {} 篇 / 范围内 {} 篇）…",
            job.pages + 1,
            job.next_offset,
            job.fetched,
            job.matched
        ),
    );
    let res = (deps.collect)(ListRequest {
        biz: job.biz.clone(),
        plan,
    })
    .await;
    let gap = job.gap_secs.max(0) as f64;
    match res {
        Ok(out) if out.cred_expired => {
            // ret=-3：凭证已打点失效，下一 tick 先接力换 key，再从同一 offset 续。
            job.errors = 0;
            job.next_page_at = Some(now() + CRED_RETRY_DELAY_SECS);
            applog::warn(
                Stage::List,
                format!(
                    "{label} 历史抓取：凭证已过期，稍后接力换 key 后从 offset={} 续",
                    job.next_offset
                ),
            );
            persist(store, job)
        }
        Ok(out) => {
            job.errors = 0;
            job.last_error = None;
            let d = apply_page(&mut job, &out);
            if d.done {
                set_finished(&mut job, HISTORY_DONE, None);
                applog::info(
                    Stage::List,
                    format!(
                        "✅ {label} 历史抓取完成：{}；共 {} 页 {} 篇，范围内 {} 篇，新增 {} 篇",
                        d.reason, job.pages, job.fetched, job.matched, job.new_articles
                    ),
                );
                let job = persist(store, job);
                report_job(store, &job, deps.report).await;
                job
            } else {
                job.next_page_at = Some(now() + gap);
                applog::info(
                    Stage::List,
                    format!(
                        "{label} 历史抓取第 {} 页：范围内 {} 篇（新增 {}），累计 {} 篇；{} 秒后下一页",
                        job.pages, d.matched, d.new_articles, job.matched, job.gap_secs
                    ),
                );
                persist(store, job)
            }
        }
        Err(e) if is_account_blocked_err(&e) => {
            let secs = runstate::block_trigger(format!("{label}（历史抓取）：{e}"));
            set_paused(
                &mut job,
                HISTORY_PAUSE_BLOCKED,
                Some(now() + secs as f64),
                Some(e.to_string()),
            );
            let msg = format!(
                "{label} 的历史抓取已暂停：微信号被限制（ret=-6），{} 小时后自动继续；换微信号后可手动「继续」",
                secs / 3600
            );
            applog::error(Stage::List, format!("⏸ {msg}"));
            runstate::alert_push("history_paused", msg);
            let job = persist(store, job);
            report_job(store, &job, deps.report).await;
            job
        }
        Err(e) if is_rate_limited_err(&e) => {
            let secs = runstate::cooldown_trigger(format!("{label}（历史抓取）：{e}"));
            set_paused(
                &mut job,
                HISTORY_PAUSE_COOLDOWN,
                Some(now() + secs as f64),
                Some(e.to_string()),
            );
            let msg = format!(
                "{label} 的历史抓取已暂停：撞到限流信号，整机退避 {} 分钟后自动继续",
                secs / 60
            );
            applog::error(Stage::List, format!("⏸ {msg}"));
            runstate::alert_push("history_paused", msg);
            let job = persist(store, job);
            report_job(store, &job, deps.report).await;
            job
        }
        Err(e) if is_credential_expired_err(&e) => {
            job.next_page_at = Some(now() + CRED_RETRY_DELAY_SECS);
            persist(store, job)
        }
        Err(e) => {
            job.errors += 1;
            job.last_error = Some(e.to_string());
            if job.errors >= MAX_CONSECUTIVE_ERRORS {
                set_finished(&mut job, HISTORY_FAILED, Some(e.to_string()));
                let msg = format!("{label} 的历史抓取失败：连续 {} 次出错（{e}）", job.errors);
                applog::error(Stage::List, format!("⛔ {msg}"));
                runstate::alert_push("history_paused", msg);
                let job = persist(store, job);
                report_job(store, &job, deps.report).await;
                job
            } else {
                job.next_page_at = Some(now() + gap);
                applog::warn(
                    Stage::List,
                    format!(
                        "{label} 历史抓取第 {} 页出错（第 {} 次）：{e}；{} 秒后重试同一页",
                        job.pages + 1,
                        job.errors,
                        job.gap_secs
                    ),
                );
                persist(store, job)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collector::CollectedArticle;
    use crate::model::{CredentialFields, ReportAck};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    fn store_with_account() -> Store {
        let store = Store::open_in_memory().unwrap();
        store
            .upsert_credential(
                "BIZ==",
                &CredentialFields {
                    uin: Some("U1".into()),
                    key: Some("K".into()),
                    pass_ticket: Some("P".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        // 微信号登记（凭证入库处平时会做；这里直接登记并激活）。
        let _ = store.wx_note_captured_uin("U1").unwrap();
        store.upsert_account("BIZ==", Some("测试号"), None).unwrap();
        store
    }

    fn item(pub_at: i64, is_new: bool) -> CollectedArticle {
        CollectedArticle {
            url: format!("https://mp.weixin.qq.com/s?__biz=BIZ==&mid={pub_at}&idx=1"),
            title: Some(format!("t{pub_at}")),
            published_at: Some(pub_at),
            is_new,
        }
    }

    fn page(items: Vec<CollectedArticle>, next: Option<i64>) -> CollectOutcome {
        CollectOutcome {
            total: items.len(),
            new: items.iter().filter(|i| i.is_new).count(),
            pages: 1,
            urls: items.iter().map(|i| i.url.clone()).collect(),
            next_offset: next,
            reached_since: next.is_none(),
            items,
            ..Default::default()
        }
    }

    fn job(target: HistoryTarget) -> HistoryJob {
        HistoryJob {
            biz: "BIZ==".into(),
            status: HISTORY_RUNNING.into(),
            target_count: target.count,
            since_ts: target.since_ts,
            until_ts: target.until_ts,
            page_count: 10,
            gap_secs: 60,
            articles_json: "[]".into(),
            ..Default::default()
        }
    }

    #[test]
    fn apply_page_filters_by_range_and_stops_on_older() {
        // 范围 [100, 300]：400 跳过不计，50 判停；范围内 3 篇
        let mut j = job(HistoryTarget {
            count: None,
            since_ts: Some(100.0),
            until_ts: Some(300.0),
        });
        let out = page(
            vec![
                item(400, true),
                item(300, true),
                item(200, false),
                item(100, true),
                item(50, true),
            ],
            Some(10),
        );
        let d = apply_page(&mut j, &out);
        assert_eq!((d.matched, d.new_articles), (3, 2));
        assert!(d.done, "出现早于起始日期的文章即停");
        assert_eq!(d.reason, "已翻到起始日期之前");
        assert_eq!(
            (j.pages, j.fetched, j.matched, j.new_articles),
            (1, 5, 3, 2)
        );
        assert_eq!(j.next_offset, 10);
        let arts: Vec<ReportArticle> = serde_json::from_str(&j.articles_json).unwrap();
        assert_eq!(arts.len(), 3);
        assert_eq!(arts[0].published_at, Some(300));
    }

    #[test]
    fn apply_page_count_target_and_reached_end() {
        let mut j = job(HistoryTarget {
            count: Some(3),
            since_ts: None,
            until_ts: None,
        });
        let d = apply_page(&mut j, &page(vec![item(3, true), item(2, true)], Some(10)));
        assert!(!d.done);
        assert_eq!(j.matched, 2);
        let d = apply_page(&mut j, &page(vec![item(1, true), item(0, true)], Some(20)));
        assert!(d.done, "范围内累计达到条数即停");
        assert_eq!(d.reason, "已达目标条数");
        assert_eq!(j.matched, 4, "本页范围内的全部计入");

        let mut j = job(HistoryTarget::default());
        let d = apply_page(&mut j, &page(vec![item(1, true)], None));
        assert!(d.done && j.reached_end);
        assert_eq!(d.reason, "没有更多历史文章");
        // 没有发布时间的文章按范围内计
        let mut j = job(HistoryTarget {
            count: None,
            since_ts: Some(100.0),
            until_ts: None,
        });
        let d = apply_page(
            &mut j,
            &page(
                vec![CollectedArticle {
                    url: "u".into(),
                    title: None,
                    published_at: None,
                    is_new: true,
                }],
                Some(10),
            ),
        );
        assert_eq!(d.matched, 1);
        assert!(!d.done);
    }

    #[test]
    fn start_validates_and_is_single() {
        let store = store_with_account();
        let cfg = HistoryConfig::default();
        assert!(start(
            &store,
            "BIZ==",
            &HistoryTarget {
                count: Some(0),
                ..Default::default()
            },
            &cfg
        )
        .is_err());
        assert!(start(
            &store,
            "BIZ==",
            &HistoryTarget {
                count: None,
                since_ts: Some(10.0),
                until_ts: Some(5.0)
            },
            &cfg
        )
        .is_err());
        assert!(start(&store, "NOPE==", &HistoryTarget::default(), &cfg).is_err());
        let j = start(&store, "BIZ==", &HistoryTarget::default(), &cfg).unwrap();
        assert!(j.id > 0);
        assert_eq!(j.gap_secs, 60);
        assert_eq!(j.page_count, 10);
        // 第二条：拒绝（单个、不排队）
        let err = start(&store, "BIZ==", &HistoryTarget::default(), &cfg).unwrap_err();
        assert!(err.to_string().contains("同一时刻只抓一个号"), "{err}");
        // due：next_page_at=now → 到点
        assert!(due(&store).unwrap().is_some());
        // 暂停 → 不到点；继续 → 到点；取消 → 没有活动任务
        pause(&store, "BIZ==").unwrap();
        assert!(due(&store).unwrap().is_none());
        assert!(pause(&store, "BIZ==").is_err());
        resume(&store, "BIZ==").unwrap();
        assert!(due(&store).unwrap().is_some());
        cancel(&store, "BIZ==").unwrap();
        assert!(store.history_active().unwrap().is_none());
        assert_eq!(
            store.history_latest_for("BIZ==").unwrap().unwrap().status,
            HISTORY_CANCELLED
        );
        // 取消后可以再开一条
        assert!(start(&store, "BIZ==", &HistoryTarget::default(), &cfg).is_ok());
    }

    #[test]
    fn due_auto_resumes_paused_with_resume_at() {
        let store = store_with_account();
        let mut j = start(
            &store,
            "BIZ==",
            &HistoryTarget::default(),
            &HistoryConfig::default(),
        )
        .unwrap();
        set_paused(&mut j, HISTORY_PAUSE_BUDGET, Some(now() - 1.0), None);
        store.history_update(&j).unwrap();
        let d = due(&store).unwrap().expect("resume_at 已过应自动继续");
        assert_eq!(d.status, HISTORY_RUNNING);
        assert!(d.paused_reason.is_none());
        // 人工暂停没有 resume_at：不自动继续
        let mut j = d;
        set_paused(&mut j, HISTORY_PAUSE_USER, None, None);
        store.history_update(&j).unwrap();
        assert!(due(&store).unwrap().is_none());
        assert!(next_due_at(&store).is_none());
    }

    #[test]
    fn estimate_pages_and_budget() {
        let store = store_with_account();
        let cfg = HistoryConfig {
            budget: 100,
            budget_reserve: 30,
            page_count: 10,
            gap_secs: 60,
            ..Default::default()
        };
        let e = estimate(
            &store,
            &HistoryTarget {
                count: Some(95),
                ..Default::default()
            },
            &cfg,
        );
        assert_eq!((e.pages, e.seconds, e.budget_available), (10, 600, 70));
        assert!(!e.exceeds_budget);
        let e = estimate(
            &store,
            &HistoryTarget {
                count: Some(800),
                ..Default::default()
            },
            &cfg,
        );
        assert_eq!(e.pages, 80);
        assert!(e.exceeds_budget);
        let e = estimate(&store, &HistoryTarget::default(), &cfg);
        assert_eq!(e.pages, 0);
        assert!(e.note.contains("翻到底"));
        // 页间隔不低于闸门上限
        let c = HistoryConfig {
            gap_secs: 5,
            gap_max_ms: 20_000,
            ..Default::default()
        };
        assert_eq!(c.gap_secs(), 20);
        assert_eq!(
            HistoryConfig {
                page_count: 999,
                ..Default::default()
            }
            .page_count(),
            PAGE_COUNT_MAX
        );
    }

    /// 假采集：按调用次数返回预置页；假接力：记录调用并把凭证刷新。
    struct Fake {
        pages: Mutex<Vec<Result<CollectOutcome>>>,
        calls: AtomicUsize,
        relays: AtomicUsize,
        reports: Mutex<Vec<ReportPayload>>,
    }

    fn deps(fake: &Arc<Fake>, store: &Arc<Store>) -> (CollectFn, ReportFn) {
        let f = fake.clone();
        let collect: CollectFn = Arc::new(move |_req| {
            let f = f.clone();
            Box::pin(async move {
                f.calls.fetch_add(1, Ordering::SeqCst);
                let mut p = f.pages.lock().unwrap();
                if p.is_empty() {
                    Ok(CollectOutcome::default())
                } else {
                    p.remove(0)
                }
            })
        });
        let f = fake.clone();
        let report: ReportFn = Arc::new(move |payload| {
            let f = f.clone();
            Box::pin(async move {
                f.reports.lock().unwrap().push(payload);
                ReportAck::accepted()
            })
        });
        let _ = store;
        (collect, report)
    }

    async fn run_tick(
        store: &Arc<Store>,
        fake: &Arc<Fake>,
        cfg: &HistoryConfig,
        job: HistoryJob,
        relay_ok: bool,
    ) -> HistoryJob {
        let (collect, report) = deps(fake, store);
        let f = fake.clone();
        let s = store.clone();
        fn relay_of<'a, F: Fn(String) -> BoxFut<'a, Result<bool>> + Sync + 'a>(f: F) -> F {
            f
        }
        let relay = relay_of(move |biz: String| {
            let f = f.clone();
            let s = s.clone();
            Box::pin(async move {
                f.relays.fetch_add(1, Ordering::SeqCst);
                if relay_ok {
                    s.upsert_credential(
                        &biz,
                        &CredentialFields {
                            uin: Some("U1".into()),
                            key: Some("K2".into()),
                            ..Default::default()
                        },
                    )
                    .unwrap();
                    Ok(true)
                } else {
                    Ok(false)
                }
            })
        });
        tick(
            store,
            cfg,
            job,
            TickDeps {
                collect: &collect,
                relay: &relay,
                report: &report,
            },
        )
        .await
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn tick_runs_pages_until_count_reached_and_reports_once() {
        let _g = runstate::COOLDOWN_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        runstate::cooldown_reset();
        runstate::alert_reset();
        let store = Arc::new(store_with_account());
        let cfg = HistoryConfig {
            budget: 0,
            ..Default::default()
        };
        let job = start(
            &store,
            "BIZ==",
            &HistoryTarget {
                count: Some(3),
                ..Default::default()
            },
            &cfg,
        )
        .unwrap();
        let fake = Arc::new(Fake {
            pages: Mutex::new(vec![
                Ok(page(vec![item(30, true), item(20, true)], Some(10))),
                Ok(page(vec![item(10, false), item(5, true)], Some(20))),
            ]),
            calls: AtomicUsize::new(0),
            relays: AtomicUsize::new(0),
            reports: Mutex::new(Vec::new()),
        });
        let j = run_tick(&store, &fake, &cfg, job, true).await;
        assert_eq!(j.status, HISTORY_RUNNING);
        assert_eq!((j.pages, j.matched, j.next_offset), (1, 2, 10));
        assert!(
            j.next_page_at.is_some_and(|t| t > now() + 50.0),
            "按页间隔排下一页"
        );
        assert!(fake.reports.lock().unwrap().is_empty(), "进行中不上报");
        // 第二页：达到条数 → done，上报一次（累计 4 篇）
        let mut j = j;
        j.next_page_at = Some(now() - 1.0);
        store.history_update(&j).unwrap();
        let j = run_tick(&store, &fake, &cfg, j, true).await;
        assert_eq!(j.status, HISTORY_DONE);
        assert_eq!((j.pages, j.matched, j.new_articles), (2, 4, 3));
        let reports = fake.reports.lock().unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].job_kind, "history");
        assert_eq!(reports[0].articles.len(), 4);
        assert_eq!(fake.relays.load(Ordering::SeqCst), 0, "凭证新鲜不接力");
        assert!(store.history_active().unwrap().is_none());
        // run_log 记了 history
        let runs = store.run_logs_since(0.0, 10).unwrap();
        assert!(runs.iter().any(|r| r.kind == "history" && r.ok == 1));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn tick_pauses_on_budget_reserve_and_relays_when_stale() {
        let _g = runstate::COOLDOWN_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        runstate::cooldown_reset();
        runstate::alert_reset();
        let store = Arc::new(store_with_account());
        // 预算 5、保留 3：已发 2 次即暂停
        for i in 0..2 {
            store
                .append_list_call(&crate::model::ListCallEvent {
                    biz: "BIZ==".into(),
                    source: "sweep".into(),
                    job_id: None,
                    page: i,
                    outcome: "ok".into(),
                    latency_ms: 1,
                    articles: 0,
                    new_articles: 0,
                    note: None,
                    factors: crate::model::ListCallFactors {
                        uin_hash: crate::model::short_hash("U1"),
                        ..Default::default()
                    },
                })
                .unwrap();
        }
        let cfg = HistoryConfig {
            budget: 5,
            budget_reserve: 3,
            ..Default::default()
        };
        let job = start(&store, "BIZ==", &HistoryTarget::default(), &cfg).unwrap();
        let fake = Arc::new(Fake {
            pages: Mutex::new(vec![Ok(page(vec![item(1, true)], Some(10)))]),
            calls: AtomicUsize::new(0),
            relays: AtomicUsize::new(0),
            reports: Mutex::new(Vec::new()),
        });
        let j = run_tick(&store, &fake, &cfg, job, true).await;
        assert_eq!(j.status, HISTORY_PAUSED);
        assert_eq!(j.paused_reason.as_deref(), Some(HISTORY_PAUSE_BUDGET));
        assert!(j.resume_at.is_some());
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0, "预算不够不发请求");
        assert_eq!(fake.reports.lock().unwrap().len(), 1, "暂停时上报一次");
        assert!(runstate::alert_latest().is_some_and(|a| a.kind == "budget"));

        // 预算充足但凭证失效 → 先接力再抓
        store.mark_credential_invalid("BIZ==").unwrap();
        let cfg = HistoryConfig {
            budget: 0,
            ..Default::default()
        };
        let mut j = j;
        j.status = HISTORY_RUNNING.into();
        j.paused_reason = None;
        j.next_page_at = Some(now());
        store.history_update(&j).unwrap();
        let j = run_tick(&store, &fake, &cfg, j, true).await;
        assert_eq!(fake.relays.load(Ordering::SeqCst), 1, "凭证失效先接力");
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
        assert_eq!(j.status, HISTORY_RUNNING);
        assert_eq!(j.pages, 1);

        // 接力失败 → paused(credential)，不自动恢复
        store.mark_credential_invalid("BIZ==").unwrap();
        let mut j = j;
        j.next_page_at = Some(now());
        store.history_update(&j).unwrap();
        let j = run_tick(&store, &fake, &cfg, j, false).await;
        assert_eq!(j.paused_reason.as_deref(), Some(HISTORY_PAUSE_CREDENTIAL));
        assert!(j.resume_at.is_none());
        assert!(due(&store).unwrap().is_none());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn tick_blocked_pauses_and_user_cancel_wins_over_progress() {
        let _g = runstate::COOLDOWN_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        runstate::cooldown_reset();
        runstate::alert_reset();
        let store = Arc::new(store_with_account());
        let cfg = HistoryConfig {
            budget: 0,
            ..Default::default()
        };
        let job = start(&store, "BIZ==", &HistoryTarget::default(), &cfg).unwrap();
        let fake = Arc::new(Fake {
            pages: Mutex::new(vec![
                Err(crate::wechat::AccountBlocked("ret=-6".into()).into()),
                Ok(page(vec![item(1, true)], Some(10))),
                Err(anyhow::anyhow!("网络错误")),
                Err(anyhow::anyhow!("网络错误")),
                Err(anyhow::anyhow!("网络错误")),
            ]),
            calls: AtomicUsize::new(0),
            relays: AtomicUsize::new(0),
            reports: Mutex::new(Vec::new()),
        });
        let j = run_tick(&store, &fake, &cfg, job, true).await;
        assert_eq!(j.paused_reason.as_deref(), Some(HISTORY_PAUSE_BLOCKED));
        assert!(j.resume_at.is_some_and(|t| t > now() + 3600.0));
        assert!(
            runstate::cooldown_remaining_secs().is_some(),
            "封号触发整机退避"
        );
        runstate::cooldown_reset();

        // 用户在 tick 期间取消：收尾以取消为准，但进度字段合并
        let mut j = j;
        j.status = HISTORY_RUNNING.into();
        j.paused_reason = None;
        store.history_update(&j).unwrap();
        cancel(&store, "BIZ==").unwrap();
        let j = run_tick(&store, &fake, &cfg, j, true).await;
        assert_eq!(j.status, HISTORY_CANCELLED);
        assert_eq!(j.pages, 1, "本页进度仍写回");

        // 连续 3 次错误 → failed
        let job2 = start(&store, "BIZ==", &HistoryTarget::default(), &cfg).unwrap();
        let mut j = job2;
        for _ in 0..3 {
            j.next_page_at = Some(now());
            store.history_update(&j).unwrap();
            j = run_tick(&store, &fake, &cfg, j, true).await;
        }
        assert_eq!(j.status, HISTORY_FAILED);
        assert_eq!(j.errors, 3);
        runstate::cooldown_reset();
        runstate::alert_reset();
    }
}
