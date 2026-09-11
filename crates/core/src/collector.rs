//! 采集编排层：把 wechat 的请求/解析与 store 的读写串起来（对齐 Python `collector.py`）。
//!
//! 本阶段实现 `collect_list`：从 store 取凭证 → 不新鲜报 [`CredentialExpired`] →
//! `fetch_msg_list` → `parse_msg_list` → 入库；按 [`ListPlan`] 决定翻不翻页：
//!
//! - **无「最后更新时间」**（`since=None`）：只采第 1 页（对齐 Python `incremental`）。
//! - **有**：逐页比对发布时间，**直到某页出现早于 `since` 的文章**才停（该页仍入库；
//!   早于 `since` 的只入库、不进回报 `urls`，等于的仍回报——同次群发头条/次条共用 datetime）；
//!   两页之间**随机等待** `[page_sleep_min_ms, page_sleep_max_ms]`；翻页数封顶 `max_pages`。
//! - 翻页途中微信回 `ret=-3`（no session，凭证过期）：**不丢已采部分**——打点实测失效
//!   （`store.mark_credential_invalid`），返回 `cred_expired=true` + `next_offset`，由编排层
//!   统一续期后从该 offset 续采（见 `credrefresh` / `orchestrator`）。
//!
//! 与 Python 的差异：Python `collect_list(mode=custom, since_date)` 是"采到早于 since 就停"
//! 的全量翻页，且页间固定 `MP_SCRAPER_SLEEP` 秒；这里页间随机、可从任意 offset 续采、
//! 且过期不抛异常而是带状态返回。

use std::error::Error;
use std::fmt;

use anyhow::Result;

use crate::applog::{self, Stage};
use crate::model::ListCallFactors;
use crate::rng;
use crate::runstate;
use crate::store::Store;
use crate::wechat::{self, is_credential_expired, parse_msg_list, response_ret};
pub use crate::wechat::{is_account_blocked_err, is_rate_limited_err, AccountBlocked, RateLimited};

/// 凭证缺失/过期。消息即给用户的续期提示（对齐 Python `collector.CredentialExpired`）。
#[derive(Debug, Clone)]
pub struct CredentialExpired(pub String);

impl fmt::Display for CredentialExpired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl Error for CredentialExpired {}

/// 凭证失效时给用户的统一提示。
pub const CRED_HINT: &str = "请在微信内置浏览器打开该公众号任一文章并刷新以续期";

/// 把一次真实发出的 `getmsg` 请求记进 `list_call_log`（限流分析的原始材料）。
/// `started` 是请求发出时刻，耗时据此算；`note` 会先脱敏再入库；`factors` 是当时的影响因素
/// （代理 / 微信号 / 凭证 / ret / 间隔 …，见 [`ListCallFactors`]）。写库失败只忽略，不影响采集。
#[allow(clippy::too_many_arguments)]
fn record_call(
    store: &Store,
    biz: &str,
    plan: &ListPlan,
    page: usize,
    started: std::time::Instant,
    outcome: &str,
    articles: usize,
    new_articles: usize,
    note: Option<String>,
    factors: ListCallFactors,
) {
    let _ = store.append_list_call(&crate::model::ListCallEvent {
        biz: biz.to_string(),
        source: plan.source.as_str().to_string(),
        job_id: applog::bus().job(),
        page: page as i64,
        outcome: outcome.to_string(),
        latency_ms: started.elapsed().as_millis() as i64,
        articles: articles as i64,
        new_articles: new_articles as i64,
        note: note.map(|n| applog::redact(&n)),
        factors,
    });
    // 预算镜像：按当前微信号滚动 24 小时重新数一遍（权威值在 `list_call_log`），供 GUI「n / 预算」展示。
    if let Ok(uin_hash) = store.current_uin_hash() {
        if let Ok((n, _)) =
            store.list_calls_window(uin_hash.as_deref(), runstate::LIST_BUDGET_WINDOW_SECS)
        {
            runstate::set_list_calls_24h(n);
        }
    }
}

/// 一页请求开始前就能确定的影响因素（与请求结果无关的部分）。`ret` / `http_status` 由各结果分支补。
fn base_factors(
    plan: &ListPlan,
    cred: &crate::model::Credential,
    offset: i64,
    day_seq: Option<i64>,
    gap_ms: Option<i64>,
) -> ListCallFactors {
    ListCallFactors {
        // 出口恒为直连（本项目不走出口代理）；列保留作留档。
        proxy: Some("direct".to_string()),
        uin_hash: cred.uin.as_deref().and_then(crate::model::short_hash),
        key_hash: cred.key.as_deref().and_then(crate::model::short_hash),
        cred_age_s: Some((crate::model::now() - cred.captured_at).max(0.0) as i64),
        ret: None,
        http_status: None,
        offset: Some(offset),
        gap_ms,
        gate_min_ms: Some(plan.gap_min_ms as i64),
        gate_max_ms: Some(plan.gap_max_ms as i64),
        day_seq,
    }
}

/// 列表采集计划（编排层按任务的「最后更新时间」与运行配置拼出）。
#[derive(Clone, Debug, PartialEq)]
pub struct ListPlan {
    /// 「最后更新时间」（epoch 秒）。`None` = 只采第 1 页。
    pub since: Option<f64>,
    /// 起始 offset（续采时从上次中断处继续；首采为 0）。
    pub start_offset: i64,
    /// 每页条数（getmsg `count`）。
    pub count: i64,
    /// 本次最多翻页数（防止 since 太早时无止境翻；到顶返回 `next_offset` 让上层决定）。
    pub max_pages: usize,
    /// 两页之间随机等待的下限 / 上限（毫秒）。
    pub page_sleep_min_ms: u64,
    pub page_sleep_max_ms: u64,
    /// 撞到限流信号（HTTP 429 / 5xx / 验证页）后等多久再**重试一次**同一页（毫秒）；
    /// 第二次仍限流才向上层报 [`crate::wechat::RateLimited`]（上层整机退避）。0 = 不等直接重试。
    pub rate_retry_wait_ms: u64,
    /// **全局** `getmsg` 闸门：任意两次列表请求（跨号、跨页、跨任务源）之间至少隔
    /// `[gap_min_ms, gap_max_ms]` 内的随机值（[`crate::runstate::list_gate_wait`]）。
    /// 与页间等待叠加时取自上次请求起的间隔。0/0 = 不设闸门（单测）。
    pub gap_min_ms: u64,
    pub gap_max_ms: u64,
    /// 请求来源（只用于限流分析 `list_call_log` 的 `source` 列）。
    pub source: ListSource,
    /// **纯翻页模式**（历史抓取用）：不按 `since` 判停（`since` 应为 `None`），只按 `max_pages` 停并回传
    /// `next_offset`；到底（`next_offset` 不前进 / 空页）时 `next_offset=None`、`reached_since=true`。
    /// 目标过滤（时间范围 / 条数）由调用方按 `items` 自行判定。
    pub paginate: bool,
}

/// `getmsg` 请求来源（限流分析按来源分组：定时巡检 / 单号手动重试 / 历史抓取）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ListSource {
    #[default]
    Sweep,
    Retry,
    /// 「抓取历史文章」长任务（一页一个执行单元）。
    History,
}

impl ListSource {
    /// 入库用的 code。
    pub fn as_str(self) -> &'static str {
        match self {
            ListSource::Sweep => "sweep",
            ListSource::Retry => "retry",
            ListSource::History => "history",
        }
    }
}

impl Default for ListPlan {
    fn default() -> Self {
        Self {
            since: None,
            start_offset: 0,
            count: 10,
            max_pages: 50,
            page_sleep_min_ms: 3000,
            page_sleep_max_ms: 8000,
            source: ListSource::Sweep,
            rate_retry_wait_ms: 15_000,
            gap_min_ms: 8_000,
            gap_max_ms: 20_000,
            paginate: false,
        }
    }
}

impl ListPlan {
    /// 只采第 1 页的计划（无最后更新时间）。
    pub fn first_page() -> Self {
        Self::default()
    }
}

/// 一次列表采集的请求（编排层 → 采集闭包的标准输入）。
#[derive(Clone, Debug, PartialEq)]
pub struct ListRequest {
    pub biz: String,
    pub plan: ListPlan,
}

/// 列表采集结果（在 Python `{total,new,pages,feedback}` 之上多了续采/回报所需状态）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CollectOutcome {
    pub total: usize,
    pub new: usize,
    pub pages: usize,
    pub feedback: String,
    /// 本次各页文章的 `content_url`（按页序、**不去重**，回报原样上送）。
    /// 有 `since` 时只含发布时间 **≥ since** 的文章；早于的只入库、计入 `skipped_older`。
    pub urls: Vec<String>,
    /// 因早于 `since` 而没进 `urls` 的篇数（已入本地库）。
    pub skipped_older: usize,
    /// `Some(offset)` = 没采完（凭证过期 / 到 `max_pages`），下次从该 offset 续；`None` = 已完成。
    pub next_offset: Option<i64>,
    /// 途中微信回 `ret=-3`（凭证过期）——需要续期后从 `next_offset` 续采。
    pub cred_expired: bool,
    /// 已看到早于 `since` 的文章（或没有更多历史）——本号增量已到底。
    pub reached_since: bool,
    /// 首采首页命中**静默空页**（[`wechat::is_silent_empty_list`]）——微信号疑似被软限制，
    /// 不是这个公众号没文章。上层据此告警 / 换号，不要当成功。
    pub suspect_blocked: bool,
    /// `urls` 对应的文章明细（同序；上报载荷用：标题 / 发布时间 / 是否本次新入库）。
    pub items: Vec<CollectedArticle>,
}

/// 本次采到的一篇文章（进 `urls` 的那些）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CollectedArticle {
    pub url: String,
    pub title: Option<String>,
    /// 发布时间（epoch 秒）。
    pub published_at: Option<i64>,
    /// 本次采集新入本地库（此前库里没有）。
    pub is_new: bool,
}

impl CollectOutcome {
    /// 是否已完成（无需续采）。
    pub fn is_done(&self) -> bool {
        self.next_offset.is_none()
    }
}

/// 把 getmsg 响应的顶层概括成一行日志：标量字段原样、非标量只记字段名与长度/条数。
/// 不含 `general_msg_list` 内容（正文/链接太长），响应本身也不含凭证。
fn describe_list_response(raw: &serde_json::Value) -> String {
    use serde_json::Value;
    let Some(obj) = raw.as_object() else {
        return format!("非 JSON 对象（{}）", short_type(raw));
    };
    let mut parts: Vec<String> = Vec::with_capacity(obj.len());
    for (k, v) in obj {
        let shown = match v {
            Value::Null | Value::Bool(_) | Value::Number(_) => v.to_string(),
            Value::String(s) => {
                if k == "general_msg_list" {
                    format!("<字符串 {} 字节>", s.len())
                } else {
                    format!("{:?}", s.chars().take(40).collect::<String>())
                }
            }
            Value::Array(a) => format!("<数组 {} 项>", a.len()),
            Value::Object(o) => format!("<对象 {} 键>", o.len()),
        };
        parts.push(format!("{k}={shown}"));
    }
    parts.join(", ")
}

fn short_type(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// 采集公众号文章列表（第 1 页 / 按最后更新时间翻页 / 从 offset 续采）。
///
/// - 凭证不新鲜（或缺失）→ 返回 `Err(CredentialExpired)`（一页都没发，可用
///   `is_credential_expired_err` 判别）；上层续期后用同一计划重来即可。
/// - 途中 `ret=-3` → `Ok(outcome)` 且 `cred_expired=true`、`next_offset=Some(当前页 offset)`。
///
/// 请求一律直连；`wrap` 是单测用的 URL 重写钩子（把 `getmsg` 指到本地 mock），真机传 `None`。
pub async fn collect_list(
    store: &Store,
    biz: &str,
    plan: &ListPlan,
    cred_ttl_secs: i64,
    wrap: Option<&(dyn Fn(&str) -> String + Send + Sync)>,
) -> Result<CollectOutcome> {
    if !store.credential_is_fresh(biz, cred_ttl_secs)? {
        return Err(CredentialExpired(CRED_HINT.to_string()).into());
    }
    let cred = store
        .get_credential(biz)?
        .ok_or_else(|| CredentialExpired(CRED_HINT.to_string()))?;

    let baseline = store.max_article_id(biz)?;
    let mut offset = plan.start_offset.max(0);
    let mut out = CollectOutcome::default();
    let count = plan.count.max(1);
    let max_pages = plan.max_pages.max(1);
    let label = store.account_label(biz);
    // 页号只用于日志：首采从第 1 页起；续采不知道之前翻了几页，按 offset/count 估算。
    let page_base = if plan.start_offset > 0 {
        (plan.start_offset / count) as usize
    } else {
        0
    };
    // 日志里的页名：第 1 页叫「首页」，之后叫「第 N 页」。不出现请求 URL / 凭证参数。
    let page_name = |n: usize| {
        if n == 1 {
            "首页".to_string()
        } else {
            format!("第 {n} 页")
        }
    };

    // 采集计划留痕：排查"为什么没翻页"时能直接看到本号生效的 since / 上限。
    applog::info(
        Stage::List,
        match plan.since {
            Some(s) => format!(
                "{label} 采集计划：按最后发布时间 {} 翻页，每页 {count} 条，最多 {max_pages} 页，从 offset={offset} 起",
                applog::format_local(s)
            ),
            None => format!("{label} 采集计划：无最后发布时间，只采首页（每页 {count} 条）"),
        },
    );

    // 每次真正发 getmsg 前：过全局闸门（跨号 / 跨页 / 跨任务源统一节流）+ 记今日计数 + 记凭证使用。
    // 闭包只捕获引用（都是 Copy），才能在循环里多次调用。
    let label_ref = &label;
    let page_name_ref = &page_name;
    let before_request = |page: usize| async move {
        let (label, page_name) = (label_ref, page_name_ref);
        let waited = runstate::list_gate_wait(plan.gap_min_ms, plan.gap_max_ms).await;
        if waited >= 500 {
            applog::progress(
                Stage::List,
                format!(
                    "{label} 列表请求闸门：距上次请求不足，已等待 {:.1}s（{}）",
                    waited as f64 / 1000.0,
                    page_name(page)
                ),
            );
        }
        let day_seq = store.bump_list_calls().ok();
        let _ = store.touch_credential_used(biz);
        // 留档用：当日序号 + 与上一次 getmsg 的实际间隔（闸门放行时记下的）。
        (day_seq, runstate::list_gate_last_gap_ms().map(|g| g as i64))
    };

    loop {
        let page_no = page_base + out.pages + 1;
        applog::info(
            Stage::List,
            format!("开始获取 {label} {}文章列表…", page_name(page_no)),
        );
        let (day_seq, gap_ms) = before_request(page_no).await;
        let mut factors = base_factors(plan, &cred, offset, day_seq, gap_ms);
        // 每次真实发出的请求都记进 list_call_log（限流分析）；重试算第二次请求。
        let mut t_req = std::time::Instant::now();
        let raw = match wechat::fetch_msg_list(&cred, offset, count, wrap).await {
            Ok(v) => v,
            // 限流信号：等一小段再重试同一页一次（区分偶发抖动与真限流）；仍限流才报给上层整机退避。
            Err(e) if is_rate_limited_err(&e) => {
                record_call(
                    store,
                    biz,
                    plan,
                    page_no,
                    t_req,
                    "rate_limited",
                    0,
                    0,
                    Some(format!("{e}；等待后重试一次")),
                    ListCallFactors {
                        http_status: wechat::error_http_status(&e),
                        ..factors.clone()
                    },
                );
                applog::warn(
                    Stage::List,
                    format!(
                        "{label} {}疑似被限流：{e}；等待 {:.0}s 后重试一次",
                        page_name(page_no),
                        plan.rate_retry_wait_ms as f64 / 1000.0
                    ),
                );
                tokio::time::sleep(std::time::Duration::from_millis(plan.rate_retry_wait_ms)).await;
                let (day_seq, gap_ms) = before_request(page_no).await;
                factors = base_factors(plan, &cred, offset, day_seq, gap_ms);
                t_req = std::time::Instant::now();
                match wechat::fetch_msg_list(&cred, offset, count, wrap).await {
                    Ok(v) => v,
                    Err(e2) => {
                        record_call(
                            store,
                            biz,
                            plan,
                            page_no,
                            t_req,
                            if is_rate_limited_err(&e2) {
                                "rate_limited"
                            } else {
                                "error"
                            },
                            0,
                            0,
                            Some(e2.to_string()),
                            ListCallFactors {
                                http_status: wechat::error_http_status(&e2),
                                ..factors.clone()
                            },
                        );
                        applog::error(
                            Stage::List,
                            format!(
                                "{label} {}重试仍失败：{e2}{}",
                                page_name(page_no),
                                if is_rate_limited_err(&e2) {
                                    "；判定为限流，整机退避"
                                } else {
                                    ""
                                }
                            ),
                        );
                        return Err(e2);
                    }
                }
            }
            Err(e) => {
                record_call(
                    store,
                    biz,
                    plan,
                    page_no,
                    t_req,
                    "error",
                    0,
                    0,
                    Some(e.to_string()),
                    ListCallFactors {
                        http_status: wechat::error_http_status(&e),
                        ..factors.clone()
                    },
                );
                applog::error(
                    Stage::List,
                    format!("{label} {}获取失败：{e}", page_name(page_no)),
                );
                return Err(e);
            }
        };
        // 拿到 JSON 即 HTTP 2xx（非 2xx / 网页在 fetch 层已转成错误）；ret 缺失按 0 记。
        factors.http_status = Some(200);
        factors.ret = Some(response_ret(&raw).unwrap_or(0));
        if is_credential_expired(&raw) {
            record_call(
                store,
                biz,
                plan,
                page_no,
                t_req,
                "expired",
                0,
                0,
                Some("ret=-3 凭证过期".to_string()),
                ListCallFactors {
                    ret: Some(-3),
                    ..factors.clone()
                },
            );
            // 实测失效打点：热表立即判不新鲜、留档记真实寿命。已采部分保留，交上层续期后续采。
            let _ = store.mark_credential_invalid(biz);
            out.cred_expired = true;
            out.next_offset = Some(offset);
            applog::warn(
                Stage::List,
                format!(
                    "{label} {}获取失败：凭证已过期（微信返回 ret=-3）；已采 {} 页 {} 篇，待集中续期后从该页续采",
                    page_name(page_no),
                    out.pages,
                    out.total
                ),
            );
            out.feedback = format!(
                "累计采集{}篇，新增{}篇（翻页{}页）；凭证过期，待续期后从 offset={offset} 续采",
                out.total, out.new, out.pages
            );
            return Ok(out);
        }
        // 响应顶层留痕（只记标量与字段名，不含 general_msg_list 内容，也没有凭证）：
        // 排查"为什么判没有更多历史"时看 can_msg_continue / next_offset 的原始取值。
        applog::info(
            Stage::List,
            format!(
                "{label} {}响应：{}",
                page_name(page_no),
                describe_list_response(&raw)
            ),
        );
        let arts = parse_msg_list(raw.clone());
        if arts.is_empty() {
            // 微信明确回了非 0 的 ret（-3 已在上面处理）：这是接口错误，不是「翻到底」。
            // 与 Python 基准的差异：Python 把任何解析不出列表的响应都当空页处理，会把
            // 接口报错静默记成「没有更多历史」。这里改为报错交上层（巡检 → 记失败）。
            if let Some(ret) = response_ret(&raw).filter(|r| *r != 0) {
                let errmsg = raw
                    .get("errmsg")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // ret=-6/-12：微信已把本机微信号识别为异常（实测：新抓的凭证也一律 -6）。
                // 不是这个号的问题、也不是凭证过期——不重试、不换 key，交上层长时退避。
                record_call(
                    store,
                    biz,
                    plan,
                    page_no,
                    t_req,
                    if wechat::is_blocked_ret(ret, &errmsg) {
                        "blocked"
                    } else {
                        "error"
                    },
                    0,
                    0,
                    Some(format!("ret={ret} {errmsg}")),
                    factors.clone(),
                );
                if wechat::is_blocked_ret(ret, &errmsg) {
                    applog::error(
                        Stage::List,
                        format!(
                            "{label} {}获取失败：微信返回 ret={ret}（{errmsg}）——微信号已被识别为异常请求方（账号级限制，约 1 天），停止一切列表请求",
                            page_name(page_no)
                        ),
                    );
                    return Err(wechat::AccountBlocked(format!(
                        "微信文章列表接口返回 ret={ret} {errmsg}（微信号被限制）"
                    ))
                    .into());
                }
                applog::error(
                    Stage::List,
                    format!(
                        "{label} {}获取失败：微信返回 ret={ret}{}",
                        page_name(page_no),
                        if errmsg.is_empty() {
                            String::new()
                        } else {
                            format!("（{errmsg}）")
                        }
                    ),
                );
                anyhow::bail!("微信文章列表接口返回 ret={ret} {errmsg}");
            }
            // 首采首页就空、且响应里连列表容器都没有：2026-09-10 起接口的常态（见
            // `wechat::is_silent_empty_list`），不是这个公众号没文章——按异常记，不报「没有更多历史」。
            let silent = page_no == 1 && offset == 0 && wechat::is_silent_empty_list(&raw);
            record_call(
                store,
                biz,
                plan,
                page_no,
                t_req,
                if silent { "empty_suspect" } else { "ok" },
                0,
                0,
                silent.then(|| "首页静默空页：ret=0 但无 general_msg_list".to_string()),
                factors.clone(),
            );
            if silent {
                out.suspect_blocked = true;
                applog::warn(
                    Stage::List,
                    format!(
                        "⚠ {label} 首页返回成功却没有任何文章（微信既不报错也不给列表）——2026-09-10 起该接口已取不到列表数据，与账号 / 机器 / 公众号无关，换号也不会恢复；详见「常见问题」页第一条"
                    ),
                );
                crate::notify::notify(
                    crate::notify::Kind::Cooldown,
                    "文章列表接口返回成功却没有任何文章：自 2026-09-10 起该接口已取不到列表数据，换微信号也不会恢复，详见软件「常见问题」页。",
                );
            } else {
                applog::info(
                    Stage::List,
                    format!("{label} {}为空：没有更多历史文章", page_name(page_no)),
                );
            }
            out.reached_since = true;
            break;
        }
        let (page_total, page_new_before) = (arts.len(), out.new);
        let mut saw_older = false;
        for a in &arts {
            let fields = crate::model::ArticleFields {
                sn: a.sn.clone(),
                title: a.title.clone(),
                digest: a.digest.clone(),
                content_url: a.content_url.clone(),
                cover: a.cover.clone(),
                author: a.author.clone(),
                published_at: a.published_at.map(|s| s as f64),
                ..Default::default()
            };
            let aid = store.upsert_article(biz, &a.mid, a.idx, &fields)?;
            out.total += 1;
            let is_new = aid > baseline;
            if is_new {
                out.new += 1;
            }
            // 早于最后发布时间的文章：只入本地库，不进上报 urls（只报该时间点之后的）。
            // 等于的仍回报：同一次群发的头条/次条共用一个 datetime，剔掉会漏次条，接收方按 URL 去重。
            let older = match (plan.since, a.published_at) {
                (Some(since), Some(pub_at)) => (pub_at as f64) < since,
                _ => false,
            };
            if older {
                saw_older = true;
                out.skipped_older += 1;
            } else if let Some(u) = a.content_url.as_ref().filter(|u| !u.is_empty()) {
                out.urls.push(u.clone());
                out.items.push(CollectedArticle {
                    url: u.clone(),
                    title: a.title.clone(),
                    published_at: a.published_at,
                    is_new,
                });
            }
        }
        out.pages += 1;
        record_call(
            store,
            biz,
            plan,
            page_no,
            t_req,
            "ok",
            page_total,
            out.new - page_new_before,
            None,
            factors.clone(),
        );
        // 本页发布时间范围（最新 ~ 最早），排查翻页停止原因用。
        let span = {
            let ts: Vec<i64> = arts.iter().filter_map(|a| a.published_at).collect();
            match (ts.iter().max(), ts.iter().min()) {
                (Some(&hi), Some(&lo)) => format!(
                    "，发布时间 {} ~ {}",
                    applog::format_local(hi as f64),
                    applog::format_local(lo as f64)
                ),
                _ => String::new(),
            }
        };
        applog::info(
            Stage::List,
            format!(
                "{label} {}获取成功：{page_total} 篇（新增 {} 篇）{span}",
                page_name(page_no),
                out.new - page_new_before
            ),
        );

        // 无最后更新时间：只采第 1 页（纯翻页模式除外：按 max_pages / 到底判停）。
        if plan.since.is_none() && !plan.paginate {
            out.reached_since = true;
            break;
        }
        let since = plan.since.unwrap_or(0.0);
        // 本页已出现早于最后更新时间的文章：增量到底。
        if saw_older {
            applog::info(
                Stage::List,
                format!(
                    "{label} 本页已出现早于最后发布时间（{}）的文章 {} 篇（只入库、不回报），停止翻页",
                    applog::format_local(since),
                    out.skipped_older
                ),
            );
            out.reached_since = true;
            break;
        }
        // 翻页判据：`can_msg_continue` 只当**提示**，真正看 `next_offset` 是否还在前进。
        // 真机实测（2026-09-04）：某公众号首页就返回 can_msg_continue=0、msg_count=10、
        // next_offset=10，按标志停会漏掉全部历史；带着 next_offset 继续请求仍能翻到更早的页。
        // 与 Python 基准的差异：Python 见 can_msg_continue 为假即停。到底的判据改为：
        // next_offset 缺失/不前进，或下一页返回空列表（循环顶部已处理）。
        let can_continue = raw
            .get("can_msg_continue")
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .unwrap_or(0);
        let next_offset = raw.get("next_offset").and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        });
        let advancing = next_offset.is_some_and(|n| n > offset);
        if can_continue == 0 && !advancing {
            applog::info(
                Stage::List,
                format!(
                    "{label} 没有更多历史文章（can_msg_continue={}，next_offset={}），停止翻页",
                    raw.get("can_msg_continue")
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "缺失".into()),
                    next_offset
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "缺失".into())
                ),
            );
            out.reached_since = true;
            break;
        }
        if can_continue == 0 {
            applog::info(
                Stage::List,
                format!(
                    "{label} 微信标记没有更多（can_msg_continue=0）但 next_offset={} 仍在前进，继续探下一页",
                    next_offset.unwrap_or_default()
                ),
            );
        }
        offset = next_offset.unwrap_or(offset + arts.len() as i64);

        // 翻页数封顶：交回 next_offset，由上层决定是否继续。
        if out.pages >= max_pages {
            applog::warn(
                Stage::List,
                format!("{label} 本次翻页已达上限 {max_pages} 页，停止"),
            );
            out.next_offset = Some(offset);
            out.feedback = format!(
                "累计采集{}篇，新增{}篇（翻页{}页，达上限，下次从 offset={offset} 续）",
                out.total, out.new, out.pages
            );
            return Ok(out);
        }

        // 还要翻下一页才等待：随机间隔（getmsg 是带凭证的私有接口，固定节拍是可疑特征）。
        let ms = rng::between(plan.page_sleep_min_ms, plan.page_sleep_max_ms);
        applog::progress(
            Stage::List,
            format!(
                "{label} 等待 {:.1}s 后获取{}…",
                ms as f64 / 1000.0,
                page_name(page_no + 1)
            ),
        );
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    }

    out.feedback = format!(
        "累计采集{}篇，新增{}篇（翻页{}页）",
        out.total, out.new, out.pages
    );
    Ok(out)
}

/// 判别一个错误是否为凭证过期（供 orchestrator 决定"跳过该号"）。
pub fn is_credential_expired_err(err: &anyhow::Error) -> bool {
    err.downcast_ref::<CredentialExpired>().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CredentialFields;

    #[tokio::test]
    async fn test_collect_list_credential_expired_without_fresh_cred() {
        let store = Store::open_in_memory().unwrap();
        // 无凭证 → CredentialExpired（不发网络）
        let err = collect_list(&store, "BIZ==", &ListPlan::first_page(), 1800, None)
            .await
            .unwrap_err();
        assert!(is_credential_expired_err(&err));

        // 有凭证但 ttl=0（立即过期）→ 仍 CredentialExpired（不发网络）
        store
            .upsert_credential(
                "BIZ==",
                &CredentialFields {
                    key: Some("K".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let err2 = collect_list(&store, "BIZ==", &ListPlan::first_page(), 0, None)
            .await
            .unwrap_err();
        assert!(is_credential_expired_err(&err2));

        // 实测失效打点后：TTL 未到也判过期（不发网络）
        store.mark_credential_invalid("BIZ==").unwrap();
        let err3 = collect_list(&store, "BIZ==", &ListPlan::first_page(), 1800, None)
            .await
            .unwrap_err();
        assert!(is_credential_expired_err(&err3));
    }

    // -- 翻页逻辑：本地 HTTP mock 当 getmsg（经 `wrap` 把请求指到本机），脱微信验证 --

    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// 一页 getmsg 响应：`items` = (mid, datetime)；`next` = Some(下一页 offset) / None=到底。
    fn page(items: &[(i64, i64)], next: Option<i64>) -> String {
        page_with(items, if next.is_some() { 1 } else { 0 }, next.unwrap_or(0))
    }

    /// 同 [`page`]，但 `can_msg_continue` / `next_offset` 各自指定（模拟真机"标 0 但 offset 前进"）。
    fn page_with(items: &[(i64, i64)], can_continue: i64, next_offset: i64) -> String {
        let list: Vec<serde_json::Value> = items
            .iter()
            .map(|(mid, dt)| {
                serde_json::json!({
                    "comm_msg_info": {"id": mid, "datetime": dt},
                    "app_msg_ext_info": {
                        "title": format!("t{mid}"),
                        "content_url": format!("https://mp.weixin.qq.com/s?__biz=AAA==&mid={mid}&idx=1&sn=s{mid}")
                    }
                })
            })
            .collect();
        serde_json::json!({
            "ret": 0,
            "list": list,
            "can_msg_continue": can_continue,
            "next_offset": next_offset,
        })
        .to_string()
    }

    /// 起一个 mock：按目标 URL 里的 offset 回页；记录收到的 offset 序列。
    async fn spawn_getmsg_mock(
        pages: Arc<dyn Fn(i64) -> String + Send + Sync>,
    ) -> (SocketAddr, Arc<Mutex<Vec<i64>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::<i64>::new()));
        let seen2 = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let pages = pages.clone();
                let seen = seen2.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    let head = String::from_utf8_lossy(&buf[..n]).to_string();
                    let path = head
                        .lines()
                        .next()
                        .unwrap_or("")
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or("/")
                        .to_string();
                    // /?url=<pct-encoded 目标>
                    let target = path
                        .split_once("url=")
                        .map(|(_, v)| v.split('&').next().unwrap_or(v).to_string())
                        .unwrap_or_default();
                    let target = percent_encoding::percent_decode_str(&target)
                        .decode_utf8_lossy()
                        .to_string();
                    let offset: i64 = target
                        .split('?')
                        .nth(1)
                        .unwrap_or("")
                        .split('&')
                        .find_map(|kv| kv.strip_prefix("offset=").and_then(|v| v.parse().ok()))
                        .unwrap_or(-1);
                    seen.lock().unwrap().push(offset);
                    let body = pages(offset);
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                    let _ = stream.flush().await;
                });
            }
        });
        (addr, seen)
    }

    fn wrapper(addr: SocketAddr) -> impl Fn(&str) -> String + Send + Sync {
        move |u: &str| {
            format!(
                "http://{addr}/?url={}",
                percent_encoding::utf8_percent_encode(u, percent_encoding::NON_ALPHANUMERIC)
            )
        }
    }

    fn fresh_store() -> Store {
        let store = Store::open_in_memory().unwrap();
        store
            .upsert_credential(
                "AAA==",
                &CredentialFields {
                    uin: Some("U".into()),
                    key: Some("K".into()),
                    pass_ticket: Some("P".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        store
    }

    fn plan(since: Option<f64>, start: i64, max_pages: usize) -> ListPlan {
        ListPlan {
            since,
            start_offset: start,
            count: 2,
            max_pages,
            page_sleep_min_ms: 0,
            page_sleep_max_ms: 0,
            rate_retry_wait_ms: 0,
            gap_min_ms: 0,
            gap_max_ms: 0,
            source: ListSource::Sweep,
            paginate: false,
        }
    }

    /// 三页历史：offset0 = (1,1000),(2,900)；offset10 = (3,800),(4,400)；offset20 = (5,300)。
    fn three_pages() -> Arc<dyn Fn(i64) -> String + Send + Sync> {
        Arc::new(|off| match off {
            0 => page(&[(1, 1000), (2, 900)], Some(10)),
            10 => page(&[(3, 800), (4, 400)], Some(20)),
            _ => page(&[(5, 300)], None),
        })
    }

    #[tokio::test]
    async fn test_collect_list_no_since_only_first_page() {
        let store = fresh_store();
        let (addr, seen) = spawn_getmsg_mock(three_pages()).await;
        let w = wrapper(addr);
        let out = collect_list(&store, "AAA==", &plan(None, 0, 50), 1800, Some(&w))
            .await
            .unwrap();
        assert_eq!(out.pages, 1);
        assert_eq!(out.total, 2);
        assert_eq!(out.urls.len(), 2);
        assert!(out.is_done() && out.reached_since && !out.cred_expired);
        assert_eq!(*seen.lock().unwrap(), vec![0]);
        // 使用打点
        assert_eq!(store.get_credential("AAA==").unwrap().unwrap().use_count, 1);
    }

    /// 静默空页（账号软限制）：首采首页 ret=0 却没有 general_msg_list → 记 `suspect_blocked`、
    /// `list_call_log` 记 `empty_suspect`、日志走 warn；而带列表字段的空页仍是正常「没有更多」。
    #[tokio::test]
    async fn test_collect_list_flags_silent_empty_page() {
        let store = fresh_store();
        // 真机形态：既不报错也不给列表
        let silent: Arc<dyn Fn(i64) -> String + Send + Sync> = Arc::new(|_| {
            serde_json::json!({
                "ret": 0, "errmsg": "ok", "msg_count": 0,
                "can_msg_continue": 0, "home_page_list": []
            })
            .to_string()
        });
        let (addr, _) = spawn_getmsg_mock(silent).await;
        let w = wrapper(addr);
        let out = collect_list(&store, "AAA==", &plan(None, 0, 50), 1800, Some(&w))
            .await
            .unwrap();
        assert_eq!(out.total, 0);
        assert!(out.suspect_blocked, "首页静默空页应标记疑似受限");
        let rows = store.list_calls_since(0.0, 10).unwrap();
        assert!(
            rows.iter().any(|r| r.outcome == "empty_suspect"),
            "留档 outcome 应为 empty_suspect，实际 {:?}",
            rows.iter().map(|r| r.outcome.clone()).collect::<Vec<_>>()
        );

        // 对照：同样 0 篇，但微信给了空列表字段 = 正常「没有更多」，不报警
        let store2 = fresh_store();
        let empty_list: Arc<dyn Fn(i64) -> String + Send + Sync> = Arc::new(|_| page(&[], None));
        let (addr2, _) = spawn_getmsg_mock(empty_list).await;
        let w2 = wrapper(addr2);
        let out2 = collect_list(&store2, "AAA==", &plan(None, 0, 50), 1800, Some(&w2))
            .await
            .unwrap();
        assert_eq!(out2.total, 0);
        assert!(!out2.suspect_blocked, "有列表字段的空页不是账号受限");
    }

    #[tokio::test]
    async fn test_collect_list_since_pages_until_older_article() {
        let store = fresh_store();
        let (addr, seen) = spawn_getmsg_mock(three_pages()).await;
        let w = wrapper(addr);
        // since=500：第 1 页全部 ≥500 → 翻；第 2 页出现 400 < 500 → 停（该页仍入库；早于的不回报），不翻第 3 页
        let out = collect_list(&store, "AAA==", &plan(Some(500.0), 0, 50), 1800, Some(&w))
            .await
            .unwrap();
        assert_eq!(out.pages, 2);
        assert_eq!(out.total, 4);
        assert_eq!(
            out.urls.len(),
            3,
            "第 2 页里早于 since 的那篇（mid=4）只入库、不回报"
        );
        assert!(out.urls.iter().all(|u| !u.contains("mid=4")));
        assert!(out.urls[2].contains("mid=3"));
        assert_eq!(out.skipped_older, 1);
        assert!(out.reached_since && out.is_done());
        assert_eq!(*seen.lock().unwrap(), vec![0, 10]);
        assert_eq!(store.count_articles(Some("AAA==")).unwrap(), 4);
    }

    #[tokio::test]
    async fn test_collect_list_ignores_can_msg_continue_zero_while_offset_advances() {
        // 真机形状：首页 can_msg_continue=0 但 next_offset=10 → 继续探；第 2 页仍标 0、offset 前进 → 继续；
        // 第 3 页出现早于 since 的文章 → 停。旧逻辑首页即停、漏掉全部历史。
        let store = fresh_store();
        let pages: Arc<dyn Fn(i64) -> String + Send + Sync> = Arc::new(|off| match off {
            0 => page_with(&[(1, 1000), (2, 900)], 0, 10),
            10 => page_with(&[(3, 800), (4, 700)], 0, 20),
            _ => page_with(&[(5, 600), (6, 400)], 0, 30),
        });
        let (addr, seen) = spawn_getmsg_mock(pages).await;
        let w = wrapper(addr);
        let out = collect_list(&store, "AAA==", &plan(Some(500.0), 0, 50), 1800, Some(&w))
            .await
            .unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![0, 10, 20]);
        assert_eq!(out.pages, 3);
        assert_eq!(out.total, 6);
        assert_eq!(out.urls.len(), 5, "mid=6 早于 since，只入库不回报");
        assert!(out.reached_since && out.is_done());

        // 标 0 且 next_offset 不前进 → 到底
        let store2 = fresh_store();
        let pages2: Arc<dyn Fn(i64) -> String + Send + Sync> = Arc::new(|off| match off {
            0 => page_with(&[(1, 1000)], 0, 10),
            _ => page_with(&[(2, 900)], 0, 10),
        });
        let (addr2, seen2) = spawn_getmsg_mock(pages2).await;
        let w2 = wrapper(addr2);
        let out2 = collect_list(&store2, "AAA==", &plan(Some(500.0), 0, 50), 1800, Some(&w2))
            .await
            .unwrap();
        assert_eq!(*seen2.lock().unwrap(), vec![0, 10]);
        assert_eq!(out2.pages, 2);
        assert!(out2.reached_since && out2.is_done());
    }

    #[tokio::test]
    async fn test_collect_list_since_reaches_end_of_history() {
        let store = fresh_store();
        let (addr, seen) = spawn_getmsg_mock(three_pages()).await;
        let w = wrapper(addr);
        // since=100：没有更早的 → 翻到 can_msg_continue=0 为止
        let out = collect_list(&store, "AAA==", &plan(Some(100.0), 0, 50), 1800, Some(&w))
            .await
            .unwrap();
        assert_eq!(out.pages, 3);
        assert_eq!(out.total, 5);
        assert!(out.reached_since && out.is_done());
        assert_eq!(*seen.lock().unwrap(), vec![0, 10, 20]);
    }

    #[tokio::test]
    async fn test_collect_list_max_pages_returns_next_offset() {
        let store = fresh_store();
        let (addr, _) = spawn_getmsg_mock(three_pages()).await;
        let w = wrapper(addr);
        let out = collect_list(&store, "AAA==", &plan(Some(100.0), 0, 1), 1800, Some(&w))
            .await
            .unwrap();
        assert_eq!(out.pages, 1);
        assert_eq!(out.next_offset, Some(10));
        assert!(!out.cred_expired && !out.is_done());
    }

    #[tokio::test]
    async fn test_collect_list_expired_midway_keeps_partial_and_resumes() {
        let store = fresh_store();
        let pages: Arc<dyn Fn(i64) -> String + Send + Sync> = Arc::new(|off| match off {
            0 => page(&[(1, 1000), (2, 900)], Some(10)),
            10 => r#"{"ret":-3,"errmsg":"no session"}"#.to_string(),
            _ => page(&[(5, 300)], None),
        });
        let (addr, seen) = spawn_getmsg_mock(pages).await;
        let w = wrapper(addr);
        let out = collect_list(&store, "AAA==", &plan(Some(100.0), 0, 50), 1800, Some(&w))
            .await
            .unwrap();
        // 第 1 页保留，第 2 页过期：带回 next_offset=10，热表实测失效打点
        assert!(out.cred_expired);
        assert_eq!(out.next_offset, Some(10));
        assert_eq!(out.urls.len(), 2);
        assert_eq!(out.pages, 1);
        assert!(
            !store.credential_is_fresh("AAA==", 1800).unwrap(),
            "ret=-3 后应立即判不新鲜"
        );
        assert!(store
            .get_credential("AAA==")
            .unwrap()
            .unwrap()
            .invalidated_at
            .is_some());
        assert_eq!(*seen.lock().unwrap(), vec![0, 10]);

        // 续期（换新 key）后从 offset=10 续采：mock 让 10 也过期，所以改用 20 起点验证续采路径
        store
            .upsert_credential(
                "AAA==",
                &CredentialFields {
                    key: Some("K2".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(store.credential_is_fresh("AAA==", 1800).unwrap());
        let out2 = collect_list(&store, "AAA==", &plan(Some(100.0), 20, 50), 1800, Some(&w))
            .await
            .unwrap();
        assert_eq!(out2.pages, 1);
        assert_eq!(out2.urls.len(), 1);
        assert!(out2.is_done());
        assert_eq!(*seen.lock().unwrap(), vec![0, 10, 20]);
    }

    /// ret=-6（unknown error）= 微信号被限制：立即报 `AccountBlocked`，不重试、不打凭证失效点，
    /// 且每次真正发出的 getmsg 都计入今日计数。
    #[tokio::test]
    async fn test_collect_list_ret_minus_6_is_account_blocked() {
        let store = fresh_store();
        let pages: Arc<dyn Fn(i64) -> String + Send + Sync> = Arc::new(|_off| {
            r#"{"ret":-6,"errmsg":"unknown error","home_page_list":[]}"#.to_string()
        });
        let (addr, seen) = spawn_getmsg_mock(pages).await;
        let w = wrapper(addr);
        let before = store.list_calls_today().unwrap();
        let err = collect_list(&store, "AAA==", &plan(Some(100.0), 0, 50), 1800, Some(&w))
            .await
            .unwrap_err();
        assert!(is_account_blocked_err(&err), "{err}");
        assert!(!is_rate_limited_err(&err));
        assert!(!is_credential_expired_err(&err));
        assert_eq!(*seen.lock().unwrap(), vec![0], "只发一次，不重试");
        assert!(
            store.credential_is_fresh("AAA==", 1800).unwrap(),
            "-6 不是凭证过期，不该打失效点"
        );
        assert_eq!(store.list_calls_today().unwrap(), before + 1);
    }

    #[test]
    fn test_list_plan_defaults() {
        let p = ListPlan::first_page();
        assert_eq!(p.since, None);
        assert_eq!(p.start_offset, 0);
        assert!(p.page_sleep_max_ms >= p.page_sleep_min_ms);
        let o = CollectOutcome::default();
        assert!(o.is_done());
    }
}
