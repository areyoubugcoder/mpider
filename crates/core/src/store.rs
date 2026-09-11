//! SQLite 存储层（rusqlite, bundled）—— 对齐 Python 原型的 schema 与 API 语义。
//!
//! 起步用 rusqlite（编译快、无 DATABASE_URL 摩擦、语义贴近 Python 的 sqlite3）。
//! 表结构与 SQL 与 Python 版逐条对齐；`init_db` 幂等。后续若要异步/连接池可平滑换成
//! sqlx（保持同一套建表 SQL 与列名即可）。
//!
//! 线程安全：把 `Connection` 包在 `Mutex` 里，`Store` 因此 `Send + Sync`，可用 `Arc<Store>`
//! 在 MITM 代理的多个 tokio 任务间共享（SQLite 调用是阻塞的，抓凭证路径调用短、可接受；
//! 若未来吞吐变大，可在异步侧改走 `spawn_blocking`）。

use std::path::Path;
use std::sync::Mutex;

use anyhow::Result;
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde_json::Value;

use crate::model::{
    now, Account, AppLogRow, ArticleFields, ArticleRow, CooldownLogRow, Credential,
    CredentialFields, CredentialLogRow, Epoch, HistoryJob, JobListItem, JobPhase, JobRow,
    ListCallEvent, ListCallFactors, ListCallRow, RunLogEvent, RunLogRow, WxAccount, WxBind,
};

/// 限流分析日志（`list_call_log` / `run_log` / `cooldown_log`）保留时长：30 天。
pub const RATELIMIT_LOG_RETENTION_SECS: i64 = 30 * 86_400;
/// 限流分析日志行数安全阀（每次请求一行的 `list_call_log` 最多；其余两表更少）。
pub const RATELIMIT_LOG_MAX_ROWS: i64 = 200_000;

/// 凭证默认 TTL（秒）：`expires_at` 预估用；与 config 的 `cred_ttl_seconds` 默认一致（30 分钟）。
pub const DEFAULT_CRED_TTL_SECS: i64 = 30 * 60;

/// 环节日志保留时长（秒）：只留近 3 天，更早的在打开库 / 任务开始 / 定期清理时删除。
pub const LOG_RETENTION_SECS: i64 = 3 * 86_400;

/// 环节日志行数安全阀：即便 3 天内也最多保留这么多行（防异常刷屏把库撑大）。
pub const LOG_MAX_ROWS: i64 = 50_000;

/// 建表 SQL —— 与 Python `store.SCHEMA` 逐字段一致。
const SCHEMA: &str = r#"
PRAGMA journal_mode=WAL;

CREATE TABLE IF NOT EXISTS accounts (
    biz          TEXT PRIMARY KEY,
    nickname     TEXT,
    round_head_img TEXT,
    first_seen   REAL NOT NULL,
    last_seen    REAL NOT NULL,
    focus        INTEGER NOT NULL DEFAULT 0,
    is_deleted   INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS credentials (
    biz          TEXT NOT NULL,
    uin          TEXT,
    key          TEXT,
    pass_ticket  TEXT,
    wxtoken      TEXT,
    x5           TEXT,
    appmsg_token TEXT,
    cookie       TEXT,
    extra        TEXT,
    captured_at  REAL NOT NULL,
    expires_at   REAL,
    invalidated_at REAL,
    last_used_at REAL,
    use_count    INTEGER NOT NULL DEFAULT 0,
    refresh_count INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (biz)
);

-- 凭证留档（append-only，每份 (biz,key) 一行）：真实有效期统计 + 后期经代理池独立回放的材料
CREATE TABLE IF NOT EXISTS credential_log (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    biz          TEXT NOT NULL,
    uin          TEXT,
    key          TEXT NOT NULL,
    pass_ticket  TEXT,
    wxtoken      TEXT,
    x5           TEXT,
    appmsg_token TEXT,
    cookie       TEXT,
    extra        TEXT,
    captured_at  REAL NOT NULL,
    last_seen_at REAL NOT NULL,
    expires_at   REAL,
    invalidated_at REAL,
    last_used_at REAL,
    use_count    INTEGER NOT NULL DEFAULT 0,
    UNIQUE (biz, key)
);

CREATE TABLE IF NOT EXISTS articles (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    biz          TEXT NOT NULL,
    mid          TEXT,
    idx          INTEGER,
    sn           TEXT,
    title        TEXT,
    author       TEXT,
    digest       TEXT,
    content_url  TEXT,
    cover        TEXT,
    published_at REAL,
    content_html TEXT,
    content_text TEXT,
    content_md   TEXT,
    read_num     INTEGER,
    old_like_num INTEGER,
    like_num     INTEGER,
    comment_count INTEGER,
    is_deleted   INTEGER NOT NULL DEFAULT 0,
    detail_done  INTEGER NOT NULL DEFAULT 0,
    detail_attempts INTEGER NOT NULL DEFAULT 0,
    detail_error TEXT,
    stat_done    INTEGER NOT NULL DEFAULT 0,
    created_at   REAL NOT NULL,
    updated_at   REAL NOT NULL,
    UNIQUE (biz, mid, idx)
);

CREATE TABLE IF NOT EXISTS comments (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    article_id   INTEGER NOT NULL,
    content_id   TEXT,
    nick_name    TEXT,
    content      TEXT,
    like_num     INTEGER,
    reply_json   TEXT,
    created_at   REAL,
    UNIQUE (article_id, content_id)
);

CREATE TABLE IF NOT EXISTS tasks (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    kind       TEXT NOT NULL,
    biz        TEXT,
    status     TEXT NOT NULL DEFAULT 'running',
    feedback   TEXT,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS config (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS jobs (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    upstream_id  TEXT,
    links_json   TEXT NOT NULL,
    since_json   TEXT,
    status       TEXT NOT NULL DEFAULT 'received',
    result_json  TEXT,
    feedback     TEXT,
    received_at  REAL NOT NULL,
    reported_at  REAL,
    -- 任务列表（2026-09-07）：来源 / 原始报文 / 起止时刻 / 结果分类 / 回报状态码
    kind         TEXT NOT NULL DEFAULT 'upstream',
    raw_json     TEXT,
    started_at   REAL,
    finished_at  REAL,
    outcome      TEXT,
    report_http_status INTEGER,
    phases_json  TEXT
    -- upstream_id / raw_json 为历史列（早期上游任务源），新任务不再写入；upstream_id 不唯一
);
CREATE INDEX IF NOT EXISTS idx_jobs_upstream ON jobs(upstream_id);

-- 环节日志（关键节点入库，只保留近 3 天；见 applog.rs）
CREATE TABLE IF NOT EXISTS app_log (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    ts       REAL NOT NULL,
    level    TEXT NOT NULL,
    stage    TEXT NOT NULL,
    job_id   INTEGER,
    message  TEXT NOT NULL
);

-- 限流分析（2026-09-07，Rust 版新增）：每次 getmsg 请求 / 每个执行单元 / 每次整机退避各一行，
-- 只保留近 30 天（见 purge_ratelimit_logs）。不含接口 URL 与凭证参数。
CREATE TABLE IF NOT EXISTS list_call_log (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    ts           REAL NOT NULL,
    biz          TEXT NOT NULL,
    source       TEXT NOT NULL,
    job_id       INTEGER,
    page         INTEGER NOT NULL DEFAULT 1,
    outcome      TEXT NOT NULL,
    latency_ms   INTEGER NOT NULL DEFAULT 0,
    articles     INTEGER NOT NULL DEFAULT 0,
    new_articles INTEGER NOT NULL DEFAULT 0,
    note         TEXT,
    -- 影响因素列（2026-09-07，对照分析用；见 model::ListCallFactors）
    proxy        TEXT,
    uin_hash     TEXT,
    key_hash     TEXT,
    cred_age_s   INTEGER,
    ret          INTEGER,
    http_status  INTEGER,
    offset       INTEGER,
    gap_ms       INTEGER,
    gate_min_ms  INTEGER,
    gate_max_ms  INTEGER,
    day_seq      INTEGER
);

CREATE TABLE IF NOT EXISTS run_log (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id       INTEGER,
    kind         TEXT NOT NULL,
    pass_no      INTEGER,
    started_at   REAL NOT NULL,
    finished_at  REAL NOT NULL,
    accounts     INTEGER NOT NULL DEFAULT 0,
    ok           INTEGER NOT NULL DEFAULT 0,
    failed       INTEGER NOT NULL DEFAULT 0,
    retry        INTEGER NOT NULL DEFAULT 0,
    deferred     INTEGER NOT NULL DEFAULT 0,
    new_articles INTEGER NOT NULL DEFAULT 0,
    urls         INTEGER NOT NULL DEFAULT 0,
    list_calls   INTEGER NOT NULL DEFAULT 0,
    rate_limited INTEGER NOT NULL DEFAULT 0,
    blocked      INTEGER NOT NULL DEFAULT 0,
    verify_hit   INTEGER NOT NULL DEFAULT 0,
    env_failure  INTEGER NOT NULL DEFAULT 0,
    truncated    INTEGER NOT NULL DEFAULT 0,
    note         TEXT
);

-- 微信号（多微信号支持）：采集同一时刻只用一个 is_active=1 的号；uin 首次抓到凭证时绑定。
CREATE TABLE IF NOT EXISTS wx_accounts (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    alias         TEXT NOT NULL,
    uin           TEXT NOT NULL UNIQUE,
    is_active     INTEGER NOT NULL DEFAULT 0,
    created_at    REAL NOT NULL,
    last_captured_at REAL,
    blocked_until REAL,
    blocked_reason TEXT,
    note          TEXT
);

-- 抓取历史文章：按公众号的长任务（同一时刻只有一条 running/paused），进度落库、重启续跑。
CREATE TABLE IF NOT EXISTS history_jobs (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    biz           TEXT NOT NULL,
    status        TEXT NOT NULL,
    paused_reason TEXT,
    target_count  INTEGER,
    since_ts      REAL,
    until_ts      REAL,
    page_count    INTEGER NOT NULL DEFAULT 10,
    gap_secs      INTEGER NOT NULL DEFAULT 60,
    next_offset   INTEGER NOT NULL DEFAULT 0,
    pages         INTEGER NOT NULL DEFAULT 0,
    fetched       INTEGER NOT NULL DEFAULT 0,
    matched       INTEGER NOT NULL DEFAULT 0,
    new_articles  INTEGER NOT NULL DEFAULT 0,
    next_page_at  REAL,
    resume_at     REAL,
    started_at    REAL NOT NULL,
    finished_at   REAL,
    last_error    TEXT,
    reached_end   INTEGER NOT NULL DEFAULT 0,
    created_at    REAL NOT NULL,
    errors        INTEGER NOT NULL DEFAULT 0,
    articles_json TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX IF NOT EXISTS idx_history_jobs_biz ON history_jobs(biz, id);

CREATE TABLE IF NOT EXISTS cooldown_log (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    ts      REAL NOT NULL,
    kind    TEXT NOT NULL,
    level   INTEGER NOT NULL DEFAULT 0,
    secs    INTEGER NOT NULL DEFAULT 0,
    reason  TEXT NOT NULL DEFAULT ''
);

CREATE INDEX IF NOT EXISTS idx_list_call_ts ON list_call_log(ts);
CREATE INDEX IF NOT EXISTS idx_list_call_job ON list_call_log(job_id);
CREATE INDEX IF NOT EXISTS idx_run_log_started ON run_log(started_at);
CREATE INDEX IF NOT EXISTS idx_cooldown_log_ts ON cooldown_log(ts);
CREATE INDEX IF NOT EXISTS idx_app_log_ts ON app_log(ts);
CREATE INDEX IF NOT EXISTS idx_articles_biz ON articles(biz);
CREATE INDEX IF NOT EXISTS idx_articles_pub ON articles(published_at);
CREATE INDEX IF NOT EXISTS idx_credlog_biz ON credential_log(biz, captured_at);
"#;

/// `accounts` 表读成 [`Account`] 的列清单（与 [`account_from_row`] 的下标一一对应）。
const ACCOUNT_COLS: &str = "biz, nickname, round_head_img, first_seen, last_seen, focus, \
     last_published_at, sweep_checked_at, sweep_status, sweep_error, sweep_enabled, sweep_attempts, \
     seed_url";

/// 按 [`ACCOUNT_COLS`] 的顺序把一行读成 [`Account`]。
/// 本地日历日 `YYYY-MM-DD`（列表请求每日计数的键）。
pub fn local_date_string() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn account_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Account> {
    Ok(Account {
        biz: r.get(0)?,
        nickname: r.get(1)?,
        round_head_img: r.get(2)?,
        first_seen: r.get(3)?,
        last_seen: r.get(4)?,
        focus: r.get(5)?,
        last_published_at: r.get(6)?,
        sweep_checked_at: r.get(7)?,
        sweep_status: r.get(8)?,
        sweep_error: r.get(9)?,
        sweep_enabled: r.get(10)?,
        sweep_attempts: r.get(11)?,
        seed_url: r.get(12)?,
    })
}

/// 存储层句柄。用 `Store::open` / `Store::open_in_memory` 创建（内部已建表，幂等）。
pub struct Store {
    conn: Mutex<Connection>,
    /// 凭证 TTL（秒），写凭证时据此算 `expires_at`；默认 [`DEFAULT_CRED_TTL_SECS`]，
    /// 由 [`Store::set_cred_ttl`] 与运行配置对齐。
    cred_ttl_secs: std::sync::atomic::AtomicI64,
}

/// 写锁等待上限：同一个库文件同时有多个连接（GUI 的 `AppState` / 运行链路 `runner::open_store` /
/// 手动抓凭证 `manualcap`）时，WAL 的写写互斥靠它排队。**不设就是 0**，即另一个连接正在写时本次
/// 立刻返回 `SQLITE_BUSY`（"database is locked"）—— 2026-09-10/11 在 mac 与 Windows 上都实测到接力抓凭证
/// 那一瞬「微信号登记失败：database is locked」，登记直接丢。单次写都是毫秒级，5s 足够排队。
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

impl Store {
    /// 打开（或新建）一个文件库，并建表（幂等）。
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let conn = Connection::open(path)?;
        // 必须在建表前设：`init_db` 自己就是一次写事务，也可能撞上别的连接。
        conn.busy_timeout(BUSY_TIMEOUT)?;
        let store = Self {
            conn: Mutex::new(conn),
            cred_ttl_secs: std::sync::atomic::AtomicI64::new(DEFAULT_CRED_TTL_SECS),
        };
        store.init_db()?;
        // 环节日志只留近 3 天、限流分析日志只留近 30 天：每次打开库顺手清一次。
        let _ = store.purge_logs();
        let _ = store.purge_ratelimit_logs();
        Ok(store)
    }

    /// 打开一个内存库（单测用），并建表。
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self {
            conn: Mutex::new(conn),
            cred_ttl_secs: std::sync::atomic::AtomicI64::new(DEFAULT_CRED_TTL_SECS),
        };
        store.init_db()?;
        Ok(store)
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("store mutex poisoned")
    }

    /// 幂等建表（CREATE TABLE IF NOT EXISTS）+ 老库补列迁移。
    pub fn init_db(&self) -> Result<()> {
        let conn = self.conn();
        conn.execute_batch(SCHEMA)?;
        // 老库迁移：列已存在时 ALTER 会报错，直接忽略（幂等）。
        for sql in [
            "ALTER TABLE accounts ADD COLUMN is_deleted INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE articles ADD COLUMN detail_attempts INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE articles ADD COLUMN detail_error TEXT",
            // 凭证热数据列（有效期 / 失效 / 使用统计）
            "ALTER TABLE credentials ADD COLUMN expires_at REAL",
            "ALTER TABLE credentials ADD COLUMN invalidated_at REAL",
            "ALTER TABLE credentials ADD COLUMN last_used_at REAL",
            "ALTER TABLE credentials ADD COLUMN use_count INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE credentials ADD COLUMN refresh_count INTEGER NOT NULL DEFAULT 0",
            // 任务的「最后更新时间」（任务级 + 链接级）
            "ALTER TABLE jobs ADD COLUMN since_json TEXT",
            // 定时巡检（2026-09-05）：每号的巡检状态列
            "ALTER TABLE accounts ADD COLUMN last_published_at REAL",
            "ALTER TABLE accounts ADD COLUMN sweep_checked_at REAL",
            "ALTER TABLE accounts ADD COLUMN sweep_status TEXT",
            "ALTER TABLE accounts ADD COLUMN sweep_error TEXT",
            "ALTER TABLE accounts ADD COLUMN sweep_enabled INTEGER NOT NULL DEFAULT 1",
            "ALTER TABLE accounts ADD COLUMN sweep_attempts INTEGER NOT NULL DEFAULT 0",
            // 限流分析影响因素列（2026-09-07）：出口代理 / 微信号 / 凭证 / ret / 间隔 / 当日序号
            "ALTER TABLE list_call_log ADD COLUMN proxy TEXT",
            "ALTER TABLE list_call_log ADD COLUMN uin_hash TEXT",
            "ALTER TABLE list_call_log ADD COLUMN key_hash TEXT",
            "ALTER TABLE list_call_log ADD COLUMN cred_age_s INTEGER",
            "ALTER TABLE list_call_log ADD COLUMN ret INTEGER",
            "ALTER TABLE list_call_log ADD COLUMN http_status INTEGER",
            "ALTER TABLE list_call_log ADD COLUMN offset INTEGER",
            "ALTER TABLE list_call_log ADD COLUMN gap_ms INTEGER",
            "ALTER TABLE list_call_log ADD COLUMN gate_min_ms INTEGER",
            "ALTER TABLE list_call_log ADD COLUMN gate_max_ms INTEGER",
            "ALTER TABLE list_call_log ADD COLUMN day_seq INTEGER",
            // 任务列表（2026-09-07）：来源 / 原始报文 / 起止时刻 / 结果分类 / 回报状态码
            "ALTER TABLE jobs ADD COLUMN kind TEXT NOT NULL DEFAULT 'upstream'",
            "ALTER TABLE jobs ADD COLUMN raw_json TEXT",
            "ALTER TABLE jobs ADD COLUMN started_at REAL",
            "ALTER TABLE jobs ADD COLUMN finished_at REAL",
            "ALTER TABLE jobs ADD COLUMN outcome TEXT",
            "ALTER TABLE jobs ADD COLUMN report_http_status INTEGER",
            "ALTER TABLE jobs ADD COLUMN phases_json TEXT",
            // 批量添加（2026-09-10）：添加时粘贴的文章短链，库里没文章时作接力取样
            "ALTER TABLE accounts ADD COLUMN seed_url TEXT",
        ] {
            let _ = conn.execute(sql, []);
        }
        Self::migrate_jobs_drop_unique(&conn)?;
        Self::migrate_wx_accounts_uin_unique(&conn)?;
        Self::migrate_wx_accounts(&conn)?;
        Self::retire_legacy_manual_jobs(&conn)?;
        Ok(())
    }

    /// 老库迁移：`jobs.upstream_id`（历史列）原来带 `UNIQUE`；SQLite 不能直接删约束，
    /// 检测到该唯一索引就把表重建一遍（保留 id 与全部列），幂等。
    fn migrate_jobs_drop_unique(conn: &Connection) -> Result<()> {
        let has_unique: bool = {
            let mut stmt = conn.prepare("PRAGMA index_list(jobs)")?;
            let rows = stmt.query_map([], |r| {
                let name: String = r.get(1)?;
                let unique: i64 = r.get(2)?;
                Ok((name, unique))
            })?;
            let mut hit = false;
            for row in rows {
                let (name, unique) = row?;
                if unique != 1 {
                    continue;
                }
                let mut cols = conn.prepare(&format!("PRAGMA index_info(\"{name}\")"))?;
                let names: Vec<String> = cols
                    .query_map([], |r| r.get::<_, String>(2))?
                    .collect::<rusqlite::Result<_>>()?;
                if names == ["upstream_id"] {
                    hit = true;
                }
            }
            hit
        };
        if !has_unique {
            return Ok(());
        }
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE jobs_new (
                 id           INTEGER PRIMARY KEY AUTOINCREMENT,
                 upstream_id  TEXT,
                 links_json   TEXT NOT NULL,
                 since_json   TEXT,
                 status       TEXT NOT NULL DEFAULT 'received',
                 result_json  TEXT,
                 feedback     TEXT,
                 received_at  REAL NOT NULL,
                 reported_at  REAL,
                 kind         TEXT NOT NULL DEFAULT 'upstream',
                 raw_json     TEXT,
                 started_at   REAL,
                 finished_at  REAL,
                 outcome      TEXT,
                 report_http_status INTEGER,
                 phases_json  TEXT
             );
             INSERT INTO jobs_new (id, upstream_id, links_json, since_json, status, result_json, feedback, \
                                   received_at, reported_at, kind, raw_json, started_at, finished_at, outcome, \
                                   report_http_status, phases_json)
                 SELECT id, upstream_id, links_json, since_json, status, result_json, feedback, \
                        received_at, reported_at, kind, raw_json, started_at, finished_at, outcome, \
                        report_http_status, phases_json FROM jobs;
             DROP TABLE jobs;
             ALTER TABLE jobs_new RENAME TO jobs;
             CREATE INDEX IF NOT EXISTS idx_jobs_upstream ON jobs(upstream_id);
             COMMIT;",
        )?;
        Ok(())
    }

    /// 设置凭证 TTL（秒）：之后写入的凭证 `expires_at = captured_at + ttl`。与运行配置的
    /// `cred_ttl_seconds` 对齐（runner 装配时调用）。
    pub fn set_cred_ttl(&self, ttl_secs: i64) {
        self.cred_ttl_secs
            .store(ttl_secs.max(1), std::sync::atomic::Ordering::Relaxed);
    }

    /// 当前凭证 TTL（秒）。
    pub fn cred_ttl(&self) -> i64 {
        self.cred_ttl_secs
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    // -----------------------------------------------------------------
    // config（运行时配置，键值对，value 为 JSON）
    // -----------------------------------------------------------------
    /// 读取一个运行时配置；未设置返回 `None`。
    pub fn get_config(&self, key: &str) -> Result<Option<Value>> {
        let conn = self.conn();
        let raw: Option<String> = conn
            .query_row("SELECT value FROM config WHERE key=?", params![key], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(raw.and_then(|s| serde_json::from_str::<Value>(&s).ok()))
    }

    /// 写入一个运行时配置（value 序列化成 JSON）。
    pub fn set_config(&self, key: &str, value: &Value) -> Result<()> {
        let json = serde_json::to_string(value)?;
        self.conn().execute(
            "INSERT INTO config(key, value) VALUES(?, ?) \
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, json],
        )?;
        Ok(())
    }

    // -----------------------------------------------------------------
    // 列表接口（getmsg）今日计数：`config.list_calls_date`（本地日历日 YYYY-MM-DD）+
    // `config.list_calls_count`。跨日自动归零；重启不丢（每日预算是防封禁的硬闸，必须持久化）。
    // -----------------------------------------------------------------
    /// 记一次 `getmsg`（所有路径都记：手动任务 / 巡检 / 手动重试 / 续采），返回今日累计。
    pub fn bump_list_calls(&self) -> Result<i64> {
        let today = local_date_string();
        let n = self.list_calls_today()? + 1;
        self.set_config("list_calls_date", &Value::String(today))?;
        self.set_config("list_calls_count", &Value::from(n))?;
        Ok(n)
    }

    /// 某个微信号（`uin_hash`；`None` = 不分号）在最近 `window_secs` 秒内已发的 `getmsg` 次数，以及窗口内
    /// 最早一次的时刻（据此算「何时腾出额度」）。权威数据是 `list_call_log`（每次真实请求一行，留 30 天）。
    ///
    /// 这是每号预算 `list_daily_budget` 的计数口径（2026-09-09）：三个微信号分别在累计第 224 / 223 / 206 次 `getmsg`
    /// 被 `ret=-6`，与本地日历日无关（第二个号被封当天只发了 68 次、跨了两个日历日），所以预算按
    /// 「当前微信号 × 滚动 24 小时」算，不按日历日；`list_calls_today` 只作展示 / 留档序号。
    pub fn list_calls_window(
        &self,
        uin_hash: Option<&str>,
        window_secs: i64,
    ) -> Result<(i64, Option<Epoch>)> {
        let since = now() - window_secs.max(0) as f64;
        let conn = self.conn();
        let row: (i64, Option<Epoch>) = match uin_hash {
            Some(h) => conn.query_row(
                "SELECT COUNT(*), MIN(ts) FROM list_call_log WHERE ts > ? AND uin_hash = ?",
                params![since, h],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?,
            None => conn.query_row(
                "SELECT COUNT(*), MIN(ts) FROM list_call_log WHERE ts > ?",
                params![since],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?,
        };
        Ok(row)
    }

    /// 当前微信号的 uin 短哈希（[`Self::current_uin`] → [`crate::model::short_hash`]；没有则 `None`）。
    /// 与 `list_call_log.uin_hash` 同一算法，可直接用来过滤留档。
    pub fn current_uin_hash(&self) -> Result<Option<String>> {
        Ok(self
            .current_uin()?
            .as_deref()
            .and_then(crate::model::short_hash))
    }

    /// 今日已发的 `getmsg` 次数（日期不是今天则为 0）。
    pub fn list_calls_today(&self) -> Result<i64> {
        let today = local_date_string();
        let date = self
            .get_config("list_calls_date")?
            .and_then(|v| v.as_str().map(str::to_string));
        if date.as_deref() != Some(today.as_str()) {
            return Ok(0);
        }
        Ok(self
            .get_config("list_calls_count")?
            .and_then(|v| v.as_i64())
            .unwrap_or(0))
    }

    // -----------------------------------------------------------------
    // 限流分析日志（list_call_log / run_log / cooldown_log）：每次 getmsg 请求 / 每个执行单元 /
    // 每次整机退避各记一行，供 GUI「限流分析」页聚合展示（`ratelimit::stats`）。只留近 30 天。
    // -----------------------------------------------------------------
    /// 记一次真实发出的 `getmsg` 请求及其结果（`collect_list` 每次请求后调用）。
    pub fn append_list_call(&self, ev: &ListCallEvent) -> Result<i64> {
        self.append_list_call_at(now(), ev)
    }

    /// 同上，但指定时刻（单测造历史数据用）。
    pub fn append_list_call_at(&self, ts: Epoch, ev: &ListCallEvent) -> Result<i64> {
        let conn = self.conn();
        let f = &ev.factors;
        conn.execute(
            "INSERT INTO list_call_log(ts, biz, source, job_id, page, outcome, latency_ms, articles, new_articles, note, \
             proxy, uin_hash, key_hash, cred_age_s, ret, http_status, offset, gap_ms, gate_min_ms, gate_max_ms, day_seq) \
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                ts,
                ev.biz,
                ev.source,
                ev.job_id,
                ev.page,
                ev.outcome,
                ev.latency_ms,
                ev.articles,
                ev.new_articles,
                ev.note,
                f.proxy,
                f.uin_hash,
                f.key_hash,
                f.cred_age_s,
                f.ret,
                f.http_status,
                f.offset,
                f.gap_ms,
                f.gate_min_ms,
                f.gate_max_ms,
                f.day_seq,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 某段时间起的全部列表请求（时间升序，最多 `limit` 条——取的是**最近** limit 条）。
    pub fn list_calls_since(&self, since: Epoch, limit: i64) -> Result<Vec<ListCallRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, ts, biz, source, job_id, page, outcome, latency_ms, articles, new_articles, note, \
             proxy, uin_hash, key_hash, cred_age_s, ret, http_status, offset, gap_ms, gate_min_ms, gate_max_ms, day_seq \
             FROM (SELECT * FROM list_call_log WHERE ts >= ? ORDER BY id DESC LIMIT ?) ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![since, limit.max(1)], |r| {
            Ok(ListCallRow {
                id: r.get(0)?,
                ts: r.get(1)?,
                biz: r.get(2)?,
                source: r.get(3)?,
                job_id: r.get(4)?,
                page: r.get(5)?,
                outcome: r.get(6)?,
                latency_ms: r.get(7)?,
                articles: r.get(8)?,
                new_articles: r.get(9)?,
                note: r.get(10)?,
                factors: ListCallFactors {
                    proxy: r.get(11)?,
                    uin_hash: r.get(12)?,
                    key_hash: r.get(13)?,
                    cred_age_s: r.get(14)?,
                    ret: r.get(15)?,
                    http_status: r.get(16)?,
                    offset: r.get(17)?,
                    gap_ms: r.get(18)?,
                    gate_min_ms: r.get(19)?,
                    gate_max_ms: r.get(20)?,
                    day_seq: r.get(21)?,
                },
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// 某个 job 发出的列表请求数；`outcome` 给了则只数该结果的。
    pub fn count_list_calls_for_job(&self, job_id: i64, outcome: Option<&str>) -> Result<i64> {
        let conn = self.conn();
        let n: i64 = match outcome {
            Some(o) => conn.query_row(
                "SELECT COUNT(*) FROM list_call_log WHERE job_id = ? AND outcome = ?",
                params![job_id, o],
                |r| r.get(0),
            )?,
            None => conn.query_row(
                "SELECT COUNT(*) FROM list_call_log WHERE job_id = ?",
                params![job_id],
                |r| r.get(0),
            )?,
        };
        Ok(n)
    }

    /// 记一个执行单元（手动任务 / 巡检批次）的结果摘要。
    pub fn append_run_log(&self, ev: &RunLogEvent) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO run_log(job_id, kind, pass_no, started_at, finished_at, accounts, ok, failed, retry, deferred, \
             new_articles, urls, list_calls, rate_limited, blocked, verify_hit, env_failure, truncated, note) \
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                ev.job_id,
                ev.kind,
                ev.pass_no,
                ev.started_at,
                ev.finished_at,
                ev.accounts,
                ev.ok,
                ev.failed,
                ev.retry,
                ev.deferred,
                ev.new_articles,
                ev.urls,
                ev.list_calls,
                ev.rate_limited as i64,
                ev.blocked as i64,
                ev.verify_hit as i64,
                ev.env_failure as i64,
                ev.truncated as i64,
                ev.note
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 某段时间起的执行单元（按开始时间升序，最多最近 `limit` 条）。
    pub fn run_logs_since(&self, since: Epoch, limit: i64) -> Result<Vec<RunLogRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, job_id, kind, pass_no, started_at, finished_at, accounts, ok, failed, retry, deferred, \
             new_articles, urls, list_calls, rate_limited, blocked, verify_hit, env_failure, truncated, note \
             FROM (SELECT * FROM run_log WHERE started_at >= ? ORDER BY id DESC LIMIT ?) ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![since, limit.max(1)], |r| {
            Ok(RunLogRow {
                id: r.get(0)?,
                job_id: r.get(1)?,
                kind: r.get(2)?,
                pass_no: r.get(3)?,
                started_at: r.get(4)?,
                finished_at: r.get(5)?,
                accounts: r.get(6)?,
                ok: r.get(7)?,
                failed: r.get(8)?,
                retry: r.get(9)?,
                deferred: r.get(10)?,
                new_articles: r.get(11)?,
                urls: r.get(12)?,
                list_calls: r.get(13)?,
                rate_limited: r.get::<_, i64>(14)? != 0,
                blocked: r.get::<_, i64>(15)? != 0,
                verify_hit: r.get::<_, i64>(16)? != 0,
                env_failure: r.get::<_, i64>(17)? != 0,
                truncated: r.get::<_, i64>(18)? != 0,
                note: r.get(19)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// 记一次整机退避触发 / 解除（`kind` = `ip` / `account` / `clear`）。
    pub fn append_cooldown_log(
        &self,
        kind: &str,
        level: i64,
        secs: i64,
        reason: &str,
    ) -> Result<i64> {
        self.append_cooldown_log_at(now(), kind, level, secs, reason)
    }

    /// 同上，但指定时刻（单测造历史数据用）。
    pub fn append_cooldown_log_at(
        &self,
        ts: Epoch,
        kind: &str,
        level: i64,
        secs: i64,
        reason: &str,
    ) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO cooldown_log(ts, kind, level, secs, reason) VALUES(?,?,?,?,?)",
            params![ts, kind, level, secs, reason],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 某段时间起的退避记录（时间升序，最多最近 `limit` 条）。
    pub fn cooldown_logs_since(&self, since: Epoch, limit: i64) -> Result<Vec<CooldownLogRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, ts, kind, level, secs, reason \
             FROM (SELECT * FROM cooldown_log WHERE ts >= ? ORDER BY id DESC LIMIT ?) ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![since, limit.max(1)], |r| {
            Ok(CooldownLogRow {
                id: r.get(0)?,
                ts: r.get(1)?,
                kind: r.get(2)?,
                level: r.get(3)?,
                secs: r.get(4)?,
                reason: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// 巡检轮次号（跨重启单调递增；`config.sweep_pass_seq`）。
    pub fn sweep_pass_seq(&self) -> Result<i64> {
        Ok(self
            .get_config("sweep_pass_seq")?
            .and_then(|v| v.as_i64())
            .unwrap_or(0))
    }

    /// 新一轮巡检开始：轮次号 +1 并持久化，返回新轮次号。
    pub fn bump_sweep_pass_seq(&self) -> Result<i64> {
        let n = self.sweep_pass_seq()? + 1;
        self.set_config("sweep_pass_seq", &Value::from(n))?;
        Ok(n)
    }

    /// 清理限流分析日志：三张表都删掉早于 30 天的行，`list_call_log` 再按行数安全阀截掉最老的。
    /// 返回删除行数。打开库时调用。
    pub fn purge_ratelimit_logs(&self) -> Result<usize> {
        let conn = self.conn();
        let cutoff = now() - RATELIMIT_LOG_RETENTION_SECS as f64;
        let mut n = conn.execute("DELETE FROM list_call_log WHERE ts < ?", params![cutoff])?;
        n += conn.execute("DELETE FROM run_log WHERE started_at < ?", params![cutoff])?;
        n += conn.execute("DELETE FROM cooldown_log WHERE ts < ?", params![cutoff])?;
        n += conn.execute(
            "DELETE FROM list_call_log WHERE id <= (\
                SELECT id FROM list_call_log ORDER BY id DESC LIMIT 1 OFFSET ?\
             )",
            params![RATELIMIT_LOG_MAX_ROWS],
        )?;
        Ok(n)
    }

    /// 读取全部运行时配置。
    pub fn all_config(&self) -> Result<serde_json::Map<String, Value>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT key, value FROM config")?;
        let rows = stmt.query_map([], |r| {
            let k: String = r.get(0)?;
            let v: String = r.get(1)?;
            Ok((k, v))
        })?;
        let mut out = serde_json::Map::new();
        for row in rows {
            let (k, v) = row?;
            let val = serde_json::from_str::<Value>(&v).unwrap_or(Value::String(v));
            out.insert(k, val);
        }
        Ok(out)
    }

    // -----------------------------------------------------------------
    // accounts
    // -----------------------------------------------------------------
    /// 插入/更新公众号。nickname / round_head_img 为 `None` 时不覆盖旧值（COALESCE）。
    pub fn upsert_account(
        &self,
        biz: &str,
        nickname: Option<&str>,
        round_head_img: Option<&str>,
    ) -> Result<()> {
        let now = now();
        self.conn().execute(
            "INSERT INTO accounts(biz, nickname, round_head_img, first_seen, last_seen) \
             VALUES(?,?,?,?,?) \
             ON CONFLICT(biz) DO UPDATE SET \
               nickname=COALESCE(excluded.nickname, accounts.nickname), \
               round_head_img=COALESCE(excluded.round_head_img, accounts.round_head_img), \
               last_seen=excluded.last_seen, is_deleted=0",
            params![biz, nickname, round_head_img, now, now],
        )?;
        Ok(())
    }

    /// 公众号的展示名（日志用）：有昵称用昵称，否则用 biz。查不到 / 出错也退回 biz，不影响主流程。
    pub fn account_label(&self, biz: &str) -> String {
        let nick: Option<String> = self
            .conn()
            .query_row(
                "SELECT nickname FROM accounts WHERE biz=?",
                params![biz],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()
            .ok()
            .flatten()
            .flatten();
        match nick.filter(|n| !n.trim().is_empty()) {
            Some(n) => format!("「{n}」"),
            None => biz.to_string(),
        }
    }

    /// 记下「批量添加」时粘贴的文章短链（种子链接）：库里没有该号文章时接力取样用。
    pub fn set_seed_url(&self, biz: &str, url: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE accounts SET seed_url=? WHERE biz=?",
            params![url, biz],
        )?;
        Ok(())
    }

    /// 设置“重点关注”标记。
    pub fn set_focus(&self, biz: &str, focus: bool) -> Result<()> {
        self.conn().execute(
            "UPDATE accounts SET focus=? WHERE biz=?",
            params![if focus { 1 } else { 0 }, biz],
        )?;
        Ok(())
    }

    /// 按 last_seen 倒序列出公众号（全量；下拉选择 / 编排器用）。
    pub fn list_accounts(&self) -> Result<Vec<Account>> {
        self.query_accounts(None)
    }

    /// 按 last_seen 倒序分页列出公众号（GUI 列表翻页用；Python 无对应）。
    pub fn list_accounts_page(&self, limit: i64, offset: i64) -> Result<Vec<Account>> {
        self.query_accounts(Some((limit.max(1), offset.max(0))))
    }

    /// 各公众号已采到的**最新一篇文章的发布时间**（`biz -> max(published_at)`；无文章 / 无发布时间的号不在结果里）。
    /// GUI 公众号列表「最新更新」列用，一次 GROUP BY 查全表，避免每行一查。
    pub fn latest_published_by_biz(&self) -> Result<std::collections::HashMap<String, f64>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT biz, MAX(published_at) FROM articles \
             WHERE is_deleted=0 AND published_at IS NOT NULL GROUP BY biz",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 公众号总数（翻页用，不含已删除）。
    pub fn count_accounts(&self) -> Result<i64> {
        let n: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM accounts WHERE is_deleted=0",
            [],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// `list_accounts` / `list_accounts_page` 共用的查询：`page=Some((limit, offset))` 时加 LIMIT/OFFSET。
    fn query_accounts(&self, page: Option<(i64, i64)>) -> Result<Vec<Account>> {
        let mut sql = format!(
            "SELECT {ACCOUNT_COLS} FROM accounts WHERE is_deleted=0 ORDER BY last_seen DESC, biz"
        );
        let mut args: Vec<SqlValue> = Vec::new();
        if let Some((limit, offset)) = page {
            sql.push_str(" LIMIT ? OFFSET ?");
            args.push(SqlValue::Integer(limit));
            args.push(SqlValue::Integer(offset));
        }
        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), account_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // -----------------------------------------------------------------
    // accounts —— 定时巡检（Rust 版新增，Python 无对应）
    // -----------------------------------------------------------------
    /// 单个公众号（不含已删除）。
    pub fn get_account(&self, biz: &str) -> Result<Option<Account>> {
        let conn = self.conn();
        let row = conn
            .query_row(
                &format!("SELECT {ACCOUNT_COLS} FROM accounts WHERE biz=? AND is_deleted=0"),
                params![biz],
                account_from_row,
            )
            .optional()?;
        Ok(row)
    }

    /// 本轮巡检还没处理的号：参与巡检、未删除、且 `sweep_checked_at` 早于本轮开始时刻
    /// （或从未巡检过）。**先取本轮尝试次数少的**（失败过的号排到后面重试），再取最久没查的。
    pub fn sweep_candidates(&self, pass_started_at: Epoch, limit: i64) -> Result<Vec<Account>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {ACCOUNT_COLS} FROM accounts \
             WHERE is_deleted=0 AND sweep_enabled=1 \
               AND (sweep_checked_at IS NULL OR sweep_checked_at < ?) \
             ORDER BY sweep_attempts ASC, (sweep_checked_at IS NULL) DESC, sweep_checked_at ASC, biz \
             LIMIT ?"
        ))?;
        let rows = stmt.query_map(params![pass_started_at, limit.max(1)], account_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// 本轮巡检还没处理的号数（`sweep_candidates` 的计数版）。
    pub fn sweep_pending_count(&self, pass_started_at: Epoch) -> Result<i64> {
        let n: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM accounts \
             WHERE is_deleted=0 AND sweep_enabled=1 \
               AND (sweep_checked_at IS NULL OR sweep_checked_at < ?)",
            params![pass_started_at],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// 参与巡检的号总数（未删除且未被排除）。
    pub fn sweep_total(&self) -> Result<i64> {
        let n: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM accounts WHERE is_deleted=0 AND sweep_enabled=1",
            [],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// 记一次巡检处理结果：`status` = `ok` / `failed` / `no_sample`；`last_published_at`
    /// 有值才覆盖（成功时传该号最新发布时间），尝试次数归零，`sweep_checked_at = now`。
    pub fn mark_sweep_checked(
        &self,
        biz: &str,
        status: &str,
        error: &str,
        last_published_at: Option<Epoch>,
    ) -> Result<()> {
        self.conn().execute(
            "UPDATE accounts SET sweep_checked_at=?, sweep_status=?, sweep_error=?, \
             sweep_attempts=0, last_published_at=COALESCE(?, last_published_at) WHERE biz=?",
            params![now(), status, error, last_published_at, biz],
        )?;
        Ok(())
    }

    /// 本轮内一次失败：尝试次数 +1（不动 `sweep_checked_at`，该号仍是本轮候选、排到后面重试），
    /// 记下原因。返回累计次数。
    pub fn bump_sweep_attempt(&self, biz: &str, error: &str) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "UPDATE accounts SET sweep_attempts=sweep_attempts+1, sweep_error=? WHERE biz=?",
            params![error, biz],
        )?;
        let n: i64 = conn.query_row(
            "SELECT sweep_attempts FROM accounts WHERE biz=?",
            params![biz],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// 全部号的本轮尝试次数归零（「立即开始新一轮」用）。
    pub fn reset_sweep_attempts(&self) -> Result<()> {
        self.conn()
            .execute("UPDATE accounts SET sweep_attempts=0", [])?;
        Ok(())
    }

    /// 设置是否参与巡检。
    pub fn set_sweep_enabled(&self, biz: &str, enabled: bool) -> Result<()> {
        self.conn().execute(
            "UPDATE accounts SET sweep_enabled=? WHERE biz=?",
            params![if enabled { 1 } else { 0 }, biz],
        )?;
        Ok(())
    }

    /// 该号**最新发布**的前 `limit` 篇文章的长链（带 `sn`，打开即能触发凭证请求），按发布时间倒序。
    /// 巡检续期取样：第一条是最新一篇，打不开（验证页 / 已删除 / 隐私 / 白屏）时按发布时间往前换。
    pub fn latest_article_urls(&self, biz: &str, limit: i64) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT content_url FROM articles \
             WHERE biz=? AND is_deleted=0 AND content_url IS NOT NULL AND content_url != '' \
             ORDER BY (published_at IS NULL), published_at DESC, id DESC LIMIT ?",
        )?;
        let rows = stmt.query_map(params![biz, limit.max(1)], |r| r.get::<_, String>(0))?;
        let urls = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        if !urls.is_empty() {
            return Ok(urls);
        }
        // 库里还没有该号的文章（刚批量添加、还没巡检过）：退回添加时记下的种子短链。
        let seed: Option<String> = conn
            .query_row(
                "SELECT seed_url FROM accounts WHERE biz=? AND is_deleted=0",
                params![biz],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .filter(|u| !u.trim().is_empty());
        Ok(seed.into_iter().collect())
    }

    /// 该号已采到的最新一篇文章的发布时间（`MAX(published_at)`）；没有文章 / 都无发布时间为 `None`。
    pub fn latest_published_for(&self, biz: &str) -> Result<Option<Epoch>> {
        let v: Option<f64> = self.conn().query_row(
            "SELECT MAX(published_at) FROM articles WHERE biz=? AND is_deleted=0 AND published_at IS NOT NULL",
            params![biz],
            |r| r.get(0),
        )?;
        Ok(v)
    }

    // -----------------------------------------------------------------
    // credentials
    // -----------------------------------------------------------------
    /// 写入/更新某公众号最新凭证。`None`/空字段不覆盖旧值（COALESCE），captured_at 每次刷新。
    ///
    /// 热数据维护（与 Python 的差异）：`expires_at = captured_at + ttl`；key **变了**才算一次换 key
    /// （`refresh_count + 1`、清 `invalidated_at`、使用计数归零），同 key 重复观测只顶 captured_at；
    /// 有 key 时同步追加/更新 `credential_log`（按 (biz,key) 去重，保留首次观测时刻作为寿命起点）。
    ///
    /// 返回**是否换了 key**（首次抓到也算换）：调用方据此只在真正拿到新 key 时记「凭证获取成功」，
    /// 文章页反复发的同 key 请求不重复刷日志。
    pub fn upsert_credential(&self, biz: &str, fields: &CredentialFields) -> Result<bool> {
        let extra_json = match &fields.extra {
            Some(Value::Object(m)) if !m.is_empty() => Some(serde_json::to_string(&fields.extra)?),
            Some(v) if !v.is_null() => Some(serde_json::to_string(v)?),
            _ => None,
        };
        let captured_at = now();
        let expires_at = captured_at + self.cred_ttl() as f64;
        let new_key = fields.key.as_deref().filter(|k| !k.is_empty());
        let conn = self.conn();
        // 旧 key（判断是否"换 key"）。
        let old_key: Option<String> = conn
            .query_row(
                "SELECT key FROM credentials WHERE biz=?",
                params![biz],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        let key_changed = match (new_key, old_key.as_deref()) {
            (Some(n), Some(o)) => n != o,
            (Some(_), None) => true,
            _ => false,
        };
        conn.execute(
            "INSERT INTO credentials(biz, uin, key, pass_ticket, wxtoken, x5, appmsg_token, cookie, extra, \
                                     captured_at, expires_at, invalidated_at, last_used_at, use_count, refresh_count) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,NULL,NULL,0,1) \
             ON CONFLICT(biz) DO UPDATE SET \
               uin=COALESCE(excluded.uin, credentials.uin), \
               key=COALESCE(excluded.key, credentials.key), \
               pass_ticket=COALESCE(excluded.pass_ticket, credentials.pass_ticket), \
               wxtoken=COALESCE(excluded.wxtoken, credentials.wxtoken), \
               x5=COALESCE(excluded.x5, credentials.x5), \
               appmsg_token=COALESCE(excluded.appmsg_token, credentials.appmsg_token), \
               cookie=COALESCE(excluded.cookie, credentials.cookie), \
               extra=COALESCE(excluded.extra, credentials.extra), \
               captured_at=excluded.captured_at, \
               expires_at=excluded.expires_at, \
               invalidated_at=CASE WHEN ?12 THEN NULL ELSE credentials.invalidated_at END, \
               use_count=CASE WHEN ?12 THEN 0 ELSE credentials.use_count END, \
               refresh_count=credentials.refresh_count + (CASE WHEN ?12 THEN 1 ELSE 0 END)",
            params![
                biz,
                fields.uin,
                fields.key,
                fields.pass_ticket,
                fields.wxtoken,
                fields.x5,
                fields.appmsg_token,
                fields.cookie,
                extra_json,
                captured_at,
                expires_at,
                key_changed,
            ],
        )?;
        // 留档：有 key 才记；同 (biz,key) 只顶 last_seen_at / 补空字段。
        if let Some(k) = new_key {
            conn.execute(
                "INSERT INTO credential_log(biz, uin, key, pass_ticket, wxtoken, x5, appmsg_token, cookie, extra, \
                                            captured_at, last_seen_at, expires_at) \
                 VALUES(?,?,?,?,?,?,?,?,?,?,?,?) \
                 ON CONFLICT(biz, key) DO UPDATE SET \
                   uin=COALESCE(excluded.uin, credential_log.uin), \
                   pass_ticket=COALESCE(excluded.pass_ticket, credential_log.pass_ticket), \
                   wxtoken=COALESCE(excluded.wxtoken, credential_log.wxtoken), \
                   x5=COALESCE(excluded.x5, credential_log.x5), \
                   appmsg_token=COALESCE(excluded.appmsg_token, credential_log.appmsg_token), \
                   cookie=COALESCE(excluded.cookie, credential_log.cookie), \
                   extra=COALESCE(excluded.extra, credential_log.extra), \
                   last_seen_at=excluded.last_seen_at",
                params![
                    biz,
                    fields.uin,
                    k,
                    fields.pass_ticket,
                    fields.wxtoken,
                    fields.x5,
                    fields.appmsg_token,
                    fields.cookie,
                    extra_json,
                    captured_at,
                    captured_at,
                    expires_at,
                ],
            )?;
        }
        Ok(key_changed)
    }

    /// 读取某公众号凭证；无则 `None`。
    pub fn get_credential(&self, biz: &str) -> Result<Option<Credential>> {
        let conn = self.conn();
        let cred = conn
            .query_row(
                "SELECT biz, uin, key, pass_ticket, wxtoken, x5, appmsg_token, cookie, extra, captured_at, \
                        expires_at, invalidated_at, last_used_at, use_count, refresh_count \
                 FROM credentials WHERE biz=?",
                params![biz],
                Self::row_to_credential,
            )
            .optional()?;
        Ok(cred)
    }

    /// credentials 表一行 → [`Credential`]（列顺序见 SELECT）。
    fn row_to_credential(r: &rusqlite::Row<'_>) -> rusqlite::Result<Credential> {
        Ok(Credential {
            biz: r.get(0)?,
            uin: r.get(1)?,
            key: r.get(2)?,
            pass_ticket: r.get(3)?,
            wxtoken: r.get(4)?,
            x5: r.get(5)?,
            appmsg_token: r.get(6)?,
            cookie: r.get(7)?,
            extra: r.get(8)?,
            captured_at: r.get(9)?,
            expires_at: r.get(10)?,
            invalidated_at: r.get(11)?,
            last_used_at: r.get(12)?,
            use_count: r.get(13)?,
            refresh_count: r.get(14)?,
        })
    }

    /// 全部凭证热数据（按 captured_at 倒序），GUI/统计用。
    pub fn list_credentials(&self) -> Result<Vec<Credential>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT biz, uin, key, pass_ticket, wxtoken, x5, appmsg_token, cookie, extra, captured_at, \
                    expires_at, invalidated_at, last_used_at, use_count, refresh_count \
             FROM credentials ORDER BY captured_at DESC",
        )?;
        let rows = stmt.query_map([], Self::row_to_credential)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// 记一次"拿该号凭证回放接口"：`last_used_at=now`、`use_count+1`（热表与留档同步）。
    pub fn touch_credential_used(&self, biz: &str) -> Result<()> {
        let t = now();
        let conn = self.conn();
        conn.execute(
            "UPDATE credentials SET last_used_at=?, use_count=use_count+1 WHERE biz=?",
            params![t, biz],
        )?;
        conn.execute(
            "UPDATE credential_log SET last_used_at=?, use_count=use_count+1 \
             WHERE biz=? AND key=(SELECT key FROM credentials WHERE biz=?)",
            params![t, biz, biz],
        )?;
        Ok(())
    }

    /// 实测失效打点：回放拿到 `ret=-3`（no session）时调用。之后 `credential_is_fresh` 立即为假，
    /// 不必等 TTL 走完；留档同步记 `invalidated_at`，供统计真实有效时长。幂等（只记第一次）。
    pub fn mark_credential_invalid(&self, biz: &str) -> Result<()> {
        let t = now();
        let conn = self.conn();
        conn.execute(
            "UPDATE credentials SET invalidated_at=COALESCE(invalidated_at, ?) WHERE biz=?",
            params![t, biz],
        )?;
        conn.execute(
            "UPDATE credential_log SET invalidated_at=COALESCE(invalidated_at, ?) \
             WHERE biz=? AND key=(SELECT key FROM credentials WHERE biz=?)",
            params![t, biz, biz],
        )?;
        Ok(())
    }

    /// 凭证留档（按 captured_at 倒序；`biz` 为 `None` 时取全部）。统计真实有效期 /
    /// 后期经代理池回放用。
    pub fn credential_log(&self, biz: Option<&str>, limit: i64) -> Result<Vec<CredentialLogRow>> {
        let mut sql = String::from(
            "SELECT id, biz, uin, key, pass_ticket, wxtoken, x5, appmsg_token, cookie, extra, \
                    captured_at, last_seen_at, expires_at, invalidated_at, last_used_at, use_count \
             FROM credential_log",
        );
        let mut args: Vec<SqlValue> = Vec::new();
        if let Some(b) = biz {
            sql.push_str(" WHERE biz=?");
            args.push(SqlValue::Text(b.to_string()));
        }
        sql.push_str(" ORDER BY captured_at DESC, id DESC LIMIT ?");
        args.push(SqlValue::Integer(limit));
        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), |r| {
            Ok(CredentialLogRow {
                id: r.get(0)?,
                biz: r.get(1)?,
                uin: r.get(2)?,
                key: r.get(3)?,
                pass_ticket: r.get(4)?,
                wxtoken: r.get(5)?,
                x5: r.get(6)?,
                appmsg_token: r.get(7)?,
                cookie: r.get(8)?,
                extra: r.get(9)?,
                captured_at: r.get(10)?,
                last_seen_at: r.get(11)?,
                expires_at: r.get(12)?,
                invalidated_at: r.get(13)?,
                last_used_at: r.get(14)?,
                use_count: r.get(15)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // -----------------------------------------------------------------
    // app_log（环节日志：关键节点入库，只保留近 3 天；见 applog.rs）
    // -----------------------------------------------------------------

    /// 追加一条环节日志，返回行 id。
    pub fn append_log(
        &self,
        ts: Epoch,
        level: &str,
        stage: &str,
        job_id: Option<i64>,
        message: &str,
    ) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO app_log(ts, level, stage, job_id, message) VALUES(?,?,?,?,?)",
            params![ts, level, stage, job_id, message],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 查询环节日志（按时间升序返回**最近**的 `limit` 条）。
    ///
    /// - `since`：只取 `ts >= since`（`None` = 不限，实际最多 3 天，因为更早的已被清）；
    /// - `stage`：只取某环节 code；
    /// - `min_level`：`"warn"` = 只看告警及以上，`"error"` = 只看错误；`None`/其它 = 全部。
    pub fn list_logs(
        &self,
        since: Option<Epoch>,
        stage: Option<&str>,
        min_level: Option<&str>,
        limit: i64,
    ) -> Result<Vec<AppLogRow>> {
        let mut sql =
            String::from("SELECT id, ts, level, stage, job_id, message FROM app_log WHERE 1=1");
        let mut args: Vec<SqlValue> = Vec::new();
        if let Some(t) = since {
            sql.push_str(" AND ts >= ?");
            args.push(SqlValue::Real(t));
        }
        if let Some(st) = stage.filter(|s| !s.is_empty()) {
            sql.push_str(" AND stage = ?");
            args.push(SqlValue::Text(st.to_string()));
        }
        match min_level {
            Some("warn") => sql.push_str(" AND level IN ('warn','error')"),
            Some("error") => sql.push_str(" AND level = 'error'"),
            _ => {}
        }
        // 先倒序取最近 limit 条，再按 id 升序输出（时间线顺序）。
        let sql = format!(
            "SELECT id, ts, level, stage, job_id, message FROM ({sql} ORDER BY id DESC LIMIT ?)              ORDER BY id ASC"
        );
        args.push(SqlValue::Integer(limit.max(1)));
        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), |r| {
            Ok(AppLogRow {
                id: r.get(0)?,
                ts: r.get(1)?,
                level: r.get(2)?,
                stage: r.get(3)?,
                job_id: r.get(4)?,
                message: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// 环节日志总行数。
    pub fn count_logs(&self) -> Result<i64> {
        Ok(self
            .conn()
            .query_row("SELECT COUNT(*) FROM app_log", [], |r| r.get(0))?)
    }

    /// 清理过期日志：删掉早于 [`LOG_RETENTION_SECS`] 的行，再按 [`LOG_MAX_ROWS`] 截掉最老的。
    /// 返回删除行数。打开库 / 任务开始 / 每写 N 条时调用（幂等、便宜）。
    pub fn purge_logs(&self) -> Result<usize> {
        let conn = self.conn();
        let cutoff = now() - LOG_RETENTION_SECS as f64;
        let mut n = conn.execute("DELETE FROM app_log WHERE ts < ?", params![cutoff])?;
        n += conn.execute(
            "DELETE FROM app_log WHERE id <= (                SELECT id FROM app_log ORDER BY id DESC LIMIT 1 OFFSET ?             )",
            params![LOG_MAX_ROWS],
        )?;
        Ok(n)
    }

    /// 清空全部环节日志（用户在日志页手动清理）。返回删除行数。
    pub fn clear_logs(&self) -> Result<usize> {
        Ok(self.conn().execute("DELETE FROM app_log", [])?)
    }

    /// 凭证真实有效时长统计（秒）：留档里**实测失效过**的 key，取 `invalidated_at - captured_at`。
    /// 返回 `(样本数, 平均, 最小, 最大)`；无样本 → `(0, 0, 0, 0)`。用于校准 TTL 猜测值。
    pub fn credential_lifetime_stats(&self) -> Result<(i64, f64, f64, f64)> {
        let conn = self.conn();
        let row = conn.query_row(
            "SELECT COUNT(*), COALESCE(AVG(invalidated_at - captured_at), 0), \
                    COALESCE(MIN(invalidated_at - captured_at), 0), \
                    COALESCE(MAX(invalidated_at - captured_at), 0) \
             FROM credential_log WHERE invalidated_at IS NOT NULL AND invalidated_at > captured_at",
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, f64>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, f64>(3)?,
                ))
            },
        )?;
        Ok(row)
    }

    /// 当前微信号的 `uin` = **激活**的微信号（`wx_accounts.is_active=1`）已绑定的 uin；
    /// 没有激活号或尚未绑定返回 `None`（此时预算按全部请求计、凭证不按 uin 比较）。
    pub fn current_uin(&self) -> Result<Option<String>> {
        let conn = self.conn();
        let uin: Option<Option<String>> = conn
            .query_row(
                "SELECT uin FROM wx_accounts WHERE is_active=1 LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(uin.flatten().filter(|u| !u.is_empty()))
    }

    /// 凭证是否仍新鲜：存在且 key 非空，且距 captured_at 未超过 `ttl_seconds`，且本份 key **没有**
    /// 被实测失效（`invalidated_at` 有值即失效，见 [`Self::mark_credential_invalid`]；只有换 key 才清），
    /// 且属于**当前激活的微信号**（uin 与 [`Self::current_uin`] 相同；凭证或激活号没有 uin 时不比较）。
    ///
    /// 实测失效判据与 uin 判据的由来：`getmsg` 的封禁按微信号计，换号后若复用旧号的 key 直接采，
    /// 会再撞 `ret=-6` 并再次退避。
    pub fn credential_is_fresh(&self, biz: &str, ttl_seconds: i64) -> Result<bool> {
        let Some(cred) = self.get_credential(biz)? else {
            return Ok(false);
        };
        if cred.key.as_deref().unwrap_or("").is_empty() {
            return Ok(false);
        }
        if cred.invalidated_at.is_some() {
            return Ok(false);
        }
        if (now() - cred.captured_at) >= ttl_seconds as f64 {
            return Ok(false);
        }
        if let (Some(uin), Some(current)) = (
            cred.uin.as_deref().filter(|u| !u.is_empty()),
            self.current_uin()?,
        ) {
            if uin != current {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// 在 `since`（epoch 秒）**及其之后**抓到（或刷新）过有效凭证（key 非空）的公众号。
    ///
    /// 用于编排把「本批真正接力打开、当场抓到凭证的号」与「此前遗留仍新鲜的号」区分开：
    /// `upsert_credential` 每次都刷新 `captured_at`，故同一条链接被重复下发、重开时也会
    /// 命中（captured_at 被顶到 `now()`），不会因「30 分钟内还新鲜」而被误当成本批成果。
    /// 按 captured_at 升序返回。
    pub fn bizs_captured_since(&self, since: Epoch) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT biz FROM credentials \
             WHERE key IS NOT NULL AND key != '' AND captured_at >= ? \
             ORDER BY captured_at ASC",
        )?;
        let rows = stmt.query_map(params![since], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // -----------------------------------------------------------------
    // wx_accounts（微信号管理）
    // -----------------------------------------------------------------
    const WX_COLS: &str = "id, alias, uin, is_active, created_at, last_captured_at, blocked_until, blocked_reason, note";

    fn wx_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<WxAccount> {
        Ok(WxAccount {
            id: r.get(0)?,
            alias: r.get(1)?,
            uin: r.get(2)?,
            is_active: r.get::<_, i64>(3)? != 0,
            created_at: r.get(4)?,
            last_captured_at: r.get(5)?,
            blocked_until: r.get(6)?,
            blocked_reason: r.get(7)?,
            note: r.get(8)?,
        })
    }

    /// 老库迁移（幂等）：早期 `wx_accounts.uin` 可空、不唯一（有手动新增入口时，激活未绑定的记录再抓到
    /// 已登记的 uin 会绑成两行同 uin）。现在 uin 必填且唯一：先按 uin 去重（同 uin 多行保留激活的那条，
    /// 否则保留最近抓到凭证的，再否则 id 最小的），再删没绑定 uin 的行，最后补唯一索引
    /// （新库建表时已带 `UNIQUE`，索引重复创建无害）。
    fn migrate_wx_accounts_uin_unique(conn: &Connection) -> Result<()> {
        conn.execute(
            "DELETE FROM wx_accounts WHERE uin IS NOT NULL AND uin != '' AND id NOT IN (
                SELECT id FROM (
                    SELECT id, ROW_NUMBER() OVER (
                        PARTITION BY uin
                        ORDER BY is_active DESC, COALESCE(last_captured_at, 0) DESC, id ASC
                    ) AS rn FROM wx_accounts WHERE uin IS NOT NULL AND uin != ''
                ) WHERE rn = 1
            )",
            [],
        )?;
        conn.execute("DELETE FROM wx_accounts WHERE uin IS NULL OR uin = ''", [])?;
        conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_wx_accounts_uin ON wx_accounts(uin)",
            [],
        )?;
        Ok(())
    }

    /// 老库迁移（幂等）：手动任务功能已下线（2026-09-10，批量添加改为添加即建号），库里还没跑完的
    /// `manual` / `upstream` 任务全部收口成 `error` 留档，否则它们永远停在「进行中」、不能删除。
    /// 反馈里提示用户重新批量添加这些链接。
    fn retire_legacy_manual_jobs(conn: &Connection) -> Result<usize> {
        let n = conn.execute(
            "UPDATE jobs SET status='error', outcome='error', finished_at=?,
                    feedback='手动任务功能已下线：请在「公众号列表 → 批量添加」重新粘贴这些文章的短链'
             WHERE kind IN ('manual','upstream') AND outcome IS NULL",
            params![now()],
        )?;
        Ok(n)
    }

    /// 老库迁移（幂等）：`wx_accounts` 为空而 `credentials` 里已有带 uin 的凭证 → 每个不同的 uin
    /// 各登记一条（别名「微信号 N」），`captured_at` 最新的那条激活；旧的整机退避若是账号类且仍在未来，
    /// 迁到激活号的 `blocked_until`。
    fn migrate_wx_accounts(conn: &Connection) -> Result<()> {
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM wx_accounts", [], |r| r.get(0))?;
        if n > 0 {
            return Ok(());
        }
        // 每个 uin 的最近抓取时间，最新的排最前。
        let uins: Vec<(String, Option<Epoch>)> = {
            let mut stmt = conn.prepare(
                "SELECT uin, MAX(captured_at) FROM credentials \
                 WHERE uin IS NOT NULL AND uin != '' GROUP BY uin ORDER BY 2 DESC",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get(1)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if uins.is_empty() {
            return Ok(());
        }
        // 旧退避状态：只认账号类（kind=account）且截止仍在未来的。
        let (blocked_until, blocked_reason) = conn
            .query_row(
                "SELECT value FROM config WHERE key='cooldown_state'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .filter(|v| v.get("kind").and_then(Value::as_str) == Some("account"))
            .and_then(|v| {
                let until = v.get("until")?.as_f64()?;
                (until > now()).then(|| {
                    (
                        until,
                        v.get("reason")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    )
                })
            })
            .map(|(u, r)| (Some(u), Some(r)))
            .unwrap_or((None, None));
        for (i, (uin, last_captured)) in uins.iter().enumerate() {
            let active = i == 0;
            let id = Self::wx_insert(conn, uin, active, *last_captured)?;
            if active {
                conn.execute(
                    "UPDATE wx_accounts SET blocked_until=?1, blocked_reason=?2 WHERE id=?3",
                    params![blocked_until, blocked_reason, id],
                )?;
            }
        }
        Ok(())
    }

    /// 登记一条微信号（别名「微信号 <id>」），返回 id。`active` 为真时调用方须保证当时没有别的激活号。
    fn wx_insert(
        conn: &Connection,
        uin: &str,
        active: bool,
        last_captured: Option<Epoch>,
    ) -> Result<i64> {
        conn.execute(
            "INSERT INTO wx_accounts(alias, uin, is_active, created_at, last_captured_at) \
             VALUES('', ?1, ?2, ?3, ?4)",
            params![uin, active as i64, now(), last_captured],
        )?;
        let id = conn.last_insert_rowid();
        conn.execute(
            "UPDATE wx_accounts SET alias=?1 WHERE id=?2",
            params![format!("微信号 {id}"), id],
        )?;
        Ok(id)
    }

    /// 全部微信号（激活的排最前，其余按创建时间）。
    pub fn wx_list(&self) -> Result<Vec<WxAccount>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM wx_accounts ORDER BY is_active DESC, created_at ASC, id ASC",
            Self::WX_COLS
        ))?;
        let rows = stmt.query_map([], Self::wx_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// 按 id 取一条。
    pub fn wx_get(&self, id: i64) -> Result<Option<WxAccount>> {
        let conn = self.conn();
        Ok(conn
            .query_row(
                &format!("SELECT {} FROM wx_accounts WHERE id=?", Self::WX_COLS),
                params![id],
                Self::wx_from_row,
            )
            .optional()?)
    }

    /// 当前激活的微信号。
    pub fn wx_active(&self) -> Result<Option<WxAccount>> {
        let conn = self.conn();
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {} FROM wx_accounts WHERE is_active=1 LIMIT 1",
                    Self::WX_COLS
                ),
                [],
                Self::wx_from_row,
            )
            .optional()?)
    }

    /// 改别名 / 备注。
    pub fn wx_update(&self, id: i64, alias: &str, note: Option<&str>) -> Result<()> {
        let alias = alias.trim();
        if alias.is_empty() {
            anyhow::bail!("别名不能为空");
        }
        let n = self.conn().execute(
            "UPDATE wx_accounts SET alias=?1, note=?2 WHERE id=?3",
            params![alias, note.map(str::trim).filter(|s| !s.is_empty()), id],
        )?;
        if n == 0 {
            anyhow::bail!("微信号不存在：#{id}");
        }
        Ok(())
    }

    /// 删除一条（调用方先判断是否在采集中）。
    pub fn wx_delete(&self, id: i64) -> Result<()> {
        let n = self
            .conn()
            .execute("DELETE FROM wx_accounts WHERE id=?", params![id])?;
        if n == 0 {
            anyhow::bail!("微信号不存在：#{id}");
        }
        Ok(())
    }

    /// 激活某个号（事务里先全清再置 1）。
    pub fn wx_activate(&self, id: i64) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let exists: i64 = tx.query_row(
            "SELECT COUNT(*) FROM wx_accounts WHERE id=?",
            params![id],
            |r| r.get(0),
        )?;
        if exists == 0 {
            anyhow::bail!("微信号不存在：#{id}");
        }
        tx.execute("UPDATE wx_accounts SET is_active=0", [])?;
        tx.execute("UPDATE wx_accounts SET is_active=1 WHERE id=?", params![id])?;
        tx.commit()?;
        Ok(())
    }

    /// 记封号退避：`until` 截止、`reason` 原因。
    pub fn wx_set_blocked(&self, id: i64, until: Epoch, reason: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE wx_accounts SET blocked_until=?1, blocked_reason=?2 WHERE id=?3",
            params![until, reason, id],
        )?;
        Ok(())
    }

    /// 解封（清掉截止与原因）。
    pub fn wx_unblock(&self, id: i64) -> Result<()> {
        self.conn().execute(
            "UPDATE wx_accounts SET blocked_until=NULL, blocked_reason=NULL WHERE id=?",
            params![id],
        )?;
        Ok(())
    }

    /// 抓到带 `uin` 的凭证时自动登记 / 比对（事务内）：
    /// - uin 属于激活号 → 刷新抓取时间，`Same`；
    /// - uin 属于另一条已登记记录 → `Mismatch`（调用方不入库并中止本批）；
    /// - uin 没见过 → 新建记录（别名「微信号 N」）；没有激活号就顺带激活（`Registered{activated:true}`），
    ///   否则 `Registered{activated:false}`，调用方按 Mismatch 处理；
    /// - 已登记但没有任何激活号（比如激活号被删了）→ 启用它，同样 `Registered{activated:true}`。
    pub fn wx_note_captured_uin(&self, uin: &str) -> Result<WxBind> {
        let uin = uin.trim();
        if uin.is_empty() {
            anyhow::bail!("uin 为空");
        }
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let active: Option<(i64, String)> = tx
            .query_row(
                "SELECT id, alias FROM wx_accounts WHERE is_active=1 LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let existing: Option<(i64, String)> = tx
            .query_row(
                "SELECT id, alias FROM wx_accounts WHERE uin=?",
                params![uin],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let out = match (existing, active) {
            (Some((id, _)), Some((aid, _))) if id == aid => {
                tx.execute(
                    "UPDATE wx_accounts SET last_captured_at=? WHERE id=?",
                    params![now(), id],
                )?;
                WxBind::Same
            }
            (Some((_, captured_alias)), Some((_, alias))) => WxBind::Mismatch {
                alias,
                captured_alias,
            },
            (Some((id, _)), None) => {
                tx.execute(
                    "UPDATE wx_accounts SET is_active=1, last_captured_at=? WHERE id=?",
                    params![now(), id],
                )?;
                WxBind::Registered {
                    id,
                    activated: true,
                }
            }
            (None, active) => {
                let activated = active.is_none();
                let id = Self::wx_insert(&tx, uin, activated, Some(now()))?;
                WxBind::Registered { id, activated }
            }
        };
        tx.commit()?;
        Ok(out)
    }

    // -----------------------------------------------------------------
    // history_jobs（抓取历史文章）
    // -----------------------------------------------------------------
    const HISTORY_COLS: &'static str = "id, biz, status, paused_reason, target_count, since_ts, until_ts, page_count, gap_secs, \
        next_offset, pages, fetched, matched, new_articles, next_page_at, resume_at, started_at, finished_at, last_error, \
        reached_end, created_at, errors, articles_json";

    fn history_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryJob> {
        Ok(HistoryJob {
            id: r.get(0)?,
            biz: r.get(1)?,
            status: r.get(2)?,
            paused_reason: r.get(3)?,
            target_count: r.get(4)?,
            since_ts: r.get(5)?,
            until_ts: r.get(6)?,
            page_count: r.get(7)?,
            gap_secs: r.get(8)?,
            next_offset: r.get(9)?,
            pages: r.get(10)?,
            fetched: r.get(11)?,
            matched: r.get(12)?,
            new_articles: r.get(13)?,
            next_page_at: r.get(14)?,
            resume_at: r.get(15)?,
            started_at: r.get(16)?,
            finished_at: r.get(17)?,
            last_error: r.get(18)?,
            reached_end: r.get::<_, i64>(19)? != 0,
            created_at: r.get(20)?,
            errors: r.get(21)?,
            articles_json: r.get(22)?,
        })
    }

    /// 新建历史抓取任务（`id` 由库分配并回填到返回值）。
    pub fn history_insert(&self, job: &HistoryJob) -> Result<HistoryJob> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO history_jobs(biz, status, paused_reason, target_count, since_ts, until_ts, page_count, gap_secs, \
             next_offset, pages, fetched, matched, new_articles, next_page_at, resume_at, started_at, finished_at, \
             last_error, reached_end, created_at, errors, articles_json) \
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                job.biz,
                job.status,
                job.paused_reason,
                job.target_count,
                job.since_ts,
                job.until_ts,
                job.page_count,
                job.gap_secs,
                job.next_offset,
                job.pages,
                job.fetched,
                job.matched,
                job.new_articles,
                job.next_page_at,
                job.resume_at,
                job.started_at,
                job.finished_at,
                job.last_error,
                job.reached_end as i64,
                job.created_at,
                job.errors,
                if job.articles_json.is_empty() {
                    "[]"
                } else {
                    job.articles_json.as_str()
                },
            ],
        )?;
        let mut out = job.clone();
        out.id = conn.last_insert_rowid();
        Ok(out)
    }

    /// 回写任务全部可变字段（按 id）。
    pub fn history_update(&self, job: &HistoryJob) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE history_jobs SET status=?, paused_reason=?, next_offset=?, pages=?, fetched=?, matched=?, \
             new_articles=?, next_page_at=?, resume_at=?, finished_at=?, last_error=?, reached_end=?, errors=?, \
             articles_json=? WHERE id=?",
            params![
                job.status,
                job.paused_reason,
                job.next_offset,
                job.pages,
                job.fetched,
                job.matched,
                job.new_articles,
                job.next_page_at,
                job.resume_at,
                job.finished_at,
                job.last_error,
                job.reached_end as i64,
                job.errors,
                if job.articles_json.is_empty() {
                    "[]"
                } else {
                    job.articles_json.as_str()
                },
                job.id,
            ],
        )?;
        Ok(())
    }

    pub fn history_get(&self, id: i64) -> Result<Option<HistoryJob>> {
        let conn = self.conn();
        Ok(conn
            .query_row(
                &format!("SELECT {} FROM history_jobs WHERE id=?", Self::HISTORY_COLS),
                params![id],
                Self::history_from_row,
            )
            .optional()?)
    }

    /// 当前活动（running / paused）的历史任务——同一时刻最多一条。
    pub fn history_active(&self) -> Result<Option<HistoryJob>> {
        let conn = self.conn();
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {} FROM history_jobs WHERE status IN ('running','paused') ORDER BY id DESC LIMIT 1",
                    Self::HISTORY_COLS
                ),
                [],
                Self::history_from_row,
            )
            .optional()?)
    }

    /// 某号最新一条历史任务（任何状态）。
    pub fn history_latest_for(&self, biz: &str) -> Result<Option<HistoryJob>> {
        let conn = self.conn();
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {} FROM history_jobs WHERE biz=? ORDER BY id DESC LIMIT 1",
                    Self::HISTORY_COLS
                ),
                params![biz],
                Self::history_from_row,
            )
            .optional()?)
    }

    /// 每个号最新一条历史任务（公众号列表一次取全）。
    pub fn history_latest_by_biz(&self) -> Result<std::collections::HashMap<String, HistoryJob>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM history_jobs WHERE id IN (SELECT MAX(id) FROM history_jobs GROUP BY biz)",
            Self::HISTORY_COLS
        ))?;
        let rows = stmt.query_map([], Self::history_from_row)?;
        let mut out = std::collections::HashMap::new();
        for r in rows {
            let j = r?;
            out.insert(j.biz.clone(), j);
        }
        Ok(out)
    }

    // -----------------------------------------------------------------
    // articles
    // -----------------------------------------------------------------
    /// 插入或更新文章，返回 article id。列表采集只带基础字段，详情/转赞评后续补齐。
    pub fn upsert_article(
        &self,
        biz: &str,
        mid: &str,
        idx: i64,
        fields: &ArticleFields,
    ) -> Result<i64> {
        let now = now();
        let payload = article_payload(fields);
        let conn = self.conn();

        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM articles WHERE biz=? AND mid=? AND idx=?",
                params![biz, mid, idx],
                |r| r.get(0),
            )
            .optional()?;

        if let Some(aid) = existing {
            if !payload.is_empty() {
                let sets: Vec<String> = payload.iter().map(|(c, _)| format!("{c}=?")).collect();
                let sql = format!(
                    "UPDATE articles SET {}, updated_at=? WHERE id=?",
                    sets.join(", ")
                );
                let mut vals: Vec<SqlValue> = payload.into_iter().map(|(_, v)| v).collect();
                vals.push(SqlValue::Real(now));
                vals.push(SqlValue::Integer(aid));
                conn.execute(&sql, params_from_iter(vals))?;
            }
            return Ok(aid);
        }

        // 插入：base(biz,mid,idx) + payload + created_at + updated_at
        let mut cols: Vec<String> = vec!["biz".into(), "mid".into(), "idx".into()];
        let mut vals: Vec<SqlValue> = vec![
            SqlValue::Text(biz.to_string()),
            SqlValue::Text(mid.to_string()),
            SqlValue::Integer(idx),
        ];
        for (c, v) in payload {
            cols.push(c.to_string());
            vals.push(v);
        }
        cols.push("created_at".into());
        vals.push(SqlValue::Real(now));
        cols.push("updated_at".into());
        vals.push(SqlValue::Real(now));

        let placeholders = vec!["?"; cols.len()].join(", ");
        let sql = format!(
            "INSERT INTO articles({}) VALUES({})",
            cols.join(", "),
            placeholders
        );
        conn.execute(&sql, params_from_iter(vals))?;
        Ok(conn.last_insert_rowid())
    }

    /// 该 biz 当前最大 article id（用于区分本次"新增"；对齐 Python `collector._max_article_id`）。
    pub fn max_article_id(&self, biz: &str) -> Result<i64> {
        let conn = self.conn();
        let m: i64 = conn.query_row(
            "SELECT COALESCE(MAX(id), 0) FROM articles WHERE biz=?",
            params![biz],
            |r| r.get(0),
        )?;
        Ok(m)
    }

    /// 按发布时间倒序列出文章（Web 后台浏览用）。`only_detail=true` 只列已采正文的。
    pub fn list_articles(
        &self,
        biz: Option<&str>,
        limit: i64,
        offset: i64,
        only_detail: bool,
    ) -> Result<Vec<ArticleRow>> {
        let mut sql = String::from(
            "SELECT id, biz, mid, idx, title, author, digest, content_url, \
             published_at, detail_done, is_deleted, detail_error FROM articles WHERE is_deleted=0",
        );
        let mut args: Vec<SqlValue> = Vec::new();
        if let Some(b) = biz {
            sql.push_str(" AND biz=?");
            args.push(SqlValue::Text(b.to_string()));
        }
        if only_detail {
            sql.push_str(" AND detail_done=1");
        }
        sql.push_str(
            " ORDER BY (published_at IS NULL), published_at DESC, id DESC LIMIT ? OFFSET ?",
        );
        args.push(SqlValue::Integer(limit));
        args.push(SqlValue::Integer(offset));

        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), |r| {
            Ok(ArticleRow {
                id: r.get(0)?,
                biz: r.get(1)?,
                mid: r.get(2)?,
                idx: r.get(3)?,
                title: r.get(4)?,
                author: r.get(5)?,
                digest: r.get(6)?,
                content_url: r.get(7)?,
                published_at: r.get(8)?,
                detail_done: r.get(9)?,
                is_deleted: r.get(10)?,
                detail_error: r.get(11)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// 尚未采过正文、且有链接可抓的文章（补详情候选，对齐 Python `articles_pending(kind="detail")`）。
    /// `article_id` 指定时只取那一篇（单篇「抓取详情」按钮用）。
    pub fn articles_pending_detail(
        &self,
        biz: Option<&str>,
        article_id: Option<i64>,
        limit: Option<i64>,
    ) -> Result<Vec<ArticleRow>> {
        let mut sql = String::from(
            "SELECT id, biz, mid, idx, title, author, digest, content_url, \
             published_at, detail_done, is_deleted, detail_error FROM articles \
             WHERE is_deleted=0 AND detail_done=0 AND content_url IS NOT NULL AND content_url != ''",
        );
        let mut args: Vec<SqlValue> = Vec::new();
        if let Some(b) = biz {
            sql.push_str(" AND biz=?");
            args.push(SqlValue::Text(b.to_string()));
        }
        if let Some(aid) = article_id {
            // 手动单篇重试：不受失败次数限制。
            sql.push_str(" AND id=?");
            args.push(SqlValue::Integer(aid));
        } else {
            // 自动批量：失败满 3 次的不再进候选，避免每批都反复抓注定失败的文章。
            sql.push_str(" AND detail_attempts < 3");
        }
        sql.push_str(" ORDER BY id");
        // limit=None 不加 LIMIT：一次把全部待补取出，逐篇串行/并发处理，没有「每批最多 N 篇」的概念。
        if let Some(n) = limit {
            sql.push_str(" LIMIT ?");
            args.push(SqlValue::Integer(n.max(1)));
        }

        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), |r| {
            Ok(ArticleRow {
                id: r.get(0)?,
                biz: r.get(1)?,
                mid: r.get(2)?,
                idx: r.get(3)?,
                title: r.get(4)?,
                author: r.get(5)?,
                digest: r.get(6)?,
                content_url: r.get(7)?,
                published_at: r.get(8)?,
                detail_done: r.get(9)?,
                is_deleted: r.get(10)?,
                detail_error: r.get(11)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// 文章总数（翻页用；biz 过滤可选，不含已删除）。
    pub fn count_articles(&self, biz: Option<&str>) -> Result<i64> {
        let conn = self.conn();
        let n: i64 = match biz {
            Some(b) => conn.query_row(
                "SELECT COUNT(*) FROM articles WHERE is_deleted=0 AND biz=?",
                params![b],
                |r| r.get(0),
            )?,
            None => conn.query_row(
                "SELECT COUNT(*) FROM articles WHERE is_deleted=0",
                [],
                |r| r.get(0),
            )?,
        };
        Ok(n)
    }

    /// 单篇文章的正文 Markdown（阅读页用）。返回 (行, content_md)。
    pub fn get_article_md(&self, id: i64) -> Result<Option<(ArticleRow, Option<String>)>> {
        let conn = self.conn();
        let row = conn
            .query_row(
                "SELECT id, biz, mid, idx, title, author, digest, content_url, \
                 published_at, detail_done, is_deleted, detail_error, content_md FROM articles WHERE id=?",
                params![id],
                |r| {
                    Ok((
                        ArticleRow {
                            id: r.get(0)?,
                            biz: r.get(1)?,
                            mid: r.get(2)?,
                            idx: r.get(3)?,
                            title: r.get(4)?,
                            author: r.get(5)?,
                            digest: r.get(6)?,
                            content_url: r.get(7)?,
                            published_at: r.get(8)?,
                            detail_done: r.get(9)?,
                            is_deleted: r.get(10)?,
                            detail_error: r.get(11)?,
                        },
                        r.get::<_, Option<String>>(12)?,
                    ))
                },
            )
            .optional()?;
        Ok(row)
    }

    /// 文章计数（控制面板状态卡用）：(总数, 已采详情数, 待补数)，均不含已删除。
    /// 待补 = 未采且还会被自动批量处理的（排除不可用 detail_done=-1、失败满 3 次、无链接）。
    pub fn article_counts(&self) -> Result<(i64, i64, i64)> {
        let conn = self.conn();
        let row = conn.query_row(
            "SELECT COUNT(*), \
                    COALESCE(SUM(detail_done=1), 0), \
                    COALESCE(SUM(detail_done=0 AND detail_attempts<3 \
                                 AND content_url IS NOT NULL AND content_url!=''), 0) \
             FROM articles WHERE is_deleted=0",
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        )?;
        Ok(row)
    }

    /// 标记一篇文章为**永久不可用**（微信提示页：已删除/违规/暂不可看）：
    /// `detail_done=-1` 不再进任何补采候选；原因存 detail_error 供前端展示。
    pub fn mark_article_unavailable(&self, id: i64, reason: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE articles SET detail_done=-1, detail_error=?, updated_at=? WHERE id=?",
            params![reason, now(), id],
        )?;
        Ok(())
    }

    /// 取一条已成功补过详情的文章 URL（已验证有效），作为限流判定的**对照 URL**：
    /// 某篇命中验证页时，再抓它一次——对照也验证页才是真限流；对照正常则是该篇链接的问题
    /// （无效 sn 的短链会稳定返回验证页，实测曾把批量补采误判成限流）。
    pub fn any_detail_done_url(&self) -> Result<Option<String>> {
        let conn = self.conn();
        let url = conn
            .query_row(
                "SELECT content_url FROM articles \
                 WHERE is_deleted=0 AND detail_done=1 AND content_url IS NOT NULL AND content_url != '' \
                 ORDER BY id LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        Ok(url)
    }

    /// 记一次补详情失败：attempts+1、存原因；满 3 次后自动批量不再重试（手动单篇仍可）。
    pub fn bump_detail_attempt(&self, id: i64, error: &str) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "UPDATE articles SET detail_attempts=detail_attempts+1, detail_error=?, updated_at=? WHERE id=?",
            params![error, now(), id],
        )?;
        let attempts: i64 = conn.query_row(
            "SELECT detail_attempts FROM articles WHERE id=?",
            params![id],
            |r| r.get(0),
        )?;
        Ok(attempts)
    }

    /// 补详情成功后清除失败痕迹（attempts / error 归零）。
    pub fn clear_detail_failure(&self, id: i64) -> Result<()> {
        self.conn().execute(
            "UPDATE articles SET detail_attempts=0, detail_error=NULL WHERE id=?",
            params![id],
        )?;
        Ok(())
    }

    /// 软删除一个公众号，并级联软删除其全部文章。返回本次被删除的文章数。
    pub fn soft_delete_account(&self, biz: &str) -> Result<i64> {
        let conn = self.conn();
        conn.execute("UPDATE accounts SET is_deleted=1 WHERE biz=?", params![biz])?;
        // 该号进行中的历史抓取任务一并取消。
        conn.execute(
            "UPDATE history_jobs SET status='cancelled', paused_reason=NULL, finished_at=?, resume_at=NULL, \
             next_page_at=NULL WHERE biz=? AND status IN ('running','paused')",
            params![now(), biz],
        )?;
        let n = conn.execute(
            "UPDATE articles SET is_deleted=1, updated_at=? WHERE biz=? AND is_deleted=0",
            params![now(), biz],
        )?;
        Ok(n as i64)
    }

    /// **物理删除**一篇文章（DELETE 行，不可恢复），其评论一并删除。
    /// 返回是否真的删掉了（id 不存在返回 false）。
    /// 与软删除（is_deleted=1）不同：行彻底移除后，之后若重新采集到同一篇
    /// （biz+mid+idx），会当作新文章重新入库。
    pub fn hard_delete_article(&self, id: i64) -> Result<bool> {
        let conn = self.conn();
        conn.execute("DELETE FROM comments WHERE article_id=?", params![id])?;
        let n = conn.execute("DELETE FROM articles WHERE id=?", params![id])?;
        Ok(n > 0)
    }

    // -----------------------------------------------------------------
    // jobs（采集任务留档：巡检批次；老库里的手动任务行只读）
    // -----------------------------------------------------------------
    /// 按一条 [`Job`] 落库：链接 / 最后更新时间 / 来源（`kind`）。编排器 `process_job` 入口在任务
    /// 没有 `local_id` 时用（巡检合成批次 / 单测）。
    pub fn create_job_for(&self, job: &crate::model::Job) -> Result<i64> {
        self.create_job_full(
            &job.links,
            job.last_updated_at,
            &job.link_since,
            job.kind.as_str(),
        )
    }

    fn create_job_full(
        &self,
        links: &[String],
        last_updated_at: Option<Epoch>,
        link_since: &std::collections::HashMap<String, Epoch>,
        kind: &str,
    ) -> Result<i64> {
        let now = now();
        let deduped: Vec<&String> = {
            let mut seen = std::collections::HashSet::new();
            links.iter().filter(|u| seen.insert((*u).clone())).collect()
        };
        let payload = serde_json::to_string(&deduped)?;
        let since_json = if last_updated_at.is_none() && link_since.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&serde_json::json!({
                "task": last_updated_at,
                "links": link_since,
            }))?)
        };
        let conn = self.conn();
        conn.execute(
            "INSERT INTO jobs(links_json, since_json, status, received_at, kind) VALUES(?,?,?,?,?)",
            params![payload, since_json, "received", now, kind],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// 记任务开始处理的时刻（`process_job` 入口）。
    pub fn set_job_started(&self, job_id: i64, started_at: Epoch) -> Result<()> {
        self.conn().execute(
            "UPDATE jobs SET started_at=? WHERE id=?",
            params![started_at, job_id],
        )?;
        Ok(())
    }

    /// 记任务处理结束：结束时刻 + 结果分类（`JOB_OUTCOME_*`）+ 错误原因（有则覆盖 `feedback`）+
    /// 回报 HTTP 状态码。`status` 流转由 `set_job_status` / `set_job_result` 另记，这里不动。
    pub fn finish_job(
        &self,
        job_id: i64,
        finished_at: Epoch,
        outcome: &str,
        error: Option<&str>,
        report_http_status: Option<i64>,
        phases: &[JobPhase],
    ) -> Result<()> {
        let phases_json = if phases.is_empty() {
            None
        } else {
            Some(serde_json::to_string(phases)?)
        };
        self.conn().execute(
            "UPDATE jobs SET finished_at=?, outcome=?, report_http_status=?, phases_json=?, \
             feedback=COALESCE(?, feedback) WHERE id=?",
            params![
                finished_at,
                outcome,
                report_http_status,
                phases_json,
                error,
                job_id
            ],
        )?;
        Ok(())
    }

    /// 解 `phases_json`（缺失 / 坏数据按空）。
    fn parse_phases(raw: Option<String>) -> Vec<JobPhase> {
        raw.and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// 「任务列表」页：按 id 倒序翻页；`kind` = `manual` / `sweep` / None（全部）。
    /// 每行只带计数（链接数 / 回报链接数 / 交回链接数），正文用 [`Self::get_job`] 取。
    pub fn list_jobs(
        &self,
        kind: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<JobListItem>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, upstream_id, kind, status, outcome, feedback, received_at, started_at, finished_at, \
                    reported_at, links_json, result_json, report_http_status, phases_json \
             FROM jobs WHERE (?1 IS NULL OR kind=?1) ORDER BY id DESC LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt.query_map(params![kind, limit, offset], |r| {
            let links_json: String = r.get(10)?;
            let result_json: Option<String> = r.get(11)?;
            let links = serde_json::from_str::<Vec<String>>(&links_json)
                .map(|v| v.len() as i64)
                .unwrap_or(0);
            let result = result_json.and_then(|s| serde_json::from_str::<Value>(&s).ok());
            let count = |key: &str| -> i64 {
                result
                    .as_ref()
                    .and_then(|v| v.get(key))
                    .and_then(Value::as_array)
                    .map(|a| a.len() as i64)
                    .unwrap_or(0)
            };
            let reported_accounts = result
                .as_ref()
                .and_then(|v| v.get("accounts"))
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter(|x| x.get("reported").and_then(Value::as_bool).unwrap_or(false))
                        .count() as i64
                })
                .unwrap_or(0);
            Ok(JobListItem {
                id: r.get(0)?,
                upstream_id: r.get(1)?,
                kind: r.get(2)?,
                status: r.get(3)?,
                outcome: r.get(4)?,
                error: r.get::<_, Option<String>>(5)?.filter(|s| !s.is_empty()),
                received_at: r.get(6)?,
                started_at: r.get(7)?,
                finished_at: r.get(8)?,
                reported_at: r.get(9)?,
                links,
                urls: count("urls"),
                remaining: count("remaining_links"),
                truncated: result
                    .as_ref()
                    .and_then(|v| v.get("truncated"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                accounts: count("accounts"),
                reported_accounts,
                report_http_status: r.get(12)?,
                phases: Self::parse_phases(r.get(13)?),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// 任务总数（翻页用；`kind` 同 [`Self::list_jobs`]）。
    pub fn count_jobs(&self, kind: Option<&str>) -> Result<i64> {
        Ok(self.conn().query_row(
            "SELECT COUNT(*) FROM jobs WHERE (?1 IS NULL OR kind=?1)",
            params![kind],
            |r| r.get(0),
        )?)
    }

    /// 解 `since_json` → (任务级, 链接级)。
    fn parse_since_json(
        raw: Option<String>,
    ) -> (Option<Epoch>, std::collections::HashMap<String, Epoch>) {
        let Some(v) = raw.and_then(|s| serde_json::from_str::<Value>(&s).ok()) else {
            return (None, std::collections::HashMap::new());
        };
        let task = v.get("task").and_then(Value::as_f64);
        let links = v
            .get("links")
            .cloned()
            .and_then(|l| serde_json::from_value(l).ok())
            .unwrap_or_default();
        (task, links)
    }

    /// 更新任务状态/反馈。
    pub fn set_job_status(&self, job_id: i64, status: &str, feedback: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE jobs SET status=?, feedback=? WHERE id=?",
            params![status, feedback, job_id],
        )?;
        Ok(())
    }

    /// 写入任务结果并置状态（默认 reported），记 reported_at。
    pub fn set_job_result(&self, job_id: i64, result: &Value, status: &str) -> Result<()> {
        let json = serde_json::to_string(result)?;
        self.conn().execute(
            "UPDATE jobs SET result_json=?, status=?, reported_at=? WHERE id=?",
            params![json, status, now(), job_id],
        )?;
        Ok(())
    }

    /// 读取一条任务（links / result 解析成结构化值）。
    pub fn get_job(&self, job_id: i64) -> Result<Option<JobRow>> {
        let conn = self.conn();
        let row = conn
            .query_row(
                "SELECT id, upstream_id, links_json, status, result_json, feedback, received_at, reported_at, since_json, \
                        kind, raw_json, started_at, finished_at, outcome, report_http_status, phases_json \
                 FROM jobs WHERE id=?",
                params![job_id],
                |r| {
                    let links_json: String = r.get(2)?;
                    let result_json: Option<String> = r.get(4)?;
                    let raw_json: Option<String> = r.get(10)?;
                    Ok((
                        JobRow {
                            id: r.get(0)?,
                            upstream_id: r.get(1)?,
                            links: serde_json::from_str(&links_json).unwrap_or_default(),
                            last_updated_at: None,
                            link_since: std::collections::HashMap::new(),
                            status: r.get(3)?,
                            result: result_json.and_then(|s| serde_json::from_str(&s).ok()),
                            feedback: r.get(5)?,
                            received_at: r.get(6)?,
                            reported_at: r.get(7)?,
                            kind: r.get(9)?,
                            raw: raw_json.and_then(|s| serde_json::from_str(&s).ok()),
                            started_at: r.get(11)?,
                            finished_at: r.get(12)?,
                            outcome: r.get(13)?,
                            report_http_status: r.get(14)?,
                            phases: Self::parse_phases(r.get(15)?),
                        },
                        r.get::<_, Option<String>>(8)?,
                    ))
                },
            )
            .optional()?;
        let Some((mut job, since_json)) = row else {
            return Ok(None);
        };
        let (last_updated_at, link_since) = Self::parse_since_json(since_json);
        job.last_updated_at = last_updated_at;
        job.link_since = link_since;
        Ok(Some(job))
    }

    /// 「进行中」的判据（SQL 片段）：还没有结果分类且流转状态处于处理阶段。删除 / 清空都跳过这些行，
    /// 否则编排器随后的 `set_job_result` / `finish_job` 会写到一条不存在的行上、留档缺失。
    const JOB_RUNNING_SQL: &'static str =
        "(outcome IS NULL AND status IN ('received','capturing','collecting'))";

    /// **物理删除**一条任务（GUI「任务列表」逐条删除，前端已二次确认）。进行中的任务拒绝删除（报错）；
    /// 不存在返回 `false`。连带清掉 `relay` 表里挂在它名下的残留行；`run_log` / `list_call_log` / `app_log`
    /// 里的 `job_id` 只是留档引用，各有自己的保留期，不动。
    pub fn delete_job(&self, job_id: i64) -> Result<bool> {
        let conn = self.conn();
        let running: Option<bool> = conn
            .query_row(
                &format!("SELECT {} FROM jobs WHERE id=?", Self::JOB_RUNNING_SQL),
                params![job_id],
                |r| r.get(0),
            )
            .optional()?;
        match running {
            None => return Ok(false),
            Some(true) => anyhow::bail!("任务 #{job_id} 正在处理中，跑完后再删除"),
            Some(false) => {}
        }
        let n = conn.execute("DELETE FROM jobs WHERE id=?", params![job_id])?;
        Ok(n > 0)
    }

    /// **一键清空**：物理删除全部已结束的任务（进行中的保留），返回删掉的条数。前端已二次确认。
    pub fn clear_finished_jobs(&self) -> Result<usize> {
        let conn = self.conn();
        Ok(conn.execute(
            &format!("DELETE FROM jobs WHERE NOT {}", Self::JOB_RUNNING_SQL),
            [],
        )?)
    }

    /// 最近的任务（不含 links/result，对齐 Python recent_jobs 的裁剪）。
    pub fn recent_jobs(&self, limit: i64) -> Result<Vec<JobRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, upstream_id, status, feedback, received_at, reported_at \
             FROM jobs ORDER BY id DESC LIMIT ?",
        )?;
        let rows = stmt.query_map(params![limit], |r| {
            Ok(JobRow {
                id: r.get(0)?,
                upstream_id: r.get(1)?,
                status: r.get(2)?,
                feedback: r.get(3)?,
                received_at: r.get(4)?,
                reported_at: r.get(5)?,
                ..Default::default()
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // -----------------------------------------------------------------
}

/// 从 [`ArticleFields`] 收集“非空”列（对齐 Python `upsert_article` 的 payload 过滤）。
fn article_payload(f: &ArticleFields) -> Vec<(&'static str, SqlValue)> {
    let mut p: Vec<(&'static str, SqlValue)> = Vec::new();
    let push_text = |c: &'static str, v: &Option<String>, p: &mut Vec<(&'static str, SqlValue)>| {
        if let Some(x) = v {
            p.push((c, SqlValue::Text(x.clone())));
        }
    };
    push_text("sn", &f.sn, &mut p);
    push_text("title", &f.title, &mut p);
    push_text("author", &f.author, &mut p);
    push_text("digest", &f.digest, &mut p);
    push_text("content_url", &f.content_url, &mut p);
    push_text("cover", &f.cover, &mut p);
    if let Some(x) = f.published_at {
        p.push(("published_at", SqlValue::Real(x)));
    }
    push_text("content_html", &f.content_html, &mut p);
    push_text("content_text", &f.content_text, &mut p);
    push_text("content_md", &f.content_md, &mut p);
    for (c, v) in [
        ("read_num", f.read_num),
        ("old_like_num", f.old_like_num),
        ("like_num", f.like_num),
        ("comment_count", f.comment_count),
        ("is_deleted", f.is_deleted),
        ("detail_done", f.detail_done),
        ("stat_done", f.stat_done),
    ] {
        if let Some(x) = v {
            p.push((c, SqlValue::Integer(x)));
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CredentialFields;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn test_account_and_credential_roundtrip() {
        let store = Store::open_in_memory().unwrap();
        store.upsert_account("BIZ==", Some("测试号"), None).unwrap();
        // nickname None 不覆盖
        store
            .upsert_account("BIZ==", None, Some("head.jpg"))
            .unwrap();
        let accts = store.list_accounts().unwrap();
        assert_eq!(accts.len(), 1);
        assert_eq!(accts[0].nickname.as_deref(), Some("测试号"));
        assert_eq!(accts[0].round_head_img.as_deref(), Some("head.jpg"));

        store
            .upsert_credential(
                "BIZ==",
                &CredentialFields {
                    uin: Some("U".into()),
                    key: Some("K".into()),
                    cookie: Some("c=1".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        // 空 key 不覆盖真实 key（COALESCE）
        store
            .upsert_credential(
                "BIZ==",
                &CredentialFields {
                    pass_ticket: Some("PT".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let cred = store.get_credential("BIZ==").unwrap().unwrap();
        assert_eq!(cred.key.as_deref(), Some("K"));
        assert_eq!(cred.uin.as_deref(), Some("U"));
        assert_eq!(cred.pass_ticket.as_deref(), Some("PT"));
        assert!(store.credential_is_fresh("BIZ==", 1800).unwrap());
        assert!(!store.credential_is_fresh("NOPE==", 1800).unwrap());
        assert!(!store.credential_is_fresh("BIZ==", 0).unwrap()); // ttl=0 -> 立即过期
    }

    #[test]
    fn test_credential_is_fresh_requires_current_uin() {
        // 当前微信号 = 激活的 wx_accounts 条目绑定的 uin：旧号（U1）抓的凭证没过 TTL，但激活号换成 U2 后
        // 它就不再新鲜；没带 uin 的凭证不参与比较（兼容旧数据 / 只抓到 key 的情况）。
        let store = Store::open_in_memory().unwrap();
        let cred = |uin: Option<&str>| CredentialFields {
            uin: uin.map(str::to_string),
            key: Some("K".into()),
            ..Default::default()
        };
        store.upsert_credential("OLD==", &cred(Some("U1"))).unwrap();
        store.upsert_credential("NOUIN==", &cred(None)).unwrap();
        // 没有激活号：不比较 uin
        assert_eq!(store.current_uin().unwrap(), None);
        assert!(store.credential_is_fresh("OLD==", 1800).unwrap());
        // 抓到 U1 → 自动登记并激活
        let a = match store.wx_note_captured_uin("U1").unwrap() {
            WxBind::Registered {
                id,
                activated: true,
            } => id,
            other => panic!("{other:?}"),
        };
        assert_eq!(store.current_uin().unwrap().as_deref(), Some("U1"));
        assert!(store.credential_is_fresh("OLD==", 1800).unwrap());
        // 抓到 U2：已有激活号，登记但不激活；手动激活它
        let b = match store.wx_note_captured_uin("U2").unwrap() {
            WxBind::Registered {
                id,
                activated: false,
            } => id,
            other => panic!("{other:?}"),
        };
        store.wx_activate(b).unwrap();
        store.upsert_credential("NEW==", &cred(Some("U2"))).unwrap();
        assert_eq!(store.current_uin().unwrap().as_deref(), Some("U2"));
        assert!(
            !store.credential_is_fresh("OLD==", 1800).unwrap(),
            "旧号凭证不复用"
        );
        assert!(store.credential_is_fresh("NEW==", 1800).unwrap());
        assert!(
            store.credential_is_fresh("NOUIN==", 1800).unwrap(),
            "无 uin 不比较"
        );
        // 旧号那个 biz 用新号重新抓到 key → 又新鲜了
        store.upsert_credential("OLD==", &cred(Some("U2"))).unwrap();
        assert!(store.credential_is_fresh("OLD==", 1800).unwrap());
        // 切回甲：U1 的凭证又算当前号
        store.wx_activate(a).unwrap();
        assert_eq!(store.current_uin().unwrap().as_deref(), Some("U1"));
        assert!(!store.credential_is_fresh("NEW==", 1800).unwrap());
    }

    #[test]
    fn test_wx_note_captured_uin_branches() {
        let store = Store::open_in_memory().unwrap();
        assert!(store.wx_note_captured_uin("  ").is_err());
        // 首次见到 U1：登记 + 激活，别名「微信号 N」
        let id = match store.wx_note_captured_uin("U1").unwrap() {
            WxBind::Registered {
                id,
                activated: true,
            } => id,
            other => panic!("{other:?}"),
        };
        let a = store.wx_get(id).unwrap().unwrap();
        assert!(a.is_active);
        assert_eq!(a.alias, format!("微信号 {id}"));
        assert_eq!(a.uin, "U1");
        assert!(a.last_captured_at.is_some());
        // 相同 → Same
        assert_eq!(store.wx_note_captured_uin("U1").unwrap(), WxBind::Same);
        // 新 uin 但已有激活号 → 登记、不激活
        let b = match store.wx_note_captured_uin("U2").unwrap() {
            WxBind::Registered {
                id,
                activated: false,
            } => id,
            other => panic!("{other:?}"),
        };
        assert!(!store.wx_get(b).unwrap().unwrap().is_active);
        assert_eq!(store.wx_list().unwrap().len(), 2);
        // 再抓到 U2：已登记但不是激活号 → Mismatch（带两边别名）
        assert_eq!(
            store.wx_note_captured_uin("U2").unwrap(),
            WxBind::Mismatch {
                alias: format!("微信号 {id}"),
                captured_alias: format!("微信号 {b}"),
            }
        );
        // uin 唯一：直接插重复的 uin 会被拒
        assert!(store
            .conn()
            .execute(
                "INSERT INTO wx_accounts(alias, uin, created_at) VALUES('x', 'U1', 0)",
                [],
            )
            .is_err());
        // 改别名 / 备注
        store.wx_update(b, " 乙 ", Some("  ")).unwrap();
        let bb = store.wx_get(b).unwrap().unwrap();
        assert_eq!(bb.alias, "乙");
        assert_eq!(bb.note, None);
        // 激活互斥
        store.wx_activate(b).unwrap();
        assert!(!store.wx_get(id).unwrap().unwrap().is_active);
        assert_eq!(store.wx_active().unwrap().unwrap().id, b);
        assert_eq!(store.wx_list().unwrap()[0].id, b, "激活的排最前");
        // 封 / 解封
        store.wx_set_blocked(b, now() + 100.0, "ret=-6").unwrap();
        assert!(store.wx_get(b).unwrap().unwrap().blocked_until.is_some());
        store.wx_unblock(b).unwrap();
        assert!(store.wx_get(b).unwrap().unwrap().blocked_until.is_none());
        // 删除激活号后没有激活号：再抓到 U1（已登记、未激活）→ 启用它
        store.wx_delete(b).unwrap();
        assert!(store.wx_get(b).unwrap().is_none());
        assert!(store.wx_delete(b).is_err());
        assert!(store.wx_active().unwrap().is_none());
        assert_eq!(
            store.wx_note_captured_uin("U1").unwrap(),
            WxBind::Registered {
                id,
                activated: true
            }
        );
        assert_eq!(store.wx_active().unwrap().unwrap().id, id);
    }

    #[test]
    fn test_migrate_wx_accounts_from_credentials() {
        let store = Store::open_in_memory().unwrap();
        let cred = |uin: &str| CredentialFields {
            uin: Some(uin.into()),
            key: Some("K".into()),
            ..Default::default()
        };
        // 老号 U8 先抓、新号 U9 后抓（captured_at 更新）
        store.upsert_credential("A==", &cred("U8")).unwrap();
        store
            .conn()
            .execute("UPDATE credentials SET captured_at = captured_at - 100", [])
            .unwrap();
        store.upsert_credential("B==", &cred("U9")).unwrap();
        store.upsert_credential("C==", &cred("U9")).unwrap();
        store
            .set_config(
                "cooldown_state",
                &serde_json::json!({"until": now() + 3600.0, "level": 1, "reason": "ret=-6", "kind": "account"}),
            )
            .unwrap();
        // 再跑一次 init_db 触发迁移（真实场景是打开老库）
        store.init_db().unwrap();
        let list = store.wx_list().unwrap();
        assert_eq!(list.len(), 2, "每个不同 uin 一条");
        let active = &list[0];
        assert!(active.is_active);
        assert_eq!(active.uin, "U9", "最近抓到的那个号激活");
        assert_eq!(active.alias, format!("微信号 {}", active.id));
        assert!(active.blocked_until.is_some(), "账号类退避迁到激活号");
        assert!(!list[1].is_active);
        assert_eq!(list[1].uin, "U8");
        assert!(list[1].blocked_until.is_none());
        // 幂等
        store.init_db().unwrap();
        assert_eq!(store.wx_list().unwrap().len(), 2);
    }

    #[test]
    fn test_migrate_wx_accounts_drops_unbound_rows() {
        // 早期版本允许没绑 uin 的手动条目：迁移时删掉，并补上唯一索引。
        let store = Store::open_in_memory().unwrap();
        store
            .conn()
            .execute_batch(
                "DROP INDEX IF EXISTS idx_wx_accounts_uin; DROP TABLE wx_accounts; \
                 CREATE TABLE wx_accounts (id INTEGER PRIMARY KEY AUTOINCREMENT, alias TEXT NOT NULL, uin TEXT, \
                 is_active INTEGER NOT NULL DEFAULT 0, created_at REAL NOT NULL, last_captured_at REAL, \
                 blocked_until REAL, blocked_reason TEXT, note TEXT); \
                 INSERT INTO wx_accounts(alias, uin, is_active, created_at) VALUES('空', NULL, 1, 0); \
                 INSERT INTO wx_accounts(alias, uin, is_active, created_at) VALUES('有', 'U1', 0, 0);",
            )
            .unwrap();
        store.init_db().unwrap();
        let list = store.wx_list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].uin, "U1");
        assert!(store
            .conn()
            .execute(
                "INSERT INTO wx_accounts(alias, uin, created_at) VALUES('重复', 'U1', 0)",
                [],
            )
            .is_err());
    }

    #[test]
    fn test_migrate_wx_accounts_dedupes_same_uin_before_unique_index() {
        // 早期版本可能把同一个 uin 绑到两条记录（A 已绑 U1，激活未绑定的 B 再抓到 U1）：
        // 建唯一索引前先去重，保留激活的那条，应用才能启动。
        let store = Store::open_in_memory().unwrap();
        store
            .conn()
            .execute_batch(
                "DROP INDEX IF EXISTS idx_wx_accounts_uin; DROP TABLE wx_accounts; \
                 CREATE TABLE wx_accounts (id INTEGER PRIMARY KEY AUTOINCREMENT, alias TEXT NOT NULL, uin TEXT, \
                 is_active INTEGER NOT NULL DEFAULT 0, created_at REAL NOT NULL, last_captured_at REAL, \
                 blocked_until REAL, blocked_reason TEXT, note TEXT); \
                 INSERT INTO wx_accounts(alias, uin, is_active, created_at, last_captured_at) VALUES('A', 'U1', 0, 0, 200); \
                 INSERT INTO wx_accounts(alias, uin, is_active, created_at, last_captured_at) VALUES('B', 'U1', 1, 0, 100); \
                 INSERT INTO wx_accounts(alias, uin, is_active, created_at, last_captured_at) VALUES('C', 'U2', 0, 0, 5); \
                 INSERT INTO wx_accounts(alias, uin, is_active, created_at, last_captured_at) VALUES('D', 'U2', 0, 0, 9);",
            )
            .unwrap();
        store.init_db().expect("同 uin 去重后建索引不应失败");
        let list = store.wx_list().unwrap();
        let mut names: Vec<_> = list.iter().map(|a| a.alias.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["B", "D"], "U1 留激活的 B，U2 留最近抓到的 D");
        assert!(store.wx_active().unwrap().is_some_and(|a| a.alias == "B"));
        // 再跑一遍幂等
        store.init_db().unwrap();
        assert_eq!(store.wx_list().unwrap().len(), 2);
    }

    #[test]
    fn test_retire_legacy_manual_jobs_on_open() {
        // 老库里没跑完的手动任务（received / capturing / collecting，无 outcome）重开库要收口成 error 留档；
        // 已结束的不动；巡检批次不动。
        let store = Store::open_in_memory().unwrap();
        let mk = |kind: &str, status: &str| -> i64 {
            store
                .conn()
                .execute(
                    "INSERT INTO jobs(links_json, status, received_at, kind) VALUES('[]', ?, 1.0, ?)",
                    params![status, kind],
                )
                .unwrap();
            store.conn().last_insert_rowid()
        };
        let a = mk("manual", "received");
        let b = mk("upstream", "collecting");
        let c = mk("manual", "done");
        store
            .conn()
            .execute("UPDATE jobs SET outcome='ok' WHERE id=?", params![c])
            .unwrap();
        let d = mk("sweep", "received");
        store.init_db().unwrap();
        for id in [a, b] {
            let row = store.get_job(id).unwrap().unwrap();
            assert_eq!(row.status, "error", "#{id}");
            assert_eq!(row.outcome.as_deref(), Some("error"));
            assert!(row.feedback.as_deref().unwrap().contains("已下线"));
        }
        assert_eq!(store.get_job(c).unwrap().unwrap().status, "done");
        assert_eq!(store.get_job(d).unwrap().unwrap().status, "received");
        // 收口后可以删除（不再算「进行中」）
        assert!(store.delete_job(a).unwrap());
    }

    #[test]
    fn test_list_accounts_page() {
        let store = Store::open_in_memory().unwrap();
        for i in 0..5 {
            store
                .upsert_account(&format!("B{i}=="), Some(&format!("号{i}")), None)
                .unwrap();
        }
        assert_eq!(store.count_accounts().unwrap(), 5);
        assert_eq!(store.list_accounts().unwrap().len(), 5);
        let p1 = store.list_accounts_page(2, 0).unwrap();
        let p2 = store.list_accounts_page(2, 2).unwrap();
        let p3 = store.list_accounts_page(2, 4).unwrap();
        assert_eq!((p1.len(), p2.len(), p3.len()), (2, 2, 1));
        // 三页拼起来正好是全量、无重复
        let mut all: Vec<String> = p1.into_iter().chain(p2).chain(p3).map(|a| a.biz).collect();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 5);
        // 越界页为空；软删除后不再计数
        assert!(store.list_accounts_page(2, 10).unwrap().is_empty());
        store.soft_delete_account("B0==").unwrap();
        assert_eq!(store.count_accounts().unwrap(), 4);
    }

    #[test]
    fn test_latest_published_by_biz() {
        let store = Store::open_in_memory().unwrap();
        for (mid, ts) in [("1", Some(100.0)), ("2", Some(300.0)), ("3", None)] {
            store
                .upsert_article(
                    "A==",
                    mid,
                    1,
                    &ArticleFields {
                        published_at: ts,
                        ..Default::default()
                    },
                )
                .unwrap();
        }
        // B== 只有一篇没有发布时间的文章 → 不出现在结果里
        store
            .upsert_article("B==", "1", 1, &ArticleFields::default())
            .unwrap();
        let m = store.latest_published_by_biz().unwrap();
        assert_eq!(m.get("A=="), Some(&300.0));
        assert!(!m.contains_key("B=="));
    }

    #[test]
    fn test_credential_hot_fields_and_log() {
        let store = Store::open_in_memory().unwrap();
        store.set_cred_ttl(600);
        let k1 = CredentialFields {
            uin: Some("U".into()),
            key: Some("K1".into()),
            ..Default::default()
        };
        store.upsert_credential("BIZ==", &k1).unwrap();
        let c = store.get_credential("BIZ==").unwrap().unwrap();
        // expires_at = captured_at + ttl；首次 key 算一次换 key
        assert!((c.expires_at.unwrap() - c.captured_at - 600.0).abs() < 1e-6);
        assert_eq!(c.refresh_count, 1);
        assert!(c.secs_to_expiry(c.captured_at + 100.0, 600) > 499.0);

        // 使用打点
        store.touch_credential_used("BIZ==").unwrap();
        store.touch_credential_used("BIZ==").unwrap();
        let c = store.get_credential("BIZ==").unwrap().unwrap();
        assert_eq!(c.use_count, 2);
        assert!(c.last_used_at.is_some());

        // 实测失效：TTL 未到也判不新鲜；留档同步
        store.mark_credential_invalid("BIZ==").unwrap();
        assert!(!store.credential_is_fresh("BIZ==", 1800).unwrap());
        let log = store.credential_log(Some("BIZ=="), 10).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].key.as_deref(), Some("K1"));
        assert_eq!(log[0].use_count, 2);
        assert!(log[0].invalidated_at.is_some());

        // 同 key 重复观测：不算换 key、不清失效标记、留档不新增
        store.upsert_credential("BIZ==", &k1).unwrap();
        let c = store.get_credential("BIZ==").unwrap().unwrap();
        assert_eq!(c.refresh_count, 1);
        assert!(!store.credential_is_fresh("BIZ==", 1800).unwrap());
        assert_eq!(store.credential_log(Some("BIZ=="), 10).unwrap().len(), 1);

        // 换 key：refresh_count+1、失效清空、重新新鲜、留档多一行且旧行保留
        let k2 = CredentialFields {
            key: Some("K2".into()),
            ..Default::default()
        };
        store.upsert_credential("BIZ==", &k2).unwrap();
        let c = store.get_credential("BIZ==").unwrap().unwrap();
        assert_eq!(c.refresh_count, 2);
        assert_eq!(c.use_count, 0);
        assert!(c.invalidated_at.is_none());
        assert!(store.credential_is_fresh("BIZ==", 1800).unwrap());
        let log = store.credential_log(Some("BIZ=="), 10).unwrap();
        assert_eq!(log.len(), 2);
        assert!(log.iter().any(|r| r.key.as_deref() == Some("K1")));
        // 只有 pass_ticket 的占位写入：不动 key、不算换 key
        store
            .upsert_credential(
                "BIZ==",
                &CredentialFields {
                    pass_ticket: Some("PT".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let c = store.get_credential("BIZ==").unwrap().unwrap();
        assert_eq!(c.key.as_deref(), Some("K2"));
        assert_eq!(c.refresh_count, 2);
        assert_eq!(store.list_credentials().unwrap().len(), 1);
        // 有效期统计：只有 K1 有实测失效样本（间隔可能 0 秒被过滤，故只看上界）
        let (n, _, _, _) = store.credential_lifetime_stats().unwrap();
        assert!(n <= 1);
    }

    #[test]
    fn test_create_job_with_since_roundtrip() {
        use crate::model::Job;
        let store = Store::open_in_memory().unwrap();
        let mut ls = std::collections::HashMap::new();
        ls.insert("u1".to_string(), 100.0);
        let id = store
            .create_job_for(&Job {
                links: s(&["u1", "u2"]),
                last_updated_at: Some(50.0),
                link_since: ls,
                ..Default::default()
            })
            .unwrap();
        let row = store.get_job(id).unwrap().unwrap();
        assert_eq!(row.last_updated_at, Some(50.0));
        assert_eq!(row.link_since.get("u1"), Some(&100.0));
        assert!(!row.link_since.contains_key("u2"));
        // 不带 since：为空
        let id2 = store
            .create_job_for(&Job {
                links: s(&["u3"]),
                ..Default::default()
            })
            .unwrap();
        let row2 = store.get_job(id2).unwrap().unwrap();
        assert_eq!(row2.last_updated_at, None);
        assert!(row2.link_since.is_empty());
    }

    #[test]
    fn test_article_upsert_and_list() {
        let store = Store::open_in_memory().unwrap();
        let id1 = store
            .upsert_article(
                "BIZ==",
                "100",
                1,
                &ArticleFields {
                    title: Some("标题".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        // 同 (biz,mid,idx) 复用 id，补充字段
        let id2 = store
            .upsert_article(
                "BIZ==",
                "100",
                1,
                &ArticleFields {
                    author: Some("作者".into()),
                    detail_done: Some(1),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(id1, id2);
        let rows = store.list_articles(Some("BIZ=="), 50, 0, false).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title.as_deref(), Some("标题"));
        assert_eq!(rows[0].author.as_deref(), Some("作者"));
        assert_eq!(rows[0].detail_done, 1);
        // only_detail 过滤
        let none = store.list_articles(None, 50, 0, true).unwrap();
        assert_eq!(none.len(), 1);
    }

    #[test]
    fn test_hard_delete_article() {
        let store = Store::open_in_memory().unwrap();
        let id = store
            .upsert_article(
                "BIZ==",
                "100",
                1,
                &ArticleFields {
                    title: Some("待删".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(store.hard_delete_article(id).unwrap());
        assert!(store
            .list_articles(Some("BIZ=="), 50, 0, false)
            .unwrap()
            .is_empty());
        // 已删除 / 不存在的 id 返回 false
        assert!(!store.hard_delete_article(id).unwrap());
        // 物理删除后重新采集同一篇会当新文章入库（新 id）
        let id2 = store
            .upsert_article("BIZ==", "100", 1, &ArticleFields::default())
            .unwrap();
        assert_ne!(id, id2);
    }

    #[test]
    fn test_store_jobs() {
        use crate::model::Job;
        let store = Store::open_in_memory().unwrap();
        let mk = |links: &[&str]| Job {
            links: s(links),
            ..Default::default()
        };
        let jid = store.create_job_for(&mk(&["l1", "l2"])).unwrap();
        // 每次登记都新建一行
        let jid2 = store.create_job_for(&mk(&["l3"])).unwrap();
        assert_ne!(jid2, jid);
        assert_eq!(store.get_job(jid).unwrap().unwrap().kind, "sweep");
        assert_eq!(store.get_job(jid2).unwrap().unwrap().links, vec!["l3"]);
        assert_eq!(store.count_jobs(None).unwrap(), 2);
        store.set_job_status(jid, "capturing", "").unwrap();
        store
            .set_job_result(jid, &serde_json::json!({"accounts": []}), "reported")
            .unwrap();
        let j = store.get_job(jid).unwrap().unwrap();
        assert_eq!(j.status, "reported");
        assert_eq!(j.links, vec!["l1", "l2"]);
        assert_eq!(j.result, Some(serde_json::json!({"accounts": []})));
        assert_eq!(store.recent_jobs(20).unwrap()[0].id, jid2);
        store.set_job_status(jid2, "error", "x").unwrap();
        assert_eq!(store.get_job(jid2).unwrap().unwrap().status, "error");
    }

    /// 老库迁移：jobs 表原带 `UNIQUE(upstream_id)`，再次 `init_db` 后约束被去掉（表重建），
    /// 老行（含 id / 结果）原样保留，同一 upstream_id 可再插一行。
    #[test]
    fn test_store_migrate_jobs_drop_unique() {
        let store = Store::open_in_memory().unwrap();
        {
            let conn = store.conn();
            conn.execute_batch(
                "DROP TABLE jobs;
                 CREATE TABLE jobs (
                     id INTEGER PRIMARY KEY AUTOINCREMENT, upstream_id TEXT, links_json TEXT NOT NULL,
                     since_json TEXT, status TEXT NOT NULL DEFAULT 'received', result_json TEXT, feedback TEXT,
                     received_at REAL NOT NULL, reported_at REAL, kind TEXT NOT NULL DEFAULT 'upstream',
                     raw_json TEXT, started_at REAL, finished_at REAL, outcome TEXT, report_http_status INTEGER,
                     phases_json TEXT, UNIQUE (upstream_id));
                 INSERT INTO jobs(id, upstream_id, links_json, status, received_at, outcome, result_json)
                     VALUES(7, 'OLD', '[\"a\"]', 'reported', 1.0, 'ok', '{\"urls\":[\"x\"]}');",
            )
            .unwrap();
            // 旧约束生效：同 upstream_id 插不进去
            assert!(conn
                .execute(
                    "INSERT INTO jobs(upstream_id, links_json, received_at) VALUES('OLD', '[]', 2.0)",
                    [],
                )
                .is_err());
        }
        store.init_db().unwrap();
        // 迁移后：老行原样、同 upstream_id 可再登记，且再次 init_db 幂等
        let row = store.get_job(7).unwrap().unwrap();
        assert_eq!(row.upstream_id.as_deref(), Some("OLD"));
        assert_eq!(row.outcome.as_deref(), Some("ok"));
        assert_eq!(row.links, vec!["a"]);
        store
            .conn()
            .execute(
                "INSERT INTO jobs(upstream_id, links_json, received_at) VALUES('OLD', '[\"b\"]', 2.0)",
                [],
            )
            .unwrap();
        store.init_db().unwrap();
        assert_eq!(store.count_jobs(None).unwrap(), 2);
    }

    /// 任务列表：按 Job 落库记来源，起止 / 结果分类 / 上报状态码可写可读，计数正确，按来源筛选。
    #[test]
    fn test_store_job_list_and_finish() {
        use crate::model::{Job, JobKind};
        let store = Store::open_in_memory().unwrap();
        let up = Job {
            links: vec![
                "https://mp.weixin.qq.com/s/a".into(),
                "https://mp.weixin.qq.com/s/b".into(),
            ],
            ..Default::default()
        };
        let jid = store.create_job_for(&up).unwrap();
        store.set_job_started(jid, 1000.0).unwrap();
        store
            .set_job_result(
                jid,
                &serde_json::json!({"urls": ["u1", "u2", "u3"], "truncated": true, "remaining_links": ["r"]}),
                "reported",
            )
            .unwrap();
        store
            .finish_job(
                jid,
                1042.5,
                crate::model::JOB_OUTCOME_TIMEOUT,
                Some("等待凭证到上限"),
                Some(200),
                &[
                    JobPhase {
                        name: "启动代理".into(),
                        ms: 800,
                    },
                    JobPhase {
                        name: "接力抓凭证".into(),
                        ms: 30_000,
                    },
                ],
            )
            .unwrap();
        let sw = Job {
            links: vec!["https://mp.weixin.qq.com/s/c".into()],
            kind: JobKind::Sweep,
            ..Default::default()
        };
        let sid = store.create_job_for(&sw).unwrap();

        // 详情：起止 / 分类 / 状态码回读一致
        let row = store.get_job(jid).unwrap().unwrap();
        assert_eq!(row.kind, "sweep");
        assert!(row.raw.is_none() && row.upstream_id.is_none());
        assert_eq!(row.started_at, Some(1000.0));
        assert_eq!(row.finished_at, Some(1042.5));
        assert_eq!(row.outcome.as_deref(), Some("timeout"));
        assert_eq!(row.feedback.as_deref(), Some("等待凭证到上限"));
        assert_eq!(row.report_http_status, Some(200));
        assert_eq!(row.phases.len(), 2);
        assert_eq!(
            (row.phases[1].name.as_str(), row.phases[1].ms),
            ("接力抓凭证", 30_000)
        );
        let srow = store.get_job(sid).unwrap().unwrap();
        assert_eq!(srow.kind, "sweep");
        assert!(srow.raw.is_none());
        assert!(srow.outcome.is_none());

        // 列表：倒序、计数、筛选
        let all = store.list_jobs(None, 10, 0).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, sid);
        let it = &all[1];
        assert_eq!(
            (it.links, it.urls, it.remaining, it.truncated),
            (2, 3, 1, true)
        );
        assert_eq!(it.phases.len(), 2, "列表行也带阶段耗时（hover 用）");
        assert_eq!(it.error.as_deref(), Some("等待凭证到上限"));
        assert_eq!(store.count_jobs(None).unwrap(), 2);
        assert_eq!(store.count_jobs(Some("sweep")).unwrap(), 2);
        assert_eq!(
            store.count_jobs(Some("manual")).unwrap(),
            0,
            "老库来源筛选仍可用，只是没有行"
        );
        assert_eq!(store.list_jobs(Some("sweep"), 10, 0).unwrap()[0].id, sid);

        // 同一批链接再登记：另起一行，旧行留档不动
        let again = store.create_job_for(&up).unwrap();
        assert_ne!(again, jid);
        let row = store.get_job(again).unwrap().unwrap();
        assert_eq!(row.status, "received");
        assert!(row.finished_at.is_none() && row.outcome.is_none() && row.result.is_none());
        let old_row = store.get_job(jid).unwrap().unwrap();
        assert_eq!(old_row.outcome.as_deref(), Some("timeout"));
        assert!(old_row.result.is_some());
        assert_eq!(store.count_jobs(Some("sweep")).unwrap(), 3);
        // 列表行的按号计数：accounts / 已上报号数
        store
            .set_job_result(
                again,
                &serde_json::json!({"urls": ["u"], "accounts": [
                    {"biz": "A==", "urls": ["u"], "finished": true, "reported": true},
                    {"biz": "B==", "urls": [], "finished": false, "reported": false}
                ]}),
                "reported",
            )
            .unwrap();
        let it = store.list_jobs(Some("sweep"), 10, 0).unwrap();
        let newest = it.iter().find(|x| x.id == again).unwrap();
        assert_eq!((newest.accounts, newest.reported_accounts), (2, 1));
        let older = it.iter().find(|x| x.id == jid).unwrap();
        assert_eq!((older.accounts, older.reported_accounts), (0, 0));
        // finish_job 不带错误原因时保留原 feedback
        store.set_job_status(jid, "error", "原因A").unwrap();
        store
            .finish_job(jid, 2.0, "error", None, None, &[])
            .unwrap();
        assert_eq!(
            store.get_job(jid).unwrap().unwrap().feedback.as_deref(),
            Some("原因A")
        );
    }

    /// 物理删除 / 一键清空：进行中的任务拒删且清空时保留；已结束的删掉并连带 relay 残留；不存在返回 false。
    #[test]
    fn test_store_job_delete_and_clear() {
        use crate::model::Job;
        let store = Store::open_in_memory().unwrap();
        let mk = |uid: &str| Job {
            links: vec![format!("https://mp.weixin.qq.com/s/{uid}")],
            ..Default::default()
        };
        let running = store.create_job_for(&mk("R")).unwrap();
        store.set_job_status(running, "collecting", "").unwrap();
        let done = store.create_job_for(&mk("D")).unwrap();
        store
            .set_job_result(done, &serde_json::json!({"urls": []}), "reported")
            .unwrap();
        store
            .finish_job(done, 1.0, "ok", None, Some(200), &[])
            .unwrap();
        let errored = store.create_job_for(&mk("E")).unwrap();
        store
            .set_job_status(errored, "error", "代理启动失败")
            .unwrap();
        // 进行中：拒绝删除
        assert!(store.delete_job(running).is_err());
        // 已结束：删掉；再删返回 false
        assert!(store.delete_job(done).unwrap());
        assert!(store.get_job(done).unwrap().is_none());
        assert!(!store.delete_job(done).unwrap());
        // 老库里 outcome 为空但 status=error 的行不算进行中，可删 / 可清
        assert_eq!(store.clear_finished_jobs().unwrap(), 1);
        assert!(store.get_job(errored).unwrap().is_none());
        // 进行中的保留
        assert_eq!(store.count_jobs(None).unwrap(), 1);
        assert!(store.get_job(running).unwrap().is_some());
    }

    #[test]
    fn test_create_job_dedup_links() {
        use crate::model::Job;
        let store = Store::open_in_memory().unwrap();
        // 保序去重：重复 a、b 只留第一次出现，顺序 a,b,c
        let jid = store
            .create_job_for(&Job {
                links: s(&["a", "b", "a", "c", "b"]),
                ..Default::default()
            })
            .unwrap();
        let j = store.get_job(jid).unwrap().unwrap();
        assert_eq!(j.links, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_latest_article_urls_falls_back_to_seed_url() {
        let store = Store::open_in_memory().unwrap();
        store.upsert_account("A==", Some("甲"), None).unwrap();
        // 没文章、没种子：空
        assert!(store.latest_article_urls("A==", 5).unwrap().is_empty());
        store
            .set_seed_url("A==", "https://mp.weixin.qq.com/s/seed")
            .unwrap();
        assert_eq!(
            store.latest_article_urls("A==", 5).unwrap(),
            vec!["https://mp.weixin.qq.com/s/seed"]
        );
        assert_eq!(
            store
                .get_account("A==")
                .unwrap()
                .unwrap()
                .seed_url
                .as_deref(),
            Some("https://mp.weixin.qq.com/s/seed")
        );
        // 有文章后优先文章长链，不再回退种子
        store
            .upsert_article(
                "A==",
                "1",
                1,
                &ArticleFields {
                    content_url: Some(
                        "https://mp.weixin.qq.com/s?__biz=A==&mid=1&idx=1&sn=x".into(),
                    ),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            store.latest_article_urls("A==", 5).unwrap(),
            vec!["https://mp.weixin.qq.com/s?__biz=A==&mid=1&idx=1&sn=x"]
        );
    }

    #[test]
    fn test_app_log_append_list_purge() {
        let store = Store::open_in_memory().unwrap();
        let t0 = now();
        // 4 天前的一条应被清；其余保留
        store
            .append_log(t0 - 4.0 * 86_400.0, "info", "list", Some(1), "老日志")
            .unwrap();
        store
            .append_log(t0 - 10.0, "info", "task", None, "收到任务")
            .unwrap();
        store
            .append_log(t0 - 5.0, "warn", "list", Some(2), "凭证过期")
            .unwrap();
        store
            .append_log(t0 - 1.0, "error", "capture", Some(2), "验证页")
            .unwrap();
        assert_eq!(store.count_logs().unwrap(), 4);
        assert_eq!(store.purge_logs().unwrap(), 1);
        assert_eq!(store.count_logs().unwrap(), 3);

        // 全量：时间线升序
        let all = store.list_logs(None, None, None, 100).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].message, "收到任务");
        assert_eq!(all[2].stage, "capture");
        // limit 取最近 N 条仍升序
        let last2 = store.list_logs(None, None, None, 2).unwrap();
        assert_eq!(last2.len(), 2);
        assert_eq!(last2[0].message, "凭证过期");
        // 按环节 / 级别 / 时间过滤
        assert_eq!(
            store
                .list_logs(None, Some("list"), None, 100)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .list_logs(None, None, Some("warn"), 100)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            store
                .list_logs(None, None, Some("error"), 100)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .list_logs(Some(t0 - 6.0), None, None, 100)
                .unwrap()
                .len(),
            2
        );
        // 行数安全阀：超过 LOG_MAX_ROWS 截掉最老的
        for i in 0..(LOG_MAX_ROWS + 5) {
            store
                .append_log(t0, "info", "app", None, &format!("填充 {i}"))
                .unwrap();
        }
        store.purge_logs().unwrap();
        assert_eq!(store.count_logs().unwrap(), LOG_MAX_ROWS);
        assert_eq!(store.clear_logs().unwrap() as i64, LOG_MAX_ROWS);
        assert_eq!(store.count_logs().unwrap(), 0);
    }

    #[test]
    fn test_account_label_prefers_nickname() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.account_label("NOPE=="), "NOPE==");
        store.upsert_account("AAA==", None, None).unwrap();
        assert_eq!(store.account_label("AAA=="), "AAA==");
        store.upsert_account("AAA==", Some("测试号"), None).unwrap();
        assert_eq!(store.account_label("AAA=="), "「测试号」");
    }

    #[test]
    fn test_upsert_credential_reports_key_change() {
        let store = Store::open_in_memory().unwrap();
        let mut f = CredentialFields {
            key: Some("K1".into()),
            uin: Some("U".into()),
            ..Default::default()
        };
        assert!(
            store.upsert_credential("AAA==", &f).unwrap(),
            "首次即换 key"
        );
        assert!(
            !store.upsert_credential("AAA==", &f).unwrap(),
            "同 key 不算换"
        );
        f.key = Some("K2".into());
        assert!(store.upsert_credential("AAA==", &f).unwrap());
        f.key = None;
        assert!(
            !store.upsert_credential("AAA==", &f).unwrap(),
            "无 key 不算换"
        );
    }

    #[test]
    fn test_config_roundtrip() {
        let store = Store::open_in_memory().unwrap();
        assert!(store.get_config("relay_dwell_ms").unwrap().is_none());
        store
            .set_config("relay_dwell_ms", &serde_json::json!(2500))
            .unwrap();
        assert_eq!(
            store.get_config("relay_dwell_ms").unwrap(),
            Some(serde_json::json!(2500))
        );
        store
            .set_config("upstream_source", &serde_json::json!("file"))
            .unwrap();
        let all = store.all_config().unwrap();
        assert_eq!(all.get("upstream_source"), Some(&serde_json::json!("file")));
    }
}
