//! 数据模型 —— 对齐 Python 版 SQLite schema 的行结构（store.py 的 SCHEMA）。
//!
//! 这些结构既用于 store 读出的行，也用于 wechat 解析的产物，保持字段名与
//! Python dict 键一致，便于对拍与迁移。

use serde::{Deserialize, Serialize};

/// 时间戳类型：与 Python `time.time()` 一致，用 epoch 秒（f64）。
pub type Epoch = f64;

/// 当前时间（epoch 秒），对齐 Python `time.time()`。
pub fn now() -> Epoch {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// 公众号（accounts 表）。`biz`（__biz）是稳定主键。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub biz: String,
    pub nickname: Option<String>,
    pub round_head_img: Option<String>,
    pub first_seen: Epoch,
    pub last_seen: Epoch,
    pub focus: i64,
    /// 定时巡检记下的「该号最新一篇文章的发布时间」（epoch 秒）：每轮巡检成功后写入，下一轮
    /// 作为翻页的 `last_published_at`。首轮 / 从未巡检成功过的号为 `None`（巡检时退回
    /// `articles` 表 MAX(published_at)，再没有就只采第 1 页）。（Rust 版新增，Python 无对应）
    #[serde(default)]
    pub last_published_at: Option<Epoch>,
    /// 最近一次巡检处理（成功 / 判定失败 / 无法续期跳过）的时刻。
    #[serde(default)]
    pub sweep_checked_at: Option<Epoch>,
    /// 最近一次巡检结果：`ok` / `failed` / `no_sample`（库里没有可用长链，无法续期）。
    #[serde(default)]
    pub sweep_status: Option<String>,
    /// 最近一次巡检失败 / 跳过的原因。
    #[serde(default)]
    pub sweep_error: Option<String>,
    /// 是否参与巡检（1 = 参与，默认；0 = 用户在列表里排除）。
    #[serde(default = "default_sweep_enabled")]
    pub sweep_enabled: i64,
    /// 本轮内已尝试次数（失败一次 +1，成功 / 判定失败归零）。
    #[serde(default)]
    pub sweep_attempts: i64,
    /// 种子链接：「批量添加」时粘贴的那条文章短链。库里还没有该号的文章时，巡检 / 历史抓取 /
    /// 重新巡检用它接力换凭证（见 `Store::latest_article_urls`）。
    #[serde(default)]
    pub seed_url: Option<String>,
}

fn default_sweep_enabled() -> i64 {
    1
}

/// 微信号（登录微信客户端的账号）。采集同一时刻只用一个（`is_active`）；`uin` 在首次接力抓到凭证时绑定，
/// 之后抓到不同的 `uin` 视为「登录的微信号与激活的不一致」。种子标定 / 24 小时预算 / 封号退避都按它区分。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WxAccount {
    pub id: i64,
    /// 别名（登记时默认「微信号 N」，用户可改）。
    pub alias: String,
    /// 微信用户 ID：登记即绑定，必填且唯一。
    pub uin: String,
    pub is_active: bool,
    pub created_at: Epoch,
    /// 最近一次抓到凭证。
    pub last_captured_at: Option<Epoch>,
    /// `ret=-6` 封号退避截止；`None` = 未封。
    pub blocked_until: Option<Epoch>,
    pub blocked_reason: Option<String>,
    pub note: Option<String>,
}

/// 抓到凭证里的 `uin` 登记 / 比对的结果（`Store::wx_note_captured_uin`）。微信号没有手动新增入口，
/// 全靠这里按 `uin` 自动登记。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WxBind {
    /// 这个 uin 刚登记（新建记录，或原有记录在没有激活号时被启用）。`activated=true` = 当时没有激活号，
    /// 已顺带把它设为当前；`false` = 已有别的激活号，调用方按 [`WxBind::Mismatch`] 处理。
    Registered { id: i64, activated: bool },
    /// uin 属于当前激活号。
    Same,
    /// uin 属于另一条已登记的记录（`captured_alias`），而激活的是 `alias`：不该入库、应中止本批。
    Mismatch {
        alias: String,
        captured_alias: String,
    },
}

/// 凭证池（credentials 表，**热数据**：每号一行、始终是最新一份）。key/uin/pass_ticket 等约 30 分钟过期。
///
/// 与 Python 的差异：多了有效期/失效/使用统计几列（`expires_at`/`invalidated_at`/`last_used_at`/
/// `use_count`/`refresh_count`），用于**提前发现**凭证过期并统计真实有效时长；每次抓到的凭证
/// 同时追加进 `credential_log`（见 [`CredentialLogRow`]）留档，供后期经代理池独立回放。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Credential {
    pub biz: String,
    pub uin: Option<String>,
    pub key: Option<String>,
    pub pass_ticket: Option<String>,
    pub wxtoken: Option<String>,
    pub x5: Option<String>,
    pub appmsg_token: Option<String>,
    pub cookie: Option<String>,
    /// 其余参数（JSON 字符串，原样保存，对齐 Python 的 extra 列）。
    pub extra: Option<String>,
    pub captured_at: Epoch,
    /// 预估过期时刻（`captured_at + cred_ttl`），用于提前判"即将过期"。
    #[serde(default)]
    pub expires_at: Option<Epoch>,
    /// 实测失效时刻（回放拿到 `ret=-3` no session 时打点）；`>= captured_at` 即当前 key 已失效。
    #[serde(default)]
    pub invalidated_at: Option<Epoch>,
    /// 最近一次拿它回放接口的时刻。
    #[serde(default)]
    pub last_used_at: Option<Epoch>,
    /// 累计回放次数。
    #[serde(default)]
    pub use_count: i64,
    /// 累计换 key 次数（同号 key 变化即 +1）。
    #[serde(default)]
    pub refresh_count: i64,
}

impl Credential {
    /// 距预估过期还有多少秒（负数=已过期；无 `expires_at` 时按 `ttl` 由 `captured_at` 推算）。
    pub fn secs_to_expiry(&self, now: Epoch, ttl_secs: i64) -> f64 {
        let exp = self
            .expires_at
            .unwrap_or(self.captured_at + ttl_secs as f64);
        exp - now
    }
}

/// `app_log` 一行（环节日志入库行，见 [`crate::applog`]）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppLogRow {
    pub id: i64,
    /// epoch 秒。
    pub ts: Epoch,
    /// `info` / `warn` / `error`。
    pub level: String,
    /// 环节 code（`app` / `task` / `proxy` / `rpa` / `capture` / `list` / `refresh` / `report` / `detail` / `sweep`）。
    pub stage: String,
    pub job_id: Option<i64>,
    pub message: String,
}

/// `list_call_log` 一行：**每一次真实发出的 `getmsg` 请求**（限流分析的原始材料，Rust 版新增，Python 无对应）。
///
/// 只记标量（时刻 / 号 / 来源 / 页号 / 结果 / 耗时 / 篇数），不含接口 URL 与凭证参数。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ListCallRow {
    pub id: i64,
    /// 请求发出时刻（epoch 秒）。
    pub ts: Epoch,
    pub biz: String,
    /// 请求来源：`sweep`（定时巡检）/ `retry`（单号手动重新巡检）/ `history`（历史抓取）；老行可能是 `manual`。
    pub source: String,
    /// 所属本地 job（手动任务 / 巡检批次都有；手动重试为 `None`）。
    pub job_id: Option<i64>,
    /// 页号（首页 = 1）。
    pub page: i64,
    /// 结果：`ok`（含空页）/ `expired`（ret=-3 凭证过期）/ `blocked`（ret=-6/-12 微信号被限制）/
    /// `rate_limited`（HTTP 429 / 5xx / 接口回网页）/ `error`（其它错误或非 0 ret）。
    pub outcome: String,
    /// 请求往返耗时（毫秒）。
    pub latency_ms: i64,
    /// 本页解析出的篇数 / 其中新入库篇数（仅 `ok`）。
    pub articles: i64,
    pub new_articles: i64,
    /// 备注（错误摘要，已脱敏）。
    pub note: Option<String>,
    /// 影响因素（2026-09-07 加，用于「限流与代理 / 微信号是否相关」的对照分析）。
    #[serde(flatten)]
    pub factors: ListCallFactors,
}

/// 每次 `getmsg` 请求当时的**影响因素**（`list_call_log` 的因素列；2026-09-07 加）。
///
/// 目的：跑一段时间后能回答「被限流 / 封禁与出口代理、微信号、凭证、请求节奏哪个有关」。
/// 全是标量或不可逆短哈希：**不含** uin / key 原文。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ListCallFactors {
    /// 出口代理名称（GUI「代理池」里选用的那条）；`None` = 直连。
    #[serde(default)]
    pub proxy: Option<String>,
    /// 微信号标识：凭证 `uin` 的短哈希（同一微信号登录抓到的所有凭证 uin 相同），用于区分新旧号。
    #[serde(default)]
    pub uin_hash: Option<String>,
    /// 凭证标识：`key` 的短哈希，用于数「一把 key 用了多少次 / 多久后失效」。
    #[serde(default)]
    pub key_hash: Option<String>,
    /// 凭证年龄：请求时距该 key 抓到时刻的秒数。
    #[serde(default)]
    pub cred_age_s: Option<i64>,
    /// 微信返回的 `ret`（0 正常 / -3 过期 / -6 封号 …）；传输层失败为 `None`。
    #[serde(default)]
    pub ret: Option<i64>,
    /// HTTP 状态码；连不上为 `None`。
    #[serde(default)]
    pub http_status: Option<i64>,
    /// 本页请求的 `offset`。
    #[serde(default)]
    pub offset: Option<i64>,
    /// 与上一次 `getmsg`（跨号 / 跨任务源，进程级闸门记的）之间的实际间隔（毫秒）；进程内第一次为 `None`。
    #[serde(default)]
    pub gap_ms: Option<i64>,
    /// 当时配置的闸门下 / 上限（毫秒），实验中改过节流参数时用它分段。
    #[serde(default)]
    pub gate_min_ms: Option<i64>,
    #[serde(default)]
    pub gate_max_ms: Option<i64>,
    /// 本次是当日第几次 `getmsg`（`config.list_calls_count`）。
    #[serde(default)]
    pub day_seq: Option<i64>,
}

/// 写入 `list_call_log` 的输入（见 [`ListCallRow`] 各字段说明）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ListCallEvent {
    pub biz: String,
    pub source: String,
    pub job_id: Option<i64>,
    pub page: i64,
    pub outcome: String,
    pub latency_ms: i64,
    pub articles: i64,
    pub new_articles: i64,
    pub note: Option<String>,
    pub factors: ListCallFactors,
}

/// 不可逆短哈希（FNV-1a 64 位，16 位十六进制）：给 uin / key 这类敏感值做**标识**用，只用来
/// 区分「是不是同一个」，不用于安全目的。空串返回 `None`。
pub fn short_hash(s: &str) -> Option<String> {
    if s.is_empty() {
        return None;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    Some(format!("{h:016x}"))
}

/// `run_log` 一行：一个**执行单元**（手动任务 / 巡检批次）的结果摘要（限流分析「任务执行频率 / 轮次」用）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunLogRow {
    pub id: i64,
    /// 本地 jobs 表 id。
    pub job_id: Option<i64>,
    /// `sweep`（老行可能是 `manual` / `upstream`，已下线的手动任务留档）。
    pub kind: String,
    /// 巡检轮次号（跨重启单调递增，`config.sweep_pass_seq`）；手动任务为 `None`。
    pub pass_no: Option<i64>,
    pub started_at: Epoch,
    pub finished_at: Epoch,
    /// 本单元涉及的号数。
    pub accounts: i64,
    /// 成功 / 判定失败 / 本轮稍后重试 / 本批未轮到（巡检语义；手动任务只用 ok / failed）。
    pub ok: i64,
    pub failed: i64,
    pub retry: i64,
    pub deferred: i64,
    /// 新入库文章数 / 回报（采到）的链接数。
    pub new_articles: i64,
    pub urls: i64,
    /// 本单元发出的 `getmsg` 次数。
    pub list_calls: i64,
    /// 本单元撞到限流 / 账号级封禁 / 验证页 / 环境故障 / 没跑完。
    pub rate_limited: bool,
    pub blocked: bool,
    pub verify_hit: bool,
    pub env_failure: bool,
    pub truncated: bool,
    /// 备注（中止原因 / 批次小结）。
    pub note: Option<String>,
}

/// 写入 `run_log` 的输入（字段同 [`RunLogRow`]，无 id）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunLogEvent {
    pub job_id: Option<i64>,
    pub kind: String,
    pub pass_no: Option<i64>,
    pub started_at: Epoch,
    pub finished_at: Epoch,
    pub accounts: i64,
    pub ok: i64,
    pub failed: i64,
    pub retry: i64,
    pub deferred: i64,
    pub new_articles: i64,
    pub urls: i64,
    pub list_calls: i64,
    pub rate_limited: bool,
    pub blocked: bool,
    pub verify_hit: bool,
    pub env_failure: bool,
    pub truncated: bool,
    pub note: Option<String>,
}

/// `cooldown_log` 一行：整机退避的触发 / 解除记录。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CooldownLogRow {
    pub id: i64,
    pub ts: Epoch,
    /// `ip`（429 / 5xx / 验证页，分钟级阶梯）/ `account`（ret=-6/-12，小时级阶梯）/ `clear`（用户手动解除）。
    pub kind: String,
    /// 触发后的退避等级（`clear` 为 0）。
    pub level: i64,
    /// 本次退避时长（秒；`clear` 为 0）。
    pub secs: i64,
    pub reason: String,
}

/// 凭证留档（credential_log 表，**append-only**）：每份抓到的 (biz, key) 一行，记录首次/末次
/// 观测、预估过期、实测失效与使用次数——用来统计真实有效期，也为后期把热凭证交给代理池
/// 独立回放保留材料。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CredentialLogRow {
    pub id: i64,
    pub biz: String,
    pub uin: Option<String>,
    pub key: Option<String>,
    pub pass_ticket: Option<String>,
    pub wxtoken: Option<String>,
    pub x5: Option<String>,
    pub appmsg_token: Option<String>,
    pub cookie: Option<String>,
    pub extra: Option<String>,
    /// 首次观测到这份 key 的时刻（真实寿命的起点）。
    pub captured_at: Epoch,
    /// 最近一次观测到这份 key 的时刻。
    pub last_seen_at: Epoch,
    pub expires_at: Option<Epoch>,
    pub invalidated_at: Option<Epoch>,
    pub last_used_at: Option<Epoch>,
    pub use_count: i64,
}

/// 写入凭证时的输入字段集合（对齐 Python `upsert_credential(**fields)`）。
///
/// 语义：只有 `Some(非空)` 的字段会写入；`None` 用 SQL 的 `COALESCE` 保留旧值，
/// 因此“占位空值”不会把此前抓到的真实凭证清掉。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CredentialFields {
    pub uin: Option<String>,
    pub key: Option<String>,
    pub pass_ticket: Option<String>,
    pub wxtoken: Option<String>,
    pub x5: Option<String>,
    pub appmsg_token: Option<String>,
    pub cookie: Option<String>,
    /// 额外字段（会被序列化成 JSON 存入 extra 列）。
    pub extra: Option<serde_json::Value>,
}

/// 文章（articles 表）。列表采集只带基础字段，详情/转赞评后续补齐。
///
/// 这里用一个宽结构承载 upsert 的可选字段；未提供的字段保持 `None`，
/// 不会覆盖库里已有值（对齐 Python `upsert_article` 的“非空才写”语义）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ArticleFields {
    pub sn: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub digest: Option<String>,
    pub content_url: Option<String>,
    pub cover: Option<String>,
    pub published_at: Option<Epoch>,
    pub content_html: Option<String>,
    pub content_text: Option<String>,
    pub content_md: Option<String>,
    pub read_num: Option<i64>,
    pub old_like_num: Option<i64>,
    pub like_num: Option<i64>,
    pub comment_count: Option<i64>,
    pub is_deleted: Option<i64>,
    pub detail_done: Option<i64>,
    pub stat_done: Option<i64>,
}

/// 文章列表行（`list_articles` 返回的精简投影，对齐 Python 的列裁剪）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ArticleRow {
    pub id: i64,
    pub biz: String,
    pub mid: Option<String>,
    pub idx: Option<i64>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub digest: Option<String>,
    pub content_url: Option<String>,
    pub published_at: Option<Epoch>,
    pub detail_done: i64,
    /// 最近一次补详情失败的原因（不可用页文案 / 抓取错误；成功后清空）。
    pub detail_error: Option<String>,
    pub is_deleted: i64,
}

/// wechat_api.parse_msg_list 解析出的单篇文章（对齐 Python 的 dict 字段）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ParsedArticle {
    pub mid: String,
    pub idx: i64,
    pub sn: Option<String>,
    pub title: Option<String>,
    pub digest: Option<String>,
    pub content_url: Option<String>,
    pub author: Option<String>,
    pub cover: Option<String>,
    /// 发布时间（epoch），来自 comm_msg_info.datetime。
    pub published_at: Option<i64>,
}

/// 一批采集任务（jobs 表一行）：手动添加的公众号链接（`kind = Manual`）或定时巡检合成的批次（`Sweep`）。
///
/// **最后发布时间**（内部字段沿用 `last_updated_at`，epoch 秒）有两级：任务级 [`Job::last_updated_at`]
/// 对全部链接兜底；链接级 [`Job::link_since`]（按 URL 索引）优先。某号取不到任何一级 → 只采第 1 页；
/// 取到 → 翻页直到某页出现发布时间**早于**它的文章为止（见 `collector::collect_list`）。
///
/// 每个号列表采完就当场上报（`report.rs`，未启用上报则跳过）；列表没正常获取到的号不报。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub links: Vec<String>,
    /// 本地 jobs 表 id：已落库的任务带上它，`process_job` 直接复用该行；`None`（巡检合成 / 单测）则
    /// `process_job` 入口新建一行。
    #[serde(default)]
    pub local_id: Option<i64>,
    /// 任务来源（现在只有巡检）。
    #[serde(default)]
    pub kind: JobKind,
    /// 任务级「最后发布时间」（epoch 秒），默认空 = 只采第 1 页。
    #[serde(default)]
    pub last_updated_at: Option<Epoch>,
    /// 链接级「最后发布时间」（URL → epoch 秒），优先于任务级。
    #[serde(default)]
    pub link_since: std::collections::HashMap<String, Epoch>,
}

/// 任务来源。现在只有巡检一种（批次 / 单号重新巡检 / 历史抓取换凭证都走它）；老库里的
/// `manual` / `upstream` 行只作留档展示，不再被领取。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobKind {
    /// 本地定时巡检合成（结果由 `sweep` 模块写回 `accounts`）。
    #[default]
    Sweep,
}

impl JobKind {
    /// 入库 / 上报用的 code。
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::Sweep => "sweep",
        }
    }
}

impl Job {
    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }

    /// 某条任务链接生效的「最后更新时间」：链接级优先，其次任务级；都没有 → `None`（只采第 1 页）。
    pub fn since_for_link(&self, url: &str) -> Option<Epoch> {
        self.link_since.get(url).copied().or(self.last_updated_at)
    }
}

/// 一条任务里**一个公众号**的采集结果——上报的最小单位。落 `jobs.result_json` 的 `accounts[]`。
///
/// 该号列表**正常获取完**（`finished=true`）时**当场**上报一条，不等整批；按时间比对后没有新文章也报
/// 空数组。列表**没正常获取到**的号（没打开 / 没凭证 / 续期失败 / 限流 / 异常，`finished=false`）**不上报**。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AccountReport {
    /// 解析 / 接力登记到的 `__biz`（短链没打开过则 `None`）。
    #[serde(default)]
    pub biz: Option<String>,
    /// 该号对应的任务链接（同一号多条链接时合并）。
    #[serde(default)]
    pub links: Vec<String>,
    /// 本次为该号采到的文章链接（按页顺序，不去重；只含发布时间 ≥ 最后发布时间的）。
    #[serde(default)]
    pub urls: Vec<String>,
    /// 该号是否采完（翻页到底 / 首页采完）；没采完（没凭证 / 续期失败 / 限流 / 异常）为 false。
    /// **只有采完的号才上报**。
    #[serde(default)]
    pub finished: bool,
    /// 已上报且被接受（2xx）。未启用上报 / 没采完的号恒为 false。
    #[serde(default)]
    pub reported: bool,
    /// 上报失败的原因（非 2xx / 网络错误）；成功、跳过或没上报为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_error: Option<String>,
}

/// jobs 表读出的一条记录（get_job 返回）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct JobRow {
    pub id: i64,
    pub upstream_id: Option<String>,
    pub links: Vec<String>,
    /// 任务级「最后更新时间」（epoch 秒）。
    #[serde(default)]
    pub last_updated_at: Option<Epoch>,
    /// 链接级「最后更新时间」（URL → epoch 秒）。
    #[serde(default)]
    pub link_since: std::collections::HashMap<String, Epoch>,
    pub status: String,
    pub result: Option<serde_json::Value>,
    pub feedback: Option<String>,
    pub received_at: Epoch,
    pub reported_at: Option<Epoch>,
    /// 任务来源 `sweep`（老库里旧行可能是 `manual` / `upstream`，只作留档展示）。
    #[serde(default)]
    pub kind: String,
    /// 历史列（老库旧行的原始报文）；新任务恒为 `None`。
    #[serde(default)]
    pub raw: Option<serde_json::Value>,
    /// 任务开始处理 / 处理结束（含收尾关窗口、停代理）的时刻；耗时 = finished_at - started_at。
    #[serde(default)]
    pub started_at: Option<Epoch>,
    #[serde(default)]
    pub finished_at: Option<Epoch>,
    /// 结果分类：见 [`JOB_OUTCOME_OK`] 等常量；跑完前为 `None`。
    #[serde(default)]
    pub outcome: Option<String>,
    /// 上报得到的 HTTP 状态码（网络错误 / 未上报为 `None`）。
    #[serde(default)]
    pub report_http_status: Option<i64>,
    /// 各阶段耗时（按时间顺序；见 [`crate::phases`]）。跑完前为空。
    #[serde(default)]
    pub phases: Vec<JobPhase>,
}

/// 任务的一个阶段及其耗时（`jobs.phases_json` 数组元素；GUI 耗时列 hover 展示）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobPhase {
    /// 阶段名（中文，如「启动代理」「接力抓凭证」「采集文章列表」「关闭浏览器」）。
    pub name: String,
    /// 耗时（毫秒）。
    pub ms: i64,
}

/// 任务结果分类（`jobs.outcome`，GUI「任务列表」状态列）：正常。
pub const JOB_OUTCOME_OK: &str = "ok";
/// 超时：等凭证到上限仍有链接没打开 / 点空、停滞重拉耗尽 / 整个等待期间没有任何请求（环境故障）。
pub const JOB_OUTCOME_TIMEOUT: &str = "timeout";
/// 服务端错误：采完了但上报失败（非 2xx 或网络错误）。
pub const JOB_OUTCOME_SERVER_ERROR: &str = "server_error";
/// 其它错误：代理起不来 / 验证页 / 限流退避 / 续期失败 / 空任务等，原因见 `feedback`。
pub const JOB_OUTCOME_ERROR: &str = "error";

/// 上报结果（`ReportFn` 的返回值）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReportAck {
    /// 上报服务是否接受（2xx）。
    pub ok: bool,
    /// HTTP 状态码（网络错误 / 跳过为 `None`）。
    pub http_status: Option<u16>,
    /// 失败原因（非 2xx 写 `HTTP 5xx`，网络错误写错误文本）。
    pub error: Option<String>,
    /// 未启用上报、根本没发（不算失败，也不算成功）。
    #[serde(default)]
    pub skipped: bool,
}

impl ReportAck {
    /// 成功（无状态码，测试桩用）。
    pub fn accepted() -> Self {
        Self {
            ok: true,
            http_status: None,
            error: None,
            skipped: false,
        }
    }

    /// 未启用上报：没发。
    pub fn skipped() -> Self {
        Self {
            ok: false,
            http_status: None,
            error: None,
            skipped: true,
        }
    }

    /// 按 HTTP 状态码构造。
    pub fn from_status(status: u16) -> Self {
        let ok = (200..300).contains(&status);
        Self {
            ok,
            http_status: Some(status),
            error: if ok {
                None
            } else {
                Some(format!("HTTP {status}"))
            },
            skipped: false,
        }
    }

    /// 网络错误（没拿到响应）。
    pub fn failed(err: impl std::fmt::Display) -> Self {
        Self {
            ok: false,
            http_status: None,
            error: Some(err.to_string()),
            skipped: false,
        }
    }
}

/// 「任务列表」页的一行（`list_jobs` 返回；不带 links / result / raw 正文，只带计数）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct JobListItem {
    pub id: i64,
    pub upstream_id: Option<String>,
    /// `sweep`（老行可能是 `manual` / `upstream`，已下线的手动任务留档）。
    pub kind: String,
    /// 流转状态 received|capturing|collecting|reported|done|error。
    pub status: String,
    /// 结果分类（跑完前 `None`）。
    pub outcome: Option<String>,
    /// 错误原因（`feedback`）。
    pub error: Option<String>,
    pub received_at: Epoch,
    pub started_at: Option<Epoch>,
    pub finished_at: Option<Epoch>,
    pub reported_at: Option<Epoch>,
    /// 任务链接数 / 回报的文章链接数 / 交回的链接数。
    pub links: i64,
    pub urls: i64,
    pub remaining: i64,
    pub truncated: bool,
    /// 按号视图的计数：任务里的号数 / 已上报（被接受）的号数（`result.accounts[]` 统计；旧行为 0）。
    #[serde(default)]
    pub accounts: i64,
    #[serde(default)]
    pub reported_accounts: i64,
    pub report_http_status: Option<i64>,
    /// 各阶段耗时（耗时列 hover）。
    #[serde(default)]
    pub phases: Vec<JobPhase>,
}

/// 接力队列的一行（relay 表）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RelayRow {
    pub seq: i64,
    pub url: String,
    pub opened: i64,
}

// -----------------------------------------------------------------------------
// 抓取历史文章（按公众号的长任务，一页一个执行单元）
// -----------------------------------------------------------------------------

/// 历史抓取的目标：三项都可空，空即不限；全空 = 翻到底。
/// `count` 按**范围内抓到的**文章计（含库里已有的）；`since_ts` = 翻到早于它的文章即停；
/// `until_ts` = 晚于它的文章跳过不计，只往前翻。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HistoryTarget {
    #[serde(default)]
    pub count: Option<i64>,
    #[serde(default)]
    pub since_ts: Option<Epoch>,
    #[serde(default)]
    pub until_ts: Option<Epoch>,
}

/// 历史抓取任务状态。
pub const HISTORY_RUNNING: &str = "running";
pub const HISTORY_PAUSED: &str = "paused";
pub const HISTORY_DONE: &str = "done";
pub const HISTORY_CANCELLED: &str = "cancelled";
pub const HISTORY_FAILED: &str = "failed";

/// 暂停原因：`budget`（预算 / 保留额度用完，`resume_at` 到点自动继续）、`blocked`（微信号受限）、
/// `credential`（接力换 key 失败，需人工「继续」）、`cooldown`（整机退避）、`user`（手动暂停）、
/// `restart`（应用重启时任务还在跑：重启后不自动抢微信窗口，需人工「继续」）。
pub const HISTORY_PAUSE_BUDGET: &str = "budget";
pub const HISTORY_PAUSE_BLOCKED: &str = "blocked";
pub const HISTORY_PAUSE_CREDENTIAL: &str = "credential";
pub const HISTORY_PAUSE_COOLDOWN: &str = "cooldown";
pub const HISTORY_PAUSE_USER: &str = "user";
pub const HISTORY_PAUSE_RESTART: &str = "restart";

/// `history_jobs` 表一行（进度落库，重启从 `next_offset` 续跑）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HistoryJob {
    pub id: i64,
    pub biz: String,
    pub status: String,
    pub paused_reason: Option<String>,
    pub target_count: Option<i64>,
    pub since_ts: Option<Epoch>,
    pub until_ts: Option<Epoch>,
    /// 任务开始时快照的每页条数 / 页间隔（秒），改设置不影响进行中的任务。
    pub page_count: i64,
    pub gap_secs: i64,
    /// 下一页的 offset（`getmsg` 的 offset 参数）。
    pub next_offset: i64,
    /// 已请求页数 / 已抓到篇数（含范围外）/ 范围内篇数 / 本任务新入库篇数。
    pub pages: i64,
    pub fetched: i64,
    pub matched: i64,
    pub new_articles: i64,
    /// 下一页最早可发的时刻（epoch 秒）。
    pub next_page_at: Option<Epoch>,
    /// 因预算 / 退避 / 受限暂停时的自动恢复时刻；人工暂停 / 凭证失败为 `None`。
    pub resume_at: Option<Epoch>,
    pub started_at: Epoch,
    pub finished_at: Option<Epoch>,
    pub last_error: Option<String>,
    /// 已翻到底（没有更多历史）。
    pub reached_end: bool,
    pub created_at: Epoch,
    /// 连续失败次数（网络 / 接口错误；成功一页归零，达到上限记 failed）。
    #[serde(skip)]
    pub errors: i64,
    /// 本任务累计范围内的文章（上报载荷用；JSON 数组，见 [`crate::report::ReportArticle`]）。
    #[serde(skip)]
    pub articles_json: String,
}

impl HistoryJob {
    pub fn target(&self) -> HistoryTarget {
        HistoryTarget {
            count: self.target_count,
            since_ts: self.since_ts,
            until_ts: self.until_ts,
        }
    }

    /// 仍在进行（running / paused）。
    pub fn is_active(&self) -> bool {
        self.status == HISTORY_RUNNING || self.status == HISTORY_PAUSED
    }
}

/// 历史抓取任务（GUI 视图：多带昵称与目标对象）。
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct HistoryJobView {
    pub id: i64,
    pub biz: String,
    pub nickname: String,
    pub status: String,
    pub paused_reason: Option<String>,
    pub target: HistoryTarget,
    pub page_count: i64,
    pub gap_secs: u64,
    pub pages: i64,
    pub fetched: i64,
    pub matched: i64,
    pub new_articles: i64,
    pub next_offset: i64,
    pub next_page_at: Option<Epoch>,
    pub resume_at: Option<Epoch>,
    pub started_at: Epoch,
    pub finished_at: Option<Epoch>,
    pub last_error: Option<String>,
    pub reached_end: bool,
}

/// 状态栏 / 公众号列表轮询用：当前活动任务 + 当前微信号预算。
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct HistoryStatus {
    pub job: Option<HistoryJobView>,
    pub budget_used: i64,
    pub budget: i64,
    pub budget_reserve: i64,
}

/// 开始前的估算（对话框显示）。
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct HistoryEstimate {
    /// 预计请求页数（0 = 不限 / 无法估算）。
    pub pages: i64,
    /// 预计耗时（秒；`pages=0` 时为 0）。
    pub seconds: i64,
    /// 当前微信号可用于历史抓取的剩余预算（预算 − 已用 − 保留；预算不限时为 -1）。
    pub budget_available: i64,
    pub exceeds_budget: bool,
    pub note: String,
}

/// 全局提醒（任何页面都要看到的一条）：预算达到上限 / 微信号受限 / 历史任务暂停。
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Alert {
    pub id: i64,
    /// `budget` / `blocked` / `history_paused`。
    pub kind: String,
    pub message: String,
    pub at: Epoch,
}
