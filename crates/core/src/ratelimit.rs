//! 限流分析 —— 把 `list_call_log`（每次 `getmsg` 请求）/ `run_log`（每个执行单元）/ `cooldown_log`
//! （每次整机退避）聚合成 GUI「限流分析」页要的统计：接口请求频率（按时间桶）、请求间隔分布
//! （闸门是否真的生效）、按来源 / 结果的计数、巡检轮次与批次的执行摘要、退避记录。
//!
//! 只做聚合，不碰网络；原始行由 `collector` / `orchestrator` / `sweep` / `runstate` 写入（Rust 版新增，
//! Python 无对应）。窗口内的原始行全部读进内存再算（每日预算 600 次量级，30 天也只有几万行）。

use serde::Serialize;

use crate::model::{now, CooldownLogRow, Epoch, ListCallRow, RunLogRow};
use crate::runstate::{self, CooldownSnapshot, SweepPhase};
use crate::store::Store;

/// 窗口内最多读多少条列表请求（安全阀；600/天 × 30 天 = 18000）。
const MAX_CALL_ROWS: i64 = 50_000;
/// 返回给前端的最近请求间隔样本数上限。
const MAX_GAP_SAMPLES: usize = 2_000;

/// 按结果分的请求计数。
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct OutcomeCounts {
    pub calls: i64,
    pub ok: i64,
    pub expired: i64,
    pub blocked: i64,
    pub rate_limited: i64,
    pub error: i64,
    /// `ok` 请求解析出的篇数 / 新入库篇数之和。
    pub articles: i64,
    pub new_articles: i64,
}

impl OutcomeCounts {
    fn add(&mut self, r: &ListCallRow) {
        self.calls += 1;
        match r.outcome.as_str() {
            "ok" => {
                self.ok += 1;
                self.articles += r.articles;
                self.new_articles += r.new_articles;
            }
            "expired" => self.expired += 1,
            "blocked" => self.blocked += 1,
            "rate_limited" => self.rate_limited += 1,
            _ => self.error += 1,
        }
    }
}

/// 按来源（巡检 / 手动重试 / 历史抓取；老库里还有 manual）的计数。
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SourceCounts {
    pub source: String,
    #[serde(flatten)]
    pub counts: OutcomeCounts,
}

/// 按某个影响因素（微信号）分组的计数 + 首末使用时刻 + 平均耗时。
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct FactorCounts {
    /// 分组键：uin 短哈希 / `未知`。
    pub key: String,
    #[serde(flatten)]
    pub counts: OutcomeCounts,
    /// 该组第一次 / 最后一次请求时刻。
    pub first_ts: Epoch,
    pub last_ts: Epoch,
    /// 该组请求往返耗时均值（毫秒）。
    pub avg_latency_ms: i64,
    /// 累计耗时（求均值用，不序列化）。
    #[serde(skip)]
    latency_sum: i64,
}

impl FactorCounts {
    fn bump(groups: &mut Vec<FactorCounts>, key: &str, r: &ListCallRow) {
        let g = match groups.iter_mut().position(|g| g.key == key) {
            Some(i) => &mut groups[i],
            None => {
                groups.push(FactorCounts {
                    key: key.to_string(),
                    first_ts: r.ts,
                    last_ts: r.ts,
                    ..Default::default()
                });
                groups.last_mut().expect("刚 push 过")
            }
        };
        g.counts.add(r);
        g.first_ts = g.first_ts.min(r.ts);
        g.last_ts = g.last_ts.max(r.ts);
        g.latency_sum += r.latency_ms.max(0);
    }

    fn finish(&mut self) {
        if self.counts.calls > 0 {
            self.avg_latency_ms = self.latency_sum / self.counts.calls;
        }
    }
}

/// 一个时间桶（起点 epoch 秒 + 桶内计数）。
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Bucket {
    pub ts: Epoch,
    #[serde(flatten)]
    pub counts: OutcomeCounts,
}

/// 请求间隔直方图的一格。
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct GapBin {
    pub label: String,
    /// 本格上限（毫秒，不含）；最后一格为 `i64::MAX`。
    pub upto_ms: i64,
    pub count: i64,
}

/// 相邻两次 `getmsg` 之间的间隔统计（不分来源——微信按微信号计频，看的就是整机节奏）。
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct GapStats {
    pub samples: i64,
    pub min_ms: i64,
    pub p50_ms: i64,
    pub avg_ms: i64,
    pub max_ms: i64,
    pub histogram: Vec<GapBin>,
    /// 最近的间隔样本（毫秒，时间升序，最多 2000 个）：前端据配置的闸门下限数「过密」次数。
    pub recent_ms: Vec<i64>,
}

/// 请求往返耗时统计。
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LatencyStats {
    pub samples: i64,
    pub avg_ms: i64,
    pub p50_ms: i64,
    pub max_ms: i64,
}

/// 一轮巡检的汇总（由该轮各批次的 `run_log` 行合成）。
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct PassSummary {
    pub pass_no: i64,
    pub started_at: Epoch,
    pub finished_at: Epoch,
    /// 该轮仍在进行（轮次号最大且巡检处于 running）。
    pub in_progress: bool,
    pub batches: i64,
    pub accounts: i64,
    pub ok: i64,
    pub failed: i64,
    pub retry: i64,
    pub deferred: i64,
    pub new_articles: i64,
    pub list_calls: i64,
    pub rate_limited_batches: i64,
    pub blocked_batches: i64,
    pub verify_batches: i64,
    pub env_failures: i64,
}

/// 「限流分析」页的整份统计。
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct RateLimitStats {
    pub window_secs: i64,
    pub generated_at: Epoch,
    pub bucket_secs: i64,
    pub totals: OutcomeCounts,
    pub by_source: Vec<SourceCounts>,
    /// 按微信号分组（`key` = uin 短哈希 / `未知`；最近使用的排前）——「限制是否跟账号走」的对照。
    pub by_account: Vec<FactorCounts>,
    /// 时间桶（升序，含空桶，覆盖整个窗口）。
    pub buckets: Vec<Bucket>,
    /// 单桶请求数峰值。
    pub peak_bucket_calls: i64,
    pub gaps: GapStats,
    pub latency: LatencyStats,
    /// 巡检轮次（轮次号降序）。
    pub passes: Vec<PassSummary>,
    /// 最近的执行单元（开始时间降序，最多 80 条）。
    pub runs: Vec<RunLogRow>,
    /// 窗口内执行单元数：老版本手动任务（留档）/ 巡检批次。
    pub runs_manual: i64,
    pub runs_sweep: i64,
    /// 退避记录（时间降序，最多 50 条）。
    pub cooldowns: Vec<CooldownLogRow>,
    /// 窗口内 IP 级限流 / 账号级封禁触发次数，以及两者退避时长之和（秒）。
    pub cooldown_hits: i64,
    pub block_hits: i64,
    pub cooldown_total_secs: i64,
    /// 当前退避状态（进程内）。
    pub cooldown_now: CooldownSnapshot,
    /// 今日（本地日历日）已发的列表请求数（`config` 表计数，仅展示）。
    pub today_calls: i64,
    /// 当前微信号近 24 小时已发的列表请求数（`list_call_log` 按 uin 短哈希过滤；预算的计数口径）与每号预算
    /// （0 = 不限 / 未启动轮询）。
    pub uin_calls_24h: i64,
    pub daily_budget: i64,
}

/// 按窗口长度选时间桶：≤ 6 小时用 10 分钟，≤ 48 小时用 1 小时，更长用 6 小时。
pub fn bucket_secs_for(window_secs: i64) -> i64 {
    if window_secs <= 6 * 3600 {
        600
    } else if window_secs <= 48 * 3600 {
        3600
    } else {
        6 * 3600
    }
}

/// 本地时区相对 UTC 的偏移（秒）：时间桶按本地整点 / 整日对齐用。
fn local_offset_secs() -> i64 {
    use chrono::Local;
    Local::now().offset().local_minus_utc() as i64
}

fn percentile_sorted(sorted: &[i64], p: f64) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// 请求间隔直方图的分格（毫秒上限，不含）与标签。
const GAP_BINS: &[(i64, &str)] = &[
    (3_000, "<3s"),
    (8_000, "3–8s"),
    (12_000, "8–12s"),
    (20_000, "12–20s"),
    (60_000, "20–60s"),
    (300_000, "1–5 分"),
    (i64::MAX, ">5 分"),
];

fn gap_stats(rows: &[ListCallRow]) -> GapStats {
    let mut gaps: Vec<i64> = Vec::with_capacity(rows.len().saturating_sub(1));
    for w in rows.windows(2) {
        let ms = ((w[1].ts - w[0].ts) * 1000.0).round() as i64;
        gaps.push(ms.max(0));
    }
    let mut histogram: Vec<GapBin> = GAP_BINS
        .iter()
        .map(|(upto, label)| GapBin {
            label: (*label).to_string(),
            upto_ms: *upto,
            count: 0,
        })
        .collect();
    for g in &gaps {
        if let Some(bin) = histogram.iter_mut().find(|b| *g < b.upto_ms) {
            bin.count += 1;
        }
    }
    let mut sorted = gaps.clone();
    sorted.sort_unstable();
    let samples = gaps.len() as i64;
    let avg = if samples > 0 {
        sorted.iter().sum::<i64>() / samples
    } else {
        0
    };
    let recent_ms = if gaps.len() > MAX_GAP_SAMPLES {
        gaps[gaps.len() - MAX_GAP_SAMPLES..].to_vec()
    } else {
        gaps
    };
    GapStats {
        samples,
        min_ms: sorted.first().copied().unwrap_or(0),
        p50_ms: percentile_sorted(&sorted, 0.5),
        avg_ms: avg,
        max_ms: sorted.last().copied().unwrap_or(0),
        histogram,
        recent_ms,
    }
}

fn latency_stats(rows: &[ListCallRow]) -> LatencyStats {
    let mut v: Vec<i64> = rows.iter().map(|r| r.latency_ms.max(0)).collect();
    v.sort_unstable();
    let samples = v.len() as i64;
    LatencyStats {
        samples,
        avg_ms: if samples > 0 {
            v.iter().sum::<i64>() / samples
        } else {
            0
        },
        p50_ms: percentile_sorted(&v, 0.5),
        max_ms: v.last().copied().unwrap_or(0),
    }
}

/// 把窗口切成对齐本地整点的时间桶并计数（含空桶）。
fn bucketize(rows: &[ListCallRow], since: Epoch, until: Epoch, bucket_secs: i64) -> Vec<Bucket> {
    let off = local_offset_secs() as f64;
    let b = bucket_secs as f64;
    let first = ((since + off) / b).floor() * b - off;
    let mut buckets: Vec<Bucket> = Vec::new();
    let mut t = first;
    while t <= until {
        buckets.push(Bucket {
            ts: t,
            counts: OutcomeCounts::default(),
        });
        t += b;
    }
    for r in rows {
        let idx = ((r.ts - first) / b).floor();
        if idx >= 0.0 && (idx as usize) < buckets.len() {
            buckets[idx as usize].counts.add(r);
        }
    }
    buckets
}

/// 把巡检批次按轮次号合成轮次汇总（轮次号降序）。
fn pass_summaries(runs: &[RunLogRow], sweep_running: bool) -> Vec<PassSummary> {
    let mut passes: Vec<PassSummary> = Vec::new();
    for r in runs.iter().filter(|r| r.kind == "sweep") {
        let Some(no) = r.pass_no else { continue };
        let p = match passes.iter_mut().find(|p| p.pass_no == no) {
            Some(p) => p,
            None => {
                passes.push(PassSummary {
                    pass_no: no,
                    started_at: r.started_at,
                    finished_at: r.finished_at,
                    ..Default::default()
                });
                passes.last_mut().expect("刚 push 过")
            }
        };
        p.started_at = p.started_at.min(r.started_at);
        p.finished_at = p.finished_at.max(r.finished_at);
        p.batches += 1;
        p.accounts += r.accounts;
        p.ok += r.ok;
        p.failed += r.failed;
        p.retry += r.retry;
        p.deferred += r.deferred;
        p.new_articles += r.new_articles;
        p.list_calls += r.list_calls;
        p.rate_limited_batches += r.rate_limited as i64;
        p.blocked_batches += r.blocked as i64;
        p.verify_batches += r.verify_hit as i64;
        p.env_failures += r.env_failure as i64;
    }
    passes.sort_by_key(|a| std::cmp::Reverse(a.pass_no));
    if let Some(latest) = passes.first_mut() {
        latest.in_progress = sweep_running;
    }
    passes
}

/// 聚合最近 `window_secs` 秒的限流分析统计。
pub fn stats(store: &Store, window_secs: i64) -> anyhow::Result<RateLimitStats> {
    let window_secs = window_secs.clamp(600, 30 * 86_400);
    let until = now();
    let since = until - window_secs as f64;
    let bucket_secs = bucket_secs_for(window_secs);

    let calls = store.list_calls_since(since, MAX_CALL_ROWS)?;
    let runs_asc = store.run_logs_since(since, 5_000)?;
    let cooldowns_asc = store.cooldown_logs_since(since, 1_000)?;

    let mut totals = OutcomeCounts::default();
    let mut by_source: Vec<SourceCounts> = Vec::new();
    let mut by_account: Vec<FactorCounts> = Vec::new();
    for r in &calls {
        totals.add(r);
        match by_source.iter_mut().find(|s| s.source == r.source) {
            Some(s) => s.counts.add(r),
            None => {
                let mut s = SourceCounts {
                    source: r.source.clone(),
                    counts: OutcomeCounts::default(),
                };
                s.counts.add(r);
                by_source.push(s);
            }
        }
        // 对照维度：微信号（uin 短哈希；老数据没有记为「未知」）。
        FactorCounts::bump(
            &mut by_account,
            r.factors.uin_hash.as_deref().unwrap_or("未知"),
            r,
        );
    }
    for g in by_account.iter_mut() {
        g.finish();
    }
    by_account.sort_by(|a, b| {
        b.last_ts
            .partial_cmp(&a.last_ts)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // 来源固定顺序：手动任务 → 巡检 → 手动重试（其它未知来源排最后；老库的 `upstream` 与手动任务同档）。
    let order = |s: &str| match s {
        "manual" | "upstream" => 0,
        "sweep" => 1,
        "retry" => 2,
        "history" => 3,
        _ => 9,
    };
    by_source.sort_by_key(|s| order(&s.source));

    let buckets = bucketize(&calls, since, until, bucket_secs);
    let peak_bucket_calls = buckets.iter().map(|b| b.counts.calls).max().unwrap_or(0);

    let sweep_status = runstate::sweep_status();
    let passes = pass_summaries(&runs_asc, sweep_status.phase == SweepPhase::Running);
    let runs_manual = runs_asc
        .iter()
        .filter(|r| r.kind == "manual" || r.kind == "upstream")
        .count() as i64;
    let runs_sweep = runs_asc.iter().filter(|r| r.kind == "sweep").count() as i64;
    let mut runs: Vec<RunLogRow> = runs_asc.into_iter().rev().take(80).collect();
    runs.sort_by(|a, b| {
        b.started_at
            .partial_cmp(&a.started_at)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let cooldown_hits = cooldowns_asc.iter().filter(|c| c.kind == "ip").count() as i64;
    let block_hits = cooldowns_asc.iter().filter(|c| c.kind == "account").count() as i64;
    let cooldown_total_secs = cooldowns_asc
        .iter()
        .filter(|c| c.kind == "ip" || c.kind == "account")
        .map(|c| c.secs)
        .sum();
    let cooldowns: Vec<CooldownLogRow> = cooldowns_asc.into_iter().rev().take(50).collect();

    let (_, daily_budget) = runstate::list_calls_snapshot();
    Ok(RateLimitStats {
        window_secs,
        generated_at: until,
        bucket_secs,
        gaps: gap_stats(&calls),
        latency: latency_stats(&calls),
        totals,
        by_source,
        by_account,
        buckets,
        peak_bucket_calls,
        passes,
        runs,
        runs_manual,
        runs_sweep,
        cooldowns,
        cooldown_hits,
        block_hits,
        cooldown_total_secs,
        cooldown_now: runstate::cooldown_snapshot(),
        today_calls: store.list_calls_today().unwrap_or(0),
        uin_calls_24h: store
            .current_uin_hash()
            .ok()
            .flatten()
            .and_then(|h| {
                store
                    .list_calls_window(Some(&h), runstate::LIST_BUDGET_WINDOW_SECS)
                    .ok()
            })
            .or_else(|| {
                store
                    .list_calls_window(None, runstate::LIST_BUDGET_WINDOW_SECS)
                    .ok()
            })
            .map(|(n, _)| n)
            .unwrap_or(0),
        daily_budget,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ListCallEvent, RunLogEvent};

    fn call(biz: &str, source: &str, outcome: &str, articles: i64) -> ListCallEvent {
        ListCallEvent {
            biz: biz.into(),
            source: source.into(),
            job_id: Some(1),
            page: 1,
            outcome: outcome.into(),
            latency_ms: 200,
            articles,
            new_articles: articles / 2,
            note: None,
            factors: Default::default(),
        }
    }

    /// 影响因素列能落库、读回，并按微信号聚合；老数据（无因素）归到「未知」。
    #[test]
    fn factors_round_trip_and_grouping() {
        use crate::model::ListCallFactors;
        let store = Store::open_in_memory().unwrap();
        let t0 = now() - 50.0;
        let mut a = call("b1", "sweep", "ok", 10);
        a.factors = ListCallFactors {
            proxy: Some("direct".into()),
            uin_hash: crate::model::short_hash("uin-A"),
            key_hash: crate::model::short_hash("key-1"),
            cred_age_s: Some(30),
            ret: Some(0),
            http_status: Some(200),
            offset: Some(0),
            gap_ms: Some(9000),
            gate_min_ms: Some(8000),
            gate_max_ms: Some(20000),
            day_seq: Some(1),
        };
        store.append_list_call_at(t0, &a).unwrap();
        let mut b = call("b2", "sweep", "blocked", 0);
        b.factors.proxy = Some("direct".into());
        b.factors.uin_hash = crate::model::short_hash("uin-B");
        b.factors.ret = Some(-6);
        store.append_list_call_at(t0 + 10.0, &b).unwrap();
        store
            .append_list_call_at(t0 + 20.0, &call("b3", "manual", "ok", 5))
            .unwrap();

        let rows = store.list_calls_since(t0 - 1.0, 100).unwrap();
        assert_eq!(rows[0].factors, a.factors);
        assert_eq!(rows[1].factors.ret, Some(-6));
        assert!(rows[2].factors.proxy.is_none());

        let s = stats(&store, 3600).unwrap();
        assert_eq!(s.by_account.len(), 3, "两个微信号 + 一个未知");
        assert_eq!(s.by_account[0].key, "未知", "最近使用的排前");
        let acc_b = s
            .by_account
            .iter()
            .find(|g| Some(&g.key) == crate::model::short_hash("uin-B").as_ref())
            .unwrap();
        assert_eq!(acc_b.counts.blocked, 1);
    }

    #[test]
    fn totals_sources_gaps_and_buckets() {
        let store = Store::open_in_memory().unwrap();
        let t0 = now() - 100.0;
        // 5 次请求：间隔 10s / 10s / 2s / 30s
        let seq = [
            (0.0, "sweep", "ok", 10),
            (10.0, "sweep", "ok", 10),
            (20.0, "manual", "expired", 0),
            (22.0, "manual", "rate_limited", 0),
            (52.0, "retry", "blocked", 0),
        ];
        for (dt, src, oc, n) in seq {
            store
                .append_list_call_at(t0 + dt, &call("AAA", src, oc, n))
                .unwrap();
        }
        let s = stats(&store, 3600).unwrap();
        assert_eq!(s.totals.calls, 5);
        assert_eq!(s.totals.ok, 2);
        assert_eq!(s.totals.expired, 1);
        assert_eq!(s.totals.rate_limited, 1);
        assert_eq!(s.totals.blocked, 1);
        assert_eq!(s.totals.articles, 20);
        assert_eq!(s.totals.new_articles, 10);
        assert_eq!(
            s.by_source
                .iter()
                .map(|x| x.source.as_str())
                .collect::<Vec<_>>(),
            vec!["manual", "sweep", "retry"]
        );
        assert_eq!(s.gaps.samples, 4);
        assert_eq!(s.gaps.min_ms, 2_000);
        assert_eq!(s.gaps.max_ms, 30_000);
        assert_eq!(s.gaps.recent_ms, vec![10_000, 10_000, 2_000, 30_000]);
        // 直方图：<3s 一个（2s），8–12s 两个（10s），20–60s 一个（30s）
        let bin = |label: &str| {
            s.gaps
                .histogram
                .iter()
                .find(|b| b.label == label)
                .unwrap()
                .count
        };
        assert_eq!(bin("<3s"), 1);
        assert_eq!(bin("8–12s"), 2);
        assert_eq!(bin("20–60s"), 1);
        assert_eq!(s.bucket_secs, 600);
        assert_eq!(s.buckets.iter().map(|b| b.counts.calls).sum::<i64>(), 5);
        assert!(s.peak_bucket_calls >= 1);
        assert_eq!(s.latency.p50_ms, 200);
        // 窗口外的不算
        store
            .append_list_call_at(now() - 7200.0, &call("AAA", "sweep", "ok", 1))
            .unwrap();
        let s2 = stats(&store, 3600).unwrap();
        assert_eq!(s2.totals.calls, 5);
    }

    #[test]
    fn passes_group_batches_and_cooldowns_counted() {
        let store = Store::open_in_memory().unwrap();
        let t = now();
        let batch = |pass: i64, start: f64, ok: i64, rl: bool| RunLogEvent {
            job_id: Some(1),
            kind: "sweep".into(),
            pass_no: Some(pass),
            started_at: start,
            finished_at: start + 60.0,
            accounts: 20,
            ok,
            failed: 20 - ok,
            retry: 0,
            deferred: 0,
            new_articles: 3,
            urls: 5,
            list_calls: 20,
            rate_limited: rl,
            blocked: false,
            verify_hit: false,
            env_failure: false,
            truncated: rl,
            note: None,
        };
        store
            .append_run_log(&batch(1, t - 3000.0, 20, false))
            .unwrap();
        store
            .append_run_log(&batch(1, t - 2000.0, 10, true))
            .unwrap();
        store
            .append_run_log(&batch(2, t - 500.0, 20, false))
            .unwrap();
        store
            .append_run_log(&RunLogEvent {
                kind: "manual".into(),
                started_at: t - 100.0,
                finished_at: t - 50.0,
                accounts: 1,
                ok: 1,
                ..Default::default()
            })
            .unwrap();
        store
            .append_cooldown_log_at(t - 1990.0, "ip", 1, 300, "429")
            .unwrap();
        store
            .append_cooldown_log_at(t - 1000.0, "account", 2, 21600, "ret=-6")
            .unwrap();
        store
            .append_cooldown_log_at(t - 900.0, "clear", 0, 0, "手动")
            .unwrap();

        let s = stats(&store, 86_400).unwrap();
        assert_eq!(s.passes.len(), 2);
        assert_eq!(s.passes[0].pass_no, 2, "轮次号降序");
        let p1 = &s.passes[1];
        assert_eq!(p1.batches, 2);
        assert_eq!(p1.accounts, 40);
        assert_eq!(p1.ok, 30);
        assert_eq!(p1.failed, 10);
        assert_eq!(p1.list_calls, 40);
        assert_eq!(p1.rate_limited_batches, 1);
        assert_eq!(p1.started_at, t - 3000.0);
        assert_eq!(p1.finished_at, t - 2000.0 + 60.0);
        assert_eq!(s.runs_manual, 1);
        assert_eq!(s.runs_sweep, 3);
        assert_eq!(s.runs.len(), 4);
        assert!(
            s.runs[0].started_at >= s.runs[1].started_at,
            "执行单元按开始时间降序"
        );
        assert_eq!(s.cooldown_hits, 1);
        assert_eq!(s.block_hits, 1);
        assert_eq!(s.cooldown_total_secs, 21_900);
        assert_eq!(s.cooldowns.len(), 3);
        assert_eq!(s.cooldowns[0].kind, "clear", "退避记录按时间降序");
    }

    #[test]
    fn bucket_size_scales_with_window() {
        assert_eq!(bucket_secs_for(3600), 600);
        assert_eq!(bucket_secs_for(24 * 3600), 3600);
        assert_eq!(bucket_secs_for(7 * 86_400), 6 * 3600);
    }
}
