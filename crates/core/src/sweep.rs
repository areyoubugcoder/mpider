//! 定时巡检（sweep）—— 本地全库公众号按轮自更新最新文章列表（2026-09-05，Python 无对应）。
//!
//! **它不是第二条流水线**，而是编排主循环（[`crate::orchestrator::Orchestrator::run_forever`]）的
//! 第二个任务源：没有待处理的手动任务时，向本模块要「下一批」——一批 = 库里 `batch_size` 个还没在本轮
//! 处理过的号，每号一条**最新发布的文章长链**做接力取样 + 该号的 `last_published_at` 做翻页
//! 依据，合成一条 [`Job`]（`kind = Sweep`）交给现有 `process_job` 跑完整链：凭证复用 →（按需）
//! 接力换 key → 翻页到「最后发布时间」→ 途中过期集中续期。批次跑完由 [`Sweep::finish_batch`]
//! 据 [`Report`] 把各号结果写回 `accounts`（成功：`last_published_at` = 该号当前最新一篇的发布
//! 时间，下一轮据此翻页；失败：本轮内排到后面重试，重试用尽记 `failed` 等下一轮）。
//!
//! 一轮 = 「所有参与巡检的号都处理过一次」；一轮跑完停留 `idle_secs`（默认 1 小时）再开始
//! 下一轮。轮的边界只靠 `accounts.sweep_checked_at < 本轮开始时刻` 判定，`本轮开始时刻` 与
//! `下一轮开始时刻` 持久化在 `config` 表——**重启后从断点继续**，不会从头再来。
//!
//! 与手动任务的关系：手动任务优先，巡检只在批次边界让路（一批的时间 = 几分钟）；限流退避是
//! 整机级的（[`crate::runstate`]），两个任务源都遵守。
//!
//! 验证页：接力打开取样链接命中人机验证页时，那一批会被看门狗中止。真实原因多半是那条链接
//! 的 `sn` 已失效（坏链假阳性），所以先把该链接记入「坏样本」、下次换次新一篇；**连续两批**都
//! 命中才当真限流触发整机退避。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::applog::{self, Stage};
use crate::model::{now, Epoch, Job, JobKind};
use crate::orchestrator::Report;
use crate::proxy_addon::{is_short_article_link, parse_s_url};
use crate::runstate::{self, SweepPhase, SweepStatus};
use crate::store::Store;

/// `config` 表里的持久化键：本轮开始时刻 / 下一轮开始时刻（epoch 秒）。
pub const KEY_PASS_STARTED_AT: &str = "sweep_pass_started_at";
pub const KEY_NEXT_PASS_AT: &str = "sweep_next_pass_at";

/// 巡检配置。
#[derive(Clone, Debug)]
pub struct SweepConfig {
    /// 一轮跑完后停留多久再开始下一轮（秒）。
    pub idle_secs: u64,
    /// 每批几个号（受凭证 TTL 与等待上限约束：一批接力 + 采集要在 key 有效期内跑完）。
    pub batch_size: usize,
    /// 取样备选：该号最新的前 N 篇（打不开时按发布时间往前换）。应 ≥ `max_swaps + 1`。
    pub sample_pool: usize,
    /// 同一轮内一个号最多**换几条**取样链接重新接力（文章被删 / 设为隐私 / 白屏 / 验证页都算打不开）；
    /// 换完仍无结果记 `failed` 跳过，等下一轮。总尝试次数 = `max_swaps + 1`。
    /// 凭证抓到了但采集失败的情况也计入同一个次数（不换链接）。
    pub max_swaps: i64,
    /// 连续几批接力命中验证页才判整机限流退避。
    pub verify_streak_limit: u32,
    /// 环境故障（接力等待期间没有任何文章请求：RPA 点不开种子 / 窗口被关 / 代理没流量）后
    /// 暂停多久再试（秒）。这类故障与号无关，不计各号的失败次数。
    pub env_pause_secs: u64,
    /// 两个批次之间至少隔多久（秒）。凭证全可复用时一批只要几十秒，不隔开就是连续轰 getmsg。
    pub batch_gap_secs: u64,
    /// 一批**一个号都没成功**且有号要重试（采集异常 / 续期失败）时，整个巡检先停多久（秒）再继续。
    /// 2026-09-06 教训：被限后失败号立刻回到下一批，每号 4 次尝试在 1 分钟内打光，只会加重封禁。
    pub fail_pause_secs: u64,
    /// 每号列表（getmsg）请求预算（0 = 不限）：当前微信号近 24 小时累计（所有路径合计）达到后巡检暂停，
    /// 等最早一次请求滑出窗口再继续；手动任务同样受限（`Orchestrator::run_forever` 领任务前也查）。
    pub daily_budget: i64,
}

impl Default for SweepConfig {
    fn default() -> Self {
        Self {
            idle_secs: 3600,
            batch_size: 20,
            sample_pool: 6,
            max_swaps: 3,
            verify_streak_limit: 2,
            env_pause_secs: 600,
            batch_gap_secs: 60,
            fail_pause_secs: 900,
            daily_budget: 180,
        }
    }
}

/// 单号「重新巡检」的结果（GUI 提示用；逻辑在 `runner::sweep_retry_account`）。
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct RetryOutcome {
    /// 是否采到了该号的最新列表（直采或接力换 key 后采）；`false` = 本次没采到（原因见 `message`）。
    pub collected: bool,
    /// 本次是否走了接力（凭证不可用 → 起代理、开微信打开该号最新一篇换 key）；`false` = 凭证有效直采。
    pub relayed: bool,
    /// 人类可读结论。
    pub message: String,
    /// 本次采到的篇数 / 新增篇数。
    pub total: usize,
    pub new_articles: usize,
    /// 采完后该号最新一篇的发布时间（已写回 `accounts.last_published_at`）。
    pub last_published_at: Option<Epoch>,
}

/// 一批巡检任务：合成的 [`Job`] + 各号的取样链接（收尾时据此对回结果）。
#[derive(Clone, Debug)]
pub struct SweepBatch {
    pub job: Job,
    /// 本批的号（与 `job.links` 一一对应、同序）。
    pub bizs: Vec<String>,
    /// 号 → 取样链接。
    pub sample_by_biz: HashMap<String, String>,
    /// 本批开始时的进度（起始序号，1 起），日志用。
    pub index_from: usize,
}

/// 进程内状态（持久化的两项另见 `config` 表）。
#[derive(Debug, Default)]
struct State {
    pass_started_at: Option<Epoch>,
    pass_total: usize,
    pass_done: usize,
    pass_new_articles: usize,
    next_pass_at: Option<Epoch>,
    passes_completed: u32,
    /// 号 → 本轮打不开的取样链接（验证页 / 已删除 / 隐私 / 白屏被隔离；下次取样跳过、换下一篇）。
    bad_samples: HashMap<String, HashSet<String>>,
    /// 连续命中验证页的批数。
    verify_streak: u32,
    /// 一个批次正在编排器里跑。
    in_batch: bool,
    last_error: Option<String>,
    /// 「没有参与巡检的号」只提醒一次（避免每次轮询刷屏）。
    warned_empty: bool,
    /// 环境故障 / 整批失败 / 预算用完的暂停截止时刻与原因。
    paused_until: Option<Epoch>,
    paused_reason: Option<String>,
    /// 批次间隔：下一批最早何时可以开始。
    next_batch_at: Option<Epoch>,
    /// 「今日预算已用完」只提醒一次。
    warned_budget: bool,
    /// 当前 / 最近一轮的轮次号（跨重启单调递增，持久化在 `config.sweep_pass_seq`；`run_log.pass_no`）。
    pass_seq: i64,
}

/// 巡检调度器（由 runner 装配、挂在 `Orchestrator.sweep` 上；`Arc` 共享）。
pub struct Sweep {
    pub cfg: SweepConfig,
    state: Mutex<State>,
}

/// 取样链接是否可用于该号的接力：带 `__biz` 的长链要与号一致；短链（批量添加记下的种子）打开后
/// 由微信 302 到长链，biz 在打开时才知道，直接放行。
pub fn sample_matches(url: &str, biz: &str) -> bool {
    parse_s_url(url).is_some_and(|p| p.biz == biz) || is_short_article_link(url)
}

/// 「开始巡检」前置检查：有活动的历史抓取任务（进行中 / 暂停）时不能开巡检——预算类暂停会自动恢复，
/// 两条链会抢同一个微信窗口与同一份预算。
pub fn start_check(store: &Store) -> anyhow::Result<()> {
    if let Some(j) = store.history_active()? {
        anyhow::bail!(
            "『{}』的历史抓取{}，先取消它（或等它完成）再开始巡检",
            store.account_label(&j.biz),
            if j.status == crate::model::HISTORY_PAUSED {
                "已暂停但未结束"
            } else {
                "进行中"
            }
        );
    }
    Ok(())
}

/// 秒数 → 「N 小时 M 分」/「M 分 S 秒」。
fn fmt_secs(secs: u64) -> String {
    if secs >= 3600 {
        format!("{} 小时 {} 分", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{} 分 {} 秒", secs / 60, secs % 60)
    } else {
        format!("{secs} 秒")
    }
}

impl Sweep {
    pub fn new(cfg: SweepConfig) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            state: Mutex::new(State::default()),
        })
    }

    /// 调度器启动：从 `config` 表恢复轮状态并透出一行概况。
    pub fn on_start(&self, store: &Store) {
        let mut st = self.state.lock().unwrap();
        runstate::list_budget_check(store, self.cfg.daily_budget);
        st.pass_started_at = read_epoch(store, KEY_PASS_STARTED_AT);
        st.pass_seq = store.sweep_pass_seq().unwrap_or(0);
        st.next_pass_at = read_epoch(store, KEY_NEXT_PASS_AT).filter(|t| *t > now());
        if st.next_pass_at.is_some() {
            st.pass_started_at = None;
        }
        let total = store.sweep_total().unwrap_or(0) as usize;
        if let Some(started) = st.pass_started_at {
            let pending = store.sweep_pending_count(started).unwrap_or(0) as usize;
            st.pass_total = total;
            st.pass_done = total.saturating_sub(pending);
            applog::info(
                Stage::Sweep,
                format!(
                    "🔄 定时巡检已开启：{total} 个号参与，每批 {} 个，一轮结束后停留 {}；从上轮断点继续（还剩 {pending} 个号）",
                    self.cfg.batch_size,
                    fmt_secs(self.cfg.idle_secs)
                ),
            );
        } else {
            applog::info(
                Stage::Sweep,
                format!(
                    "🔄 定时巡检已开启：{total} 个号参与，每批 {} 个，一轮结束后停留 {}{}",
                    self.cfg.batch_size,
                    fmt_secs(self.cfg.idle_secs),
                    st.next_pass_at
                        .map(|t| format!("；下一轮 {} 开始", applog::format_local(t)))
                        .unwrap_or_default()
                ),
            );
        }
        applog::info(
            Stage::Sweep,
            format!(
                "巡检节流：批次间隔 {}，整批失败暂停 {}，每号列表请求预算 {}（当前号近 24 小时已用 {}）",
                fmt_secs(self.cfg.batch_gap_secs),
                fmt_secs(self.cfg.fail_pause_secs),
                if self.cfg.daily_budget > 0 {
                    format!("{} 次", self.cfg.daily_budget)
                } else {
                    "不限".to_string()
                },
                runstate::list_calls_snapshot().0
            ),
        );
        self.sync_locked(&st);
    }

    /// 调度器退出：进度回到 Off。
    pub fn on_stop(&self) {
        let mut st = self.state.lock().unwrap();
        st.in_batch = false;
        runstate::sweep_set_off();
        applog::info(Stage::Sweep, "定时巡检已随轮询停止".to_string());
    }

    /// 一个批次是否正在编排器里跑。
    pub fn batch_in_progress(&self) -> bool {
        self.state.lock().unwrap().in_batch
    }

    /// 把进程内状态同步到 [`runstate`]（GUI 读）。
    pub fn sync_status(&self) {
        let st = self.state.lock().unwrap();
        self.sync_locked(&st);
    }

    /// 当前进度快照（本实例视角；GUI 读的是同步到 [`runstate`] 的那份）。
    pub fn status(&self) -> SweepStatus {
        let st = self.state.lock().unwrap();
        Self::status_of(&st)
    }

    fn status_of(st: &State) -> SweepStatus {
        let phase = if st.pass_started_at.is_some() {
            SweepPhase::Running
        } else {
            SweepPhase::Waiting
        };
        SweepStatus {
            phase,
            pass_started_at: st.pass_started_at,
            pass_total: st.pass_total,
            pass_done: st.pass_done,
            pass_new_articles: st.pass_new_articles,
            next_pass_at: st.next_pass_at,
            cooldown_until: None,
            cooldown_reason: None,
            last_error: st.last_error.clone(),
            passes_completed: st.passes_completed,
            paused_until: st.paused_until.filter(|t| *t > now()),
            paused_reason: st.paused_reason.clone(),
            list_calls_24h: runstate::list_calls_snapshot().0,
            list_daily_budget: runstate::list_calls_snapshot().1,
            stopping: runstate::sweep_stopping(),
        }
    }

    fn sync_locked(&self, st: &State) {
        runstate::set_sweep_status(Self::status_of(st));
    }

    /// 立即开始新一轮：清停留 / 暂停 / 本轮开始时刻（持久化一并清），尝试次数归零。
    /// 新一轮开始时刻 = 现在，所有此前查过的号都早于它 → 全部重新成为候选。
    fn restart_pass_locked(&self, store: &Store, st: &mut State) {
        st.next_pass_at = None;
        st.next_batch_at = None;
        st.paused_until = None;
        st.paused_reason = None;
        st.pass_started_at = None;
        st.bad_samples.clear();
        st.verify_streak = 0;
        write_epoch(store, KEY_NEXT_PASS_AT, None);
        write_epoch(store, KEY_PASS_STARTED_AT, None);
        let _ = store.reset_sweep_attempts();
        applog::info(
            Stage::Sweep,
            "用户要求立即开始新一轮：跳过停留 / 暂停，全部号重新排队".to_string(),
        );
    }

    /// 立即开始新一轮（进程内直接调用；GUI 走 [`runstate::sweep_request_restart`] 信号）。
    pub fn restart_now(&self, store: &Store) {
        let mut st = self.state.lock().unwrap();
        self.restart_pass_locked(store, &mut st);
        self.sync_locked(&st);
    }

    /// 轮询没在跑时的「立即开始新一轮」：只清持久化状态（下次启动轮询即开新一轮）。
    pub fn reset_persisted(store: &Store) {
        write_epoch(store, KEY_NEXT_PASS_AT, None);
        write_epoch(store, KEY_PASS_STARTED_AT, None);
        let _ = store.reset_sweep_attempts();
    }

    /// 下一批：没到点 / 一轮刚跑完 / 没有号可巡检时返回 `None`。
    ///
    /// 同一次调用里，库里没有可用取样链接的号直接记 `no_sample` 跳过并继续凑下一批，
    /// 直到凑出至少一个号或候选耗尽。
    pub fn next_batch(&self, store: &Store) -> Option<SweepBatch> {
        let mut st = self.state.lock().unwrap();
        let t_now = now();
        // 用户要求立即开始新一轮：清掉停留 / 暂停 / 本轮状态，所有号重新成为候选。
        if runstate::sweep_take_restart() {
            self.restart_pass_locked(store, &mut st);
        }
        // 暂停中（环境故障 / 整批失败 / 预算用完）
        if let Some(until) = st.paused_until {
            if t_now < until {
                self.sync_locked(&st);
                return None;
            }
            st.paused_until = None;
            st.paused_reason = None;
            applog::info(Stage::Sweep, "▶ 巡检暂停结束，继续".to_string());
        }
        // 批次间隔
        if let Some(next) = st.next_batch_at {
            if t_now < next {
                applog::progress(
                    Stage::Sweep,
                    format!("批次间隔… {} 后开始下一批", fmt_secs((next - t_now) as u64)),
                );
                self.sync_locked(&st);
                return None;
            }
            st.next_batch_at = None;
        }
        // 轮间停留
        if let Some(next) = st.next_pass_at {
            if t_now < next {
                self.sync_locked(&st);
                return None;
            }
            st.next_pass_at = None;
            write_epoch(store, KEY_NEXT_PASS_AT, None);
        }
        // 每号预算：当前微信号近 24 小时列表请求已达上限 → 暂停到最早一次请求滑出窗口（手动任务同样不领，
        // 见 `Orchestrator::run_forever`；这里再查一次是为了 `next_batch` 单独使用 / 单测时也成立）。
        if let Some(h) = runstate::list_budget_check(store, self.cfg.daily_budget) {
            let until = h.resume_at;
            let reason = format!(
                "当前微信号近 24 小时列表请求已达预算（{}/{}），巡检暂停到 {} 再继续（手动任务同样暂停领取）",
                h.used,
                h.budget,
                applog::format_local(until)
            );
            st.paused_until = Some(until);
            st.paused_reason = Some(reason.clone());
            if !st.warned_budget {
                applog::warn(Stage::Sweep, format!("⏸ {reason}"));
                crate::notify::notify(crate::notify::Kind::Sweep, reason.clone());
                st.warned_budget = true;
            }
            self.sync_locked(&st);
            return None;
        }
        st.warned_budget = false;
        // 开新一轮
        if st.pass_started_at.is_none() {
            let total = store.sweep_total().unwrap_or(0) as usize;
            if total == 0 {
                if !st.warned_empty {
                    applog::warn(
                        Stage::Sweep,
                        "没有参与巡检的公众号（库里没有号，或都被排除），巡检空转".to_string(),
                    );
                    st.warned_empty = true;
                }
                self.sync_locked(&st);
                return None;
            }
            st.warned_empty = false;
            st.pass_started_at = Some(t_now);
            st.pass_seq = store.bump_sweep_pass_seq().unwrap_or(st.pass_seq + 1);
            st.pass_total = total;
            st.pass_done = 0;
            st.pass_new_articles = 0;
            // 坏样本名单只在一轮内有效：新一轮从最新一篇重新试起。
            st.bad_samples.clear();
            write_epoch(store, KEY_PASS_STARTED_AT, Some(t_now));
            applog::info(
                Stage::Sweep,
                format!(
                    "🔄 巡检第 {} 轮开始：{total} 个号，每批 {} 个",
                    st.passes_completed + 1,
                    self.cfg.batch_size
                ),
            );
        }
        let started = st.pass_started_at.unwrap_or(t_now);

        loop {
            let cands = store
                .sweep_candidates(started, self.cfg.batch_size.max(1) as i64)
                .unwrap_or_default();
            if cands.is_empty() {
                // 一轮完成
                st.passes_completed += 1;
                let used = (now() - started).max(0.0) as u64;
                let next = now() + self.cfg.idle_secs as f64;
                applog::info(
                    Stage::Sweep,
                    format!(
                        "✅ 巡检第 {} 轮完成：{} 个号，新文章 {} 篇，用时 {}；停留 {} 后开始下一轮（{}）",
                        st.passes_completed,
                        st.pass_total,
                        st.pass_new_articles,
                        fmt_secs(used),
                        fmt_secs(self.cfg.idle_secs),
                        applog::format_local(next)
                    ),
                );
                // 一轮完成是正常状态，不推飞书（汇总看 GUI 巡检页 / 日志页）；只有暂停类异常才预警。
                st.pass_started_at = None;
                st.next_pass_at = Some(next);
                write_epoch(store, KEY_PASS_STARTED_AT, None);
                write_epoch(store, KEY_NEXT_PASS_AT, Some(next));
                self.sync_locked(&st);
                return None;
            }

            let index_from = st.pass_done + 1;
            let mut links = Vec::new();
            let mut bizs = Vec::new();
            let mut link_since = HashMap::new();
            let mut sample_by_biz = HashMap::new();
            for a in &cands {
                let bad = st.bad_samples.get(&a.biz);
                let sample = store
                    .latest_article_urls(&a.biz, self.cfg.sample_pool.max(1) as i64)
                    .unwrap_or_default()
                    .into_iter()
                    .find(|u| sample_matches(u, &a.biz) && !bad.is_some_and(|b| b.contains(u)));
                let Some(url) = sample else {
                    let swapped = bad.map(|b| b.len()).unwrap_or(0);
                    let (status, reason) = if swapped > 0 {
                        (
                            "failed",
                            format!(
                                "已换 {swapped} 条文章链接都打不开换凭证，库里没有更多可用长链，本轮跳过"
                            ),
                        )
                    } else {
                        (
                            "no_sample",
                            "库里没有可用的文章长链，无法打开文章换凭证，跳过".to_string(),
                        )
                    };
                    let _ = store.mark_sweep_checked(&a.biz, status, &reason, None);
                    st.pass_done += 1;
                    applog::warn(
                        Stage::Sweep,
                        format!("{} {reason}", store.account_label(&a.biz)),
                    );
                    continue;
                };
                // 翻页依据：上轮记下的 last_published_at；没有则退回库里该号最新一篇的发布时间；
                // 再没有（新号）→ 只采第 1 页。
                let since = a
                    .last_published_at
                    .or_else(|| store.latest_published_for(&a.biz).ok().flatten());
                if let Some(s) = since {
                    link_since.insert(url.clone(), s);
                }
                sample_by_biz.insert(a.biz.clone(), url.clone());
                links.push(url);
                bizs.push(a.biz.clone());
            }
            if links.is_empty() {
                // 这一批全被跳过：继续凑下一批（候选已被标记，不会死循环）。
                continue;
            }
            st.in_batch = true;
            let index_to = st.pass_done + links.len();
            let names: Vec<String> = bizs
                .iter()
                .take(5)
                .map(|b| store.account_label(b))
                .collect();
            applog::info(
                Stage::Sweep,
                format!(
                    "🔄 巡检批次 {index_from}–{index_to}/{}：{}{}",
                    st.pass_total,
                    names.join("、"),
                    if bizs.len() > 5 {
                        format!(" 等 {} 个号", bizs.len())
                    } else {
                        String::new()
                    }
                ),
            );
            self.sync_locked(&st);
            return Some(SweepBatch {
                job: Job {
                    links,
                    local_id: None,
                    kind: JobKind::Sweep,
                    last_updated_at: None,
                    link_since,
                },
                bizs,
                sample_by_biz,
                index_from,
            });
        }
    }

    /// 批次收尾：据编排器的 [`Report`] 把各号结果写回 `accounts`，维护验证页连击与退避等级。
    pub fn finish_batch(&self, store: &Store, batch: &SweepBatch, report: &Report) {
        let mut st = self.state.lock().unwrap();
        st.in_batch = false;
        // 批次间隔：无论结果如何，下一批至少隔 batch_gap_secs（凭证全可复用时一批几十秒就完，
        // 不隔开就是对 getmsg 的连续轰击）。
        if self.cfg.batch_gap_secs > 0 {
            st.next_batch_at = Some(now() + self.cfg.batch_gap_secs as f64);
        }
        // 环境故障：整个接力等待期间一次文章请求都没有——RPA 点不开种子 / 窗口被关 / 代理没流量。
        // 与号无关：不计任何号的失败次数，整个巡检暂停 env_pause_secs 再试，避免一批批空转把全库记成失败。
        if report.env_failure {
            let until = now() + self.cfg.env_pause_secs as f64;
            let reason = report
                .abort_reason
                .clone()
                .unwrap_or_else(|| "接力等待期间没有任何文章请求".to_string());
            st.paused_until = Some(until);
            st.paused_reason = Some(reason.clone());
            st.pass_new_articles += report.new_articles;
            let msg = format!(
                "⛔ 环境故障：{reason}。这批号不计失败，巡检暂停 {} 后重试（{}）。请检查：文件助手是否被其它窗口盖住 / 种子消息是否仍在标定框内（必要时重新「框选种子」）/ 微信是否在线",
                fmt_secs(self.cfg.env_pause_secs),
                applog::format_local(until)
            );
            st.last_error = Some(msg.clone());
            applog::error(Stage::Sweep, msg.clone());
            crate::notify::notify(crate::notify::Kind::Sweep, msg);
            Self::log_batch(
                store,
                batch,
                report,
                st.pass_seq,
                [0, 0, 0, 0],
                Some(reason),
            );
            self.sync_locked(&st);
            return;
        }
        let (mut ok, mut failed, mut retry, mut deferred, mut swapped) =
            (0usize, 0usize, 0usize, 0usize, 0usize);
        for biz in &batch.bizs {
            let sample = batch.sample_by_biz.get(biz).cloned().unwrap_or_default();
            let label = store.account_label(biz);
            if report.finished_bizs.contains(biz) {
                let latest = store.latest_published_for(biz).ok().flatten();
                let _ = store.mark_sweep_checked(biz, "ok", "", latest);
                ok += 1;
                st.pass_done += 1;
                continue;
            }
            // 统一判定「这条取样链接打不开换凭证」：命中验证页 / 被看门狗隔离（白屏、不跳转）/
            // 接力已经打开它但该号没拿到凭证（文章已删除、设为隐私……页面有响应但没有凭证请求）。
            // 三种都换下一篇重新接力，最多换 max_swaps 次。
            let hit_verify = report.verify_url.as_deref() == Some(sample.as_str());
            let isolated = report.isolated_links.contains(&sample);
            let opened_no_cred = !report.unopened_links.contains(&sample)
                && !report.captured_bizs.contains(biz)
                && !hit_verify
                && !isolated;
            let link_bad = hit_verify || isolated || opened_no_cred;
            let never_opened = report.unopened_links.contains(&sample) && !link_bad;
            if never_opened || (report.rate_limited && !link_bad) {
                // 这批没轮到它（前面的链接把时间耗完 / 中止）或被限流：都不算该号失败，
                // 不动尝试次数、不换链接，下批 / 退避结束后原样再来。
                deferred += 1;
                continue;
            }
            let reason = if hit_verify {
                "打开该篇文章命中验证页".to_string()
            } else if isolated {
                "打开该篇文章后页面无响应 / 不跳转（白屏）".to_string()
            } else if opened_no_cred {
                "打开该篇文章后没抓到凭证（疑似已删除 / 设为隐私）".to_string()
            } else {
                report
                    .abort_reason
                    .clone()
                    .unwrap_or_else(|| "凭证已到手但采集未完成（续期失败 / 采集异常）".to_string())
            };
            let n = store.bump_sweep_attempt(biz, &reason).unwrap_or(i64::MAX);
            if n > self.cfg.max_swaps {
                let reason = if link_bad {
                    format!(
                        "已换 {} 条文章链接都打不开换凭证，本轮跳过：{reason}",
                        self.cfg.max_swaps
                    )
                } else {
                    format!("本轮 {n} 次均失败，等下一轮：{reason}")
                };
                let _ = store.mark_sweep_checked(biz, "failed", &reason, None);
                failed += 1;
                st.pass_done += 1;
                applog::warn(Stage::Sweep, format!("{label} {reason}"));
            } else if link_bad {
                st.bad_samples
                    .entry(biz.clone())
                    .or_default()
                    .insert(sample.clone());
                swapped += 1;
                retry += 1;
                applog::warn(
                    Stage::Sweep,
                    format!(
                        "{label} {reason}，换下一篇重新接力（第 {n}/{} 次换链接）",
                        self.cfg.max_swaps
                    ),
                );
            } else {
                retry += 1;
            }
        }
        st.pass_new_articles += report.new_articles;

        // 验证页连击：连续 verify_streak_limit 批都命中才当真限流。
        if report.verify_url.is_some() {
            st.verify_streak += 1;
            if st.verify_streak >= self.cfg.verify_streak_limit.max(1) {
                let secs = runstate::cooldown_trigger(format!(
                    "连续 {} 批接力命中人机验证页",
                    st.verify_streak
                ));
                applog::error(
                    Stage::Sweep,
                    format!(
                        "⛔ 连续 {} 批接力命中人机验证页，判为整机限流，退避 {} 分钟",
                        st.verify_streak,
                        secs / 60
                    ),
                );
                st.verify_streak = 0;
            }
        } else if !report.rate_limited {
            st.verify_streak = 0;
        }
        // 干净批次：退避等级归零。
        if !report.rate_limited && report.verify_url.is_none() && report.abort_reason.is_none() {
            runstate::cooldown_note_clean();
        }
        // 整批失败（一个号都没成功、有号要重试，且不是限流 / 验证页那条路）：多半是环境或微信侧
        // 出了状况，立刻重试只会把每号的尝试次数在几分钟内打光。整个巡检先停 fail_pause_secs。
        if ok == 0 && retry > 0 && !report.rate_limited && self.cfg.fail_pause_secs > 0 {
            let until = now() + self.cfg.fail_pause_secs as f64;
            let reason = format!(
                "本批 {} 个号无一成功（{retry} 个待重试），巡检暂停 {} 再继续（{}）",
                batch.bizs.len(),
                fmt_secs(self.cfg.fail_pause_secs),
                applog::format_local(until)
            );
            st.paused_until = Some(until);
            st.paused_reason = Some(reason.clone());
            applog::warn(Stage::Sweep, format!("⏸ {reason}"));
            crate::notify::notify(crate::notify::Kind::Sweep, format!("整批失败：{reason}"));
        }

        let summary = format!(
            "巡检批次结果：成功 {ok}，失败 {failed}，本轮稍后重试 {retry}{}{}；进度 {}/{}，本轮新文章 {} 篇",
            if swapped > 0 {
                format!("（其中换链接重接力 {swapped}）")
            } else {
                String::new()
            },
            if deferred > 0 && report.rate_limited {
                format!("，限流退避后重试 {deferred}")
            } else if deferred > 0 {
                format!("，本批未轮到、下批照旧 {deferred}")
            } else {
                String::new()
            },
            st.pass_done,
            st.pass_total,
            st.pass_new_articles
        );
        st.last_error = if failed + retry + deferred > 0 {
            Some(summary.clone())
        } else {
            None
        };
        if failed + retry + deferred > 0 {
            applog::warn(Stage::Sweep, summary.clone());
        } else {
            applog::info(Stage::Sweep, summary.clone());
        }
        Self::log_batch(
            store,
            batch,
            report,
            st.pass_seq,
            [ok, failed, retry, deferred],
            report.abort_reason.clone().or(Some(summary)),
        );
        self.sync_locked(&st);
    }

    /// 巡检批次的执行摘要写进 `run_log`（限流分析「轮次 / 批次」）。`counts` = [成功, 失败, 重试, 未轮到]。
    fn log_batch(
        store: &Store,
        batch: &SweepBatch,
        report: &Report,
        pass_no: i64,
        counts: [usize; 4],
        note: Option<String>,
    ) {
        let list_calls = store
            .count_list_calls_for_job(report.job_id, None)
            .unwrap_or(0);
        let blocked = store
            .count_list_calls_for_job(report.job_id, Some("blocked"))
            .unwrap_or(0)
            > 0;
        let _ = store.append_run_log(&crate::model::RunLogEvent {
            job_id: Some(report.job_id),
            kind: "sweep".to_string(),
            pass_no: Some(pass_no),
            started_at: if report.started_at > 0.0 {
                report.started_at
            } else {
                now()
            },
            finished_at: now(),
            accounts: batch.bizs.len() as i64,
            ok: counts[0] as i64,
            failed: counts[1] as i64,
            retry: counts[2] as i64,
            deferred: counts[3] as i64,
            new_articles: report.new_articles as i64,
            urls: report.urls.len() as i64,
            list_calls,
            rate_limited: report.rate_limited,
            blocked,
            verify_hit: report.verify_url.is_some(),
            env_failure: report.env_failure,
            truncated: report.truncated,
            note,
        });
    }
}

fn read_epoch(store: &Store, key: &str) -> Option<Epoch> {
    store
        .get_config(key)
        .ok()
        .flatten()
        .and_then(|v| v.as_f64())
}

fn write_epoch(store: &Store, key: &str, value: Option<Epoch>) {
    let v = value.map(Value::from).unwrap_or(Value::Null);
    let _ = store.set_config(key, &v);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ArticleFields;

    fn seed_account(store: &Store, biz: &str, arts: &[(&str, i64, Option<f64>)]) {
        store.upsert_account(biz, Some(biz), None).unwrap();
        for (mid, idx, pub_at) in arts {
            let fields = ArticleFields {
                content_url: Some(format!(
                    "http://mp.weixin.qq.com/s?__biz={biz}&mid={mid}&idx={idx}&sn=abc{mid}"
                )),
                published_at: *pub_at,
                ..Default::default()
            };
            store.upsert_article(biz, mid, *idx, &fields).unwrap();
        }
    }

    fn cfg() -> SweepConfig {
        SweepConfig {
            idle_secs: 3600,
            batch_size: 2,
            sample_pool: 3,
            max_swaps: 1,
            verify_streak_limit: 2,
            env_pause_secs: 600,
            // 单测默认不设批次间隔 / 整批失败暂停 / 预算，专门的用例再打开。
            batch_gap_secs: 0,
            fail_pause_secs: 0,
            daily_budget: 0,
        }
    }

    fn ok_report(bizs: &[&str], new_articles: usize) -> Report {
        Report {
            finished_bizs: bizs.iter().map(|s| s.to_string()).collect(),
            captured_bizs: bizs.iter().map(|s| s.to_string()).collect(),
            new_articles,
            ..Default::default()
        }
    }

    /// 接力打开了取样链接、但该号没拿到凭证（文章已删除 / 隐私 / 白屏）。
    fn opened_no_cred_report(links: &[String]) -> Report {
        Report {
            truncated: true,
            remaining_links: links.to_vec(),
            ..Default::default()
        }
    }

    fn attempts(store: &Store, biz: &str) -> i64 {
        store.get_account(biz).unwrap().unwrap().sweep_attempts
    }

    #[test]
    fn batch_uses_latest_article_and_since() {
        let store = Store::open_in_memory().unwrap();
        seed_account(
            &store,
            "A==",
            &[
                ("m1", 1, Some(1000.0)),
                ("m2", 1, Some(3000.0)),
                ("m3", 1, Some(2000.0)),
            ],
        );
        seed_account(&store, "B==", &[("m9", 1, None)]);
        // C 没有任何文章 → no_sample 跳过
        store.upsert_account("C==", Some("C"), None).unwrap();
        let sw = Sweep::new(SweepConfig {
            batch_size: 10,
            ..cfg()
        });
        sw.on_start(&store);
        let b = sw.next_batch(&store).expect("应有一批");
        assert_eq!(b.bizs, vec!["A==".to_string(), "B==".to_string()]);
        // A 取最新发布（m2），since = MAX(published_at) = 3000
        assert!(b.sample_by_biz["A=="].contains("mid=m2"));
        assert_eq!(b.job.since_for_link(&b.sample_by_biz["A=="]), Some(3000.0));
        // B 只有无发布时间的文章：取样仍有，since 为 None（只采首页）
        assert!(b.sample_by_biz["B=="].contains("mid=m9"));
        assert_eq!(b.job.since_for_link(&b.sample_by_biz["B=="]), None);
        assert_eq!(b.job.kind, crate::model::JobKind::Sweep);
        // C 已被记为 no_sample
        let c = store.get_account("C==").unwrap().unwrap();
        assert_eq!(c.sweep_status.as_deref(), Some("no_sample"));
        assert!(c.sweep_checked_at.is_some());
        assert!(sw.batch_in_progress());
    }

    #[test]
    fn finish_ok_records_last_published_and_pass_completes() {
        let store = Store::open_in_memory().unwrap();
        seed_account(&store, "A==", &[("m1", 1, Some(1000.0))]);
        seed_account(&store, "B==", &[("m1", 1, Some(2000.0))]);
        let sw = Sweep::new(SweepConfig {
            idle_secs: 3600,
            ..cfg()
        });
        sw.on_start(&store);
        let b = sw.next_batch(&store).unwrap();
        // 模拟采到新文章后成功
        let f = ArticleFields {
            published_at: Some(5000.0),
            content_url: Some("http://mp.weixin.qq.com/s?__biz=A==&mid=m7&idx=1&sn=x".into()),
            ..Default::default()
        };
        store.upsert_article("A==", "m7", 1, &f).unwrap();
        sw.finish_batch(&store, &b, &ok_report(&["A==", "B=="], 1));
        let a = store.get_account("A==").unwrap().unwrap();
        assert_eq!(a.sweep_status.as_deref(), Some("ok"));
        assert_eq!(a.last_published_at, Some(5000.0));
        assert_eq!(a.sweep_attempts, 0);
        assert!(!sw.batch_in_progress());
        // 候选耗尽 → 一轮完成，进入停留；再要批次返回 None
        assert!(sw.next_batch(&store).is_none());
        let status = sw.status();
        assert_eq!(status.phase, SweepPhase::Waiting);
        assert!(status.next_pass_at.is_some_and(|t| t > now() + 3000.0));
        assert_eq!(status.passes_completed, 1);
        assert_eq!(status.pass_new_articles, 1);
        assert!(sw.next_batch(&store).is_none());
        // 持久化：下一轮时刻已写入 config
        assert!(read_epoch(&store, KEY_NEXT_PASS_AT).is_some());
        assert!(read_epoch(&store, KEY_PASS_STARTED_AT).is_none());
    }

    #[test]
    fn bad_link_swaps_sample_then_marks_failed() {
        let store = Store::open_in_memory().unwrap();
        seed_account(
            &store,
            "A==",
            &[
                ("m1", 1, Some(1000.0)),
                ("m2", 1, Some(2000.0)),
                ("m3", 1, Some(3000.0)),
            ],
        );
        let sw = Sweep::new(cfg()); // max_swaps = 1
        sw.on_start(&store);
        let b = sw.next_batch(&store).unwrap();
        assert!(b.sample_by_biz["A=="].contains("mid=m3"));
        // 打开了最新一篇但没抓到凭证 → 记坏样本、换次新一篇重接力，仍是本轮候选
        sw.finish_batch(&store, &b, &opened_no_cred_report(&b.job.links));
        let a = store.get_account("A==").unwrap().unwrap();
        assert_eq!(a.sweep_attempts, 1);
        assert!(a.sweep_checked_at.is_none(), "换链接后仍是本轮候选");
        assert!(a.sweep_error.as_deref().unwrap().contains("没抓到凭证"));
        let b2 = sw.next_batch(&store).unwrap();
        assert_eq!(b2.bizs, vec!["A==".to_string()]);
        assert!(b2.sample_by_biz["A=="].contains("mid=m2"), "应换成次新一篇");
        // 换过 1 次后再失败 → 超过 max_swaps，记 failed 跳过
        sw.finish_batch(&store, &b2, &opened_no_cred_report(&b2.job.links));
        let a = store.get_account("A==").unwrap().unwrap();
        assert_eq!(a.sweep_status.as_deref(), Some("failed"));
        assert!(a.sweep_checked_at.is_some());
        assert_eq!(a.sweep_attempts, 0);
        assert!(a.sweep_error.as_deref().unwrap().contains("已换 1 条"));
        // 一轮完成
        assert!(sw.next_batch(&store).is_none());
        assert_eq!(sw.status().phase, SweepPhase::Waiting);
    }

    #[test]
    fn isolated_link_counts_as_swap_and_pool_exhaustion_marks_failed() {
        let store = Store::open_in_memory().unwrap();
        seed_account(
            &store,
            "A==",
            &[("m1", 1, Some(1000.0)), ("m2", 1, Some(2000.0))],
        );
        let sw = Sweep::new(SweepConfig {
            max_swaps: 5,
            ..cfg()
        });
        sw.on_start(&store);
        let b = sw.next_batch(&store).unwrap();
        // 白屏被看门狗隔离：换链接
        let rep = Report {
            truncated: true,
            remaining_links: b.job.links.clone(),
            isolated_links: b.job.links.clone(),
            abort_reason: Some("接力再次停滞，隔离该链接".into()),
            ..Default::default()
        };
        sw.finish_batch(&store, &b, &rep);
        assert_eq!(attempts(&store, "A=="), 1);
        let b2 = sw.next_batch(&store).unwrap();
        assert!(b2.sample_by_biz["A=="].contains("mid=m1"));
        sw.finish_batch(&store, &b2, &opened_no_cred_report(&b2.job.links));
        assert_eq!(attempts(&store, "A=="), 2);
        // 两篇都坏了、库里没有更多长链 → next_batch 直接记 failed（不是 no_sample）
        assert!(sw.next_batch(&store).is_none());
        let a = store.get_account("A==").unwrap().unwrap();
        assert_eq!(a.sweep_status.as_deref(), Some("failed"));
        assert!(a.sweep_error.as_deref().unwrap().contains("已换 2 条"));
    }

    #[test]
    fn unopened_link_is_not_counted_and_keeps_sample() {
        let store = Store::open_in_memory().unwrap();
        seed_account(
            &store,
            "A==",
            &[("m1", 1, Some(1000.0)), ("m2", 1, Some(2000.0))],
        );
        seed_account(&store, "B==", &[("m1", 1, Some(1000.0))]);
        let sw = Sweep::new(cfg());
        sw.on_start(&store);
        let b = sw.next_batch(&store).unwrap();
        assert_eq!(b.bizs.len(), 2);
        let a_link = b.sample_by_biz["A=="].clone();
        let b_link = b.sample_by_biz["B=="].clone();
        // A 打开了没拿到凭证；B 根本没轮到（等待到上限）
        let rep = Report {
            truncated: true,
            remaining_links: b.job.links.clone(),
            unopened_links: vec![b_link.clone()],
            ..Default::default()
        };
        sw.finish_batch(&store, &b, &rep);
        assert_eq!(attempts(&store, "A=="), 1);
        assert_eq!(attempts(&store, "B=="), 0, "没轮到的号不计失败");
        // 下一批：B 没动过排最前、取样不变；A 换了链接
        let b2 = sw.next_batch(&store).unwrap();
        assert_eq!(b2.bizs, vec!["B==".to_string(), "A==".to_string()]);
        assert_eq!(b2.sample_by_biz["B=="], b_link);
        assert_ne!(b2.sample_by_biz["A=="], a_link);
    }

    #[test]
    fn captured_but_collect_failed_counts_without_swap() {
        let store = Store::open_in_memory().unwrap();
        seed_account(
            &store,
            "A==",
            &[("m1", 1, Some(1000.0)), ("m2", 1, Some(2000.0))],
        );
        let sw = Sweep::new(cfg());
        sw.on_start(&store);
        let b = sw.next_batch(&store).unwrap();
        let link = b.sample_by_biz["A=="].clone();
        // 凭证拿到了（captured）但没采完：计一次失败，但链接没问题、不换
        let rep = Report {
            truncated: true,
            remaining_links: b.job.links.clone(),
            captured_bizs: vec!["A==".into()],
            ..Default::default()
        };
        sw.finish_batch(&store, &b, &rep);
        let a = store.get_account("A==").unwrap().unwrap();
        assert_eq!(a.sweep_attempts, 1);
        assert!(a.sweep_error.as_deref().unwrap().contains("采集未完成"));
        let b2 = sw.next_batch(&store).unwrap();
        assert_eq!(b2.sample_by_biz["A=="], link, "凭证到手过的链接不换");
    }

    #[test]
    fn verify_page_switches_sample_and_rate_limit_defers() {
        let store = Store::open_in_memory().unwrap();
        seed_account(
            &store,
            "A==",
            &[("m1", 1, Some(1000.0)), ("m2", 1, Some(2000.0))],
        );
        let sw = Sweep::new(cfg());
        sw.on_start(&store);
        let b = sw.next_batch(&store).unwrap();
        let first = b.sample_by_biz["A=="].clone();
        assert!(first.contains("mid=m2"));
        // 命中验证页：该样本进坏名单，下一批换次新
        let rep = Report {
            truncated: true,
            remaining_links: vec![first.clone()],
            abort_reason: Some(format!("命中人机验证页（{first}）")),
            verify_url: Some(first.clone()),
            ..Default::default()
        };
        sw.finish_batch(&store, &b, &rep);
        let b2 = sw.next_batch(&store).unwrap();
        assert!(b2.sample_by_biz["A=="].contains("mid=m1"));
        // 限流（凭证已到手、采集时撞上）：不计尝试次数（仍为 1），号保持候选
        let before = store.get_account("A==").unwrap().unwrap().sweep_attempts;
        assert_eq!(before, 1);
        let rep2 = Report {
            truncated: true,
            remaining_links: b2.job.links.clone(),
            captured_bizs: vec!["A==".into()],
            rate_limited: true,
            ..Default::default()
        };
        sw.finish_batch(&store, &b2, &rep2);
        let a = store.get_account("A==").unwrap().unwrap();
        assert_eq!(a.sweep_attempts, before);
        assert!(a.sweep_checked_at.is_none());
    }

    #[test]
    fn env_failure_pauses_without_counting_attempts() {
        let store = Store::open_in_memory().unwrap();
        seed_account(&store, "A==", &[("m1", 1, Some(1000.0))]);
        let sw = Sweep::new(SweepConfig {
            env_pause_secs: 600,
            ..cfg()
        });
        sw.on_start(&store);
        let b = sw.next_batch(&store).unwrap();
        let rep = Report {
            truncated: true,
            remaining_links: b.job.links.clone(),
            abort_reason: Some("点种子后无任何文章请求（疑似点空）；重拉 3 次仍无进展".into()),
            env_failure: true,
            ..Default::default()
        };
        sw.finish_batch(&store, &b, &rep);
        // 号不计失败
        let a = store.get_account("A==").unwrap().unwrap();
        assert_eq!(a.sweep_attempts, 0);
        assert!(a.sweep_checked_at.is_none());
        // 巡检暂停：不再出批次；状态带暂停截止与原因
        assert!(sw.next_batch(&store).is_none());
        let st = sw.status();
        assert!(st.paused_until.is_some_and(|t| t > now() + 500.0));
        assert!(st.paused_reason.as_deref().unwrap().contains("点空"));
        assert!(st.last_error.as_deref().unwrap().contains("环境故障"));
        // 暂停为 0 秒的配置：立刻恢复
        let sw2 = Sweep::new(SweepConfig {
            env_pause_secs: 0,
            ..cfg()
        });
        sw2.on_start(&store);
        let b2 = sw2.next_batch(&store).unwrap();
        sw2.finish_batch(&store, &b2, &rep);
        assert!(sw2.next_batch(&store).is_some());
    }

    #[test]
    fn restart_now_skips_idle_and_requeues_failed() {
        let store = Store::open_in_memory().unwrap();
        seed_account(&store, "A==", &[("m1", 1, Some(1000.0))]);
        let sw = Sweep::new(SweepConfig {
            max_swaps: 0,
            ..cfg()
        });
        sw.on_start(&store);
        let b = sw.next_batch(&store).unwrap();
        // 一次失败即记 failed（max_swaps=0）→ 一轮完成进入停留
        let rep = Report {
            truncated: true,
            remaining_links: b.job.links.clone(),
            abort_reason: Some("x".into()),
            ..Default::default()
        };
        sw.finish_batch(&store, &b, &rep);
        assert!(sw.next_batch(&store).is_none());
        assert_eq!(sw.status().phase, SweepPhase::Waiting);
        assert_eq!(
            store
                .get_account("A==")
                .unwrap()
                .unwrap()
                .sweep_status
                .as_deref(),
            Some("failed")
        );
        // 立即开始新一轮：跳过停留，failed 的号重新排队
        sw.restart_now(&store);
        assert!(read_epoch(&store, KEY_NEXT_PASS_AT).is_none());
        let b2 = sw.next_batch(&store).expect("新一轮应立刻出批次");
        assert_eq!(b2.bizs, vec!["A==".to_string()]);
        assert_eq!(sw.status().phase, SweepPhase::Running);
    }

    #[test]
    fn excluded_account_not_swept_and_empty_db_idles() {
        let store = Store::open_in_memory().unwrap();
        seed_account(&store, "A==", &[("m1", 1, Some(1000.0))]);
        store.set_sweep_enabled("A==", false).unwrap();
        let sw = Sweep::new(cfg());
        sw.on_start(&store);
        assert!(sw.next_batch(&store).is_none());
        assert_eq!(store.sweep_total().unwrap(), 0);
        store.set_sweep_enabled("A==", true).unwrap();
        assert!(sw.next_batch(&store).is_some());
    }

    #[test]
    fn resumes_pass_from_persisted_state() {
        let store = Store::open_in_memory().unwrap();
        seed_account(&store, "A==", &[("m1", 1, Some(1000.0))]);
        seed_account(&store, "B==", &[("m1", 1, Some(1000.0))]);
        let sw = Sweep::new(SweepConfig {
            batch_size: 1,
            ..cfg()
        });
        sw.on_start(&store);
        let b = sw.next_batch(&store).unwrap();
        sw.finish_batch(&store, &b, &ok_report(&[b.bizs[0].as_str()], 0));
        // 「重启」：新实例从 config 恢复本轮，只剩另一个号
        let sw2 = Sweep::new(SweepConfig {
            batch_size: 1,
            ..cfg()
        });
        sw2.on_start(&store);
        let st = sw2.status();
        assert_eq!(st.phase, SweepPhase::Running);
        assert_eq!((st.pass_done, st.pass_total), (1, 2));
        let b2 = sw2.next_batch(&store).unwrap();
        assert_ne!(b2.bizs[0], b.bizs[0]);
    }

    /// 整批失败（无一成功、有号待重试）→ 巡检暂停 fail_pause_secs，不再立刻重试同一批号。
    #[test]
    fn whole_batch_failure_pauses_sweep() {
        let store = Store::open_in_memory().unwrap();
        seed_account(&store, "A==", &[("m1", 1, Some(1000.0))]);
        seed_account(&store, "B==", &[("m1", 1, Some(1000.0))]);
        let sw = Sweep::new(SweepConfig {
            fail_pause_secs: 900,
            max_swaps: 5,
            ..cfg()
        });
        sw.on_start(&store);
        let b = sw.next_batch(&store).unwrap();
        // 凭证都到手但采集异常（例如接口报错）：两个号都进「稍后重试」
        let rep = Report {
            truncated: true,
            remaining_links: b.job.links.clone(),
            captured_bizs: b.bizs.clone(),
            ..Default::default()
        };
        sw.finish_batch(&store, &b, &rep);
        let st = sw.status();
        assert!(st.paused_until.is_some_and(|t| t > now() + 800.0));
        assert!(st.paused_reason.as_deref().unwrap().contains("无一成功"));
        assert!(sw.next_batch(&store).is_none(), "暂停期间不出批次");
        assert_eq!(attempts(&store, "A=="), 1, "失败次数照计");

        // 有一个号成功就不暂停
        let sw2 = Sweep::new(SweepConfig {
            fail_pause_secs: 900,
            ..cfg()
        });
        sw2.restart_now(&store);
        let b2 = sw2.next_batch(&store).unwrap();
        let rep2 = Report {
            truncated: true,
            remaining_links: vec![b2.job.links[1].clone()],
            captured_bizs: b2.bizs.clone(),
            finished_bizs: vec![b2.bizs[0].clone()],
            ..Default::default()
        };
        sw2.finish_batch(&store, &b2, &rep2);
        assert!(sw2.status().paused_until.is_none());
    }

    /// 批次间隔：finish_batch 之后 batch_gap_secs 内不出下一批。
    #[test]
    fn batch_gap_delays_next_batch() {
        let store = Store::open_in_memory().unwrap();
        seed_account(&store, "A==", &[("m1", 1, Some(1000.0))]);
        seed_account(&store, "B==", &[("m1", 1, Some(1000.0))]);
        let sw = Sweep::new(SweepConfig {
            batch_size: 1,
            batch_gap_secs: 600,
            ..cfg()
        });
        sw.on_start(&store);
        let b = sw.next_batch(&store).unwrap();
        sw.finish_batch(&store, &b, &ok_report(&[b.bizs[0].as_str()], 0));
        assert!(sw.next_batch(&store).is_none(), "间隔内不出下一批");
        // 「立即开始新一轮」清掉间隔
        sw.restart_now(&store);
        assert!(sw.next_batch(&store).is_some());
    }

    /// 每号预算：当前号近 24 小时 getmsg 留档达到预算 → 巡检暂停到最早一次滑出窗口；预算 0 = 不限。
    #[test]
    fn daily_budget_pauses_until_window_frees() {
        let store = Store::open_in_memory().unwrap();
        seed_account(&store, "A==", &[("m1", 1, Some(1000.0))]);
        let ev = crate::model::ListCallEvent {
            biz: "A==".into(),
            source: "sweep".into(),
            job_id: None,
            page: 1,
            outcome: "ok".into(),
            latency_ms: 0,
            articles: 0,
            new_articles: 0,
            note: None,
            factors: Default::default(),
        };
        // 3 次留档：最早一次在 1 小时前，其余刚刚。
        store.append_list_call_at(now() - 3600.0, &ev).unwrap();
        store.append_list_call(&ev).unwrap();
        store.append_list_call(&ev).unwrap();
        let sw = Sweep::new(SweepConfig {
            daily_budget: 3,
            ..cfg()
        });
        sw.on_start(&store);
        assert!(sw.next_batch(&store).is_none());
        let st = sw.status();
        assert!(st.paused_reason.as_deref().unwrap().contains("已达预算"));
        // 最早一次是 1 小时前 → 约 23 小时后腾出额度。
        let until = st.paused_until.unwrap();
        assert!(until > now() + 22.0 * 3600.0 && until < now() + 24.0 * 3600.0);
        // 进程级预算镜像会被并行测试改写，只验暂停文案里的「已用/预算」。
        assert!(st.paused_reason.as_deref().unwrap().contains("（3/3）"));

        let sw2 = Sweep::new(SweepConfig {
            daily_budget: 0,
            ..cfg()
        });
        sw2.restart_now(&store);
        assert!(sw2.next_batch(&store).is_some(), "预算 0 = 不限");
    }
}
