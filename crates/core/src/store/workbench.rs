//! 工作台持久化：所有关联由事务校验，游标原值仅存于受保护的页面证据文件。
use super::Store;
use crate::{
    model::{now, ParsedArticle},
    native::{protocol::NativePage, NativeConfig},
    workbench::*,
};
use anyhow::{ensure, Context, Result};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
mod home_sync;
mod pagination;
mod source_actions;
mod source_delete;

pub(super) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS wb_discovery(id INTEGER PRIMARY KEY,document TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS wb_sources(biz TEXT PRIMARY KEY,document TEXT NOT NULL,pending TEXT NOT NULL DEFAULT '[]');
CREATE TABLE IF NOT EXISTS wb_sessions(id TEXT PRIMARY KEY,biz TEXT NOT NULL,path TEXT NOT NULL,config TEXT NOT NULL,created_at REAL NOT NULL);
CREATE TABLE IF NOT EXISTS wb_cursors(id INTEGER PRIMARY KEY,biz TEXT NOT NULL,session_id TEXT NOT NULL,parent_id INTEGER,hash TEXT NOT NULL,evidence_path TEXT NOT NULL,observed_at REAL NOT NULL,last_success_at REAL,state TEXT NOT NULL DEFAULT 'unverified',UNIQUE(biz,session_id,hash));
CREATE TABLE IF NOT EXISTS wb_coverage(biz TEXT NOT NULL,mid TEXT NOT NULL,idx INTEGER NOT NULL,PRIMARY KEY(biz,mid,idx));
CREATE TABLE IF NOT EXISTS wb_tasks(id INTEGER PRIMARY KEY,document TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS wb_task_state ON wb_tasks(json_extract(document,'$.state'),json_extract(document,'$.kind'));
CREATE TABLE IF NOT EXISTS wb_experiments(id INTEGER PRIMARY KEY,document TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS wb_probes(id INTEGER PRIMARY KEY,experiment_id INTEGER NOT NULL,document TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS wb_requests(id TEXT PRIMARY KEY,document TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS wb_request_time ON wb_requests(json_extract(document,'$.started_at'));
CREATE TABLE IF NOT EXISTS wb_policy(id INTEGER PRIMARY KEY CHECK(id=1),document TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS wb_policy_history(version INTEGER PRIMARY KEY,changed_at REAL NOT NULL,document TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS wb_archives(article_id INTEGER PRIMARY KEY,document TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS wb_source_deletions(biz TEXT PRIMARY KEY,deleted_at REAL NOT NULL);
CREATE TABLE IF NOT EXISTS wb_archive_cleanup(article_id INTEGER PRIMARY KEY);
"#;

/// 仅沿有首页证据的真实父子链补页码；没有页边界的旧检查点保持未知。
pub(super) const PAGE_MIGRATION: &str = r#"
WITH RECURSIVE pages(id,biz,n) AS (
 SELECT id,biz,1 FROM wb_cursors
 WHERE parent_id IS NULL AND state!='legacy_unverified'
 AND (evidence_path LIKE '%-home.dat' OR id IN (SELECT json_extract(document,'$.home_cursor_id') FROM wb_sources))
 UNION ALL
 SELECT c.id,c.biz,p.n+1 FROM wb_cursors c JOIN pages p ON c.parent_id=p.id
 WHERE c.biz=p.biz AND c.id>p.id AND p.n<100000
)
UPDATE wb_cursors SET source_page=(SELECT n FROM pages WHERE pages.id=wb_cursors.id)
WHERE source_page IS NULL AND id IN (SELECT id FROM pages);
"#;

/// 旧库按真实观测时间选择当前游标；导入旧历史游标的较大ID不代表较新。
pub(super) const CURRENT_MIGRATION: &str = r#"
UPDATE wb_sources SET document=json_set(document,
 '$.current_cursor_id',CASE WHEN json_extract(document,'$.ended')=1 AND json_extract(document,'$.history_cursor_id') IS NULL THEN NULL ELSE
 (SELECT id FROM wb_cursors c WHERE c.biz=wb_sources.biz AND c.id IN (json_extract(document,'$.home_cursor_id'),json_extract(document,'$.history_cursor_id')) ORDER BY observed_at DESC,id DESC LIMIT 1) END,
 '$.current_cursor_at',COALESCE(CASE WHEN json_extract(document,'$.ended')=1 AND json_extract(document,'$.history_cursor_id') IS NULL THEN json_extract(document,'$.updated_at') ELSE
 (SELECT observed_at FROM wb_cursors c WHERE c.biz=wb_sources.biz AND c.id IN (json_extract(document,'$.home_cursor_id'),json_extract(document,'$.history_cursor_id')) ORDER BY observed_at DESC,id DESC LIMIT 1) END,0))
WHERE json_type(document,'$.current_cursor_id') IS NULL;
"#;
fn promote_current(
    tx: &Transaction<'_>,
    source: &mut Source,
    cursor: Option<i64>,
    ts: f64,
) -> Result<()> {
    let observed = match cursor {
        Some(id) => tx.query_row(
            "SELECT observed_at FROM wb_cursors WHERE id=? AND biz=?",
            params![id, source.biz],
            |r| r.get(0),
        )?,
        None => ts,
    };
    if observed >= source.current_cursor_at {
        source.current_cursor_id = cursor;
        source.current_cursor_at = observed;
    }
    Ok(())
}

fn decode<T: DeserializeOwned>(r: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<T> {
    let s: String = r.get(index)?;
    serde_json::from_str(&s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(e))
    })
}
fn cursor_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<CursorRecord> {
    Ok(CursorRecord {
        id: r.get(0)?,
        biz: r.get(1)?,
        session_id: r.get(2)?,
        parent_id: r.get(3)?,
        source_page: r.get(9)?,
        hash: r.get(4)?,
        evidence_path: r.get(5)?,
        observed_at: r.get(6)?,
        last_success_at: r.get(7)?,
        state: r.get(8)?,
    })
}
fn put<T: Serialize>(tx: &Transaction<'_>, table: &str, id: i64, value: &T) -> Result<()> {
    let document = serde_json::to_value(value)?;
    let previous: Value = tx.query_row(
        &format!("SELECT document FROM {table} WHERE id=?"),
        [id],
        |r| decode(r, 0),
    )?;
    let module = match table {
        "wb_tasks" => {
            if document["kind"] == "archive" {
                "archive"
            } else {
                "history"
            }
        }
        "wb_discovery" => "discovery",
        "wb_experiments" => "experiment",
        _ => "task",
    };
    if previous["state"] != document["state"]
        || previous["progress"] != document["progress"]
        || previous["reason"] != document["reason"]
        || previous["phase"] != document["phase"]
    {
        let mut context = document
            .get("progress")
            .cloned()
            .unwrap_or_else(|| json!({}));
        context["phase"] = document["phase"].clone();
        context["next_action_at"] = document["next_action_at"].clone();
        context["verdict"] = document["verdict"].clone();
        context["reason"] = document["reason"].clone();
        context["state"] = document["state"].clone();
        context["previous_state"] = previous["state"].clone();
        context["biz"] = document["biz"].clone();
        context[match table {
            "wb_tasks" => "task_id",
            "wb_discovery" => "discovery_id",
            _ => "experiment_id",
        }] = json!(id);
        context["cursor_id"] = document
            .pointer("/target/cursor_id")
            .cloned()
            .unwrap_or_else(|| document["cursor_id"].clone());
        super::system_logs::write(
            tx,
            module,
            if document["state"] == "failed" {
                "error"
            } else if document["state"] == "needs_action" {
                "warn"
            } else {
                "info"
            },
            "业务任务状态或进度已保存",
            context,
        )?;
    }
    // table 仅来自模块内固定常量，不接收 IPC 输入。
    tx.execute(
        &format!("UPDATE {table} SET document=? WHERE id=?"),
        params![serde_json::to_string(value)?, id],
    )?;
    Ok(())
}
fn save_source(tx: &Transaction<'_>, source: &Source, pending: &[ParsedArticle]) -> Result<()> {
    let previous: Option<Source> = tx
        .query_row(
            "SELECT document FROM wb_sources WHERE biz=?",
            [&source.biz],
            |r| decode(r, 0),
        )
        .optional()?;
    if previous
        .as_ref()
        .map(|s| (s.current_cursor_id, s.current_cursor_at))
        != Some((source.current_cursor_id, source.current_cursor_at))
    {
        super::system_logs::write(
            tx,
            "history",
            "info",
            "当前游标已更新；年龄按当前游标首次获取时间计算",
            json!({"biz":source.biz,"nickname":source.nickname,"cursor_id":previous.as_ref().and_then(|s|s.current_cursor_id),"next_cursor_id":source.current_cursor_id,"age_reset":source.current_cursor_id.is_some(),"outcome":if source.current_cursor_id.is_some(){"cursor_changed"}else{"end"}}),
        )?;
    }
    tx.execute("INSERT INTO wb_sources(biz,document,pending) VALUES(?,?,?) ON CONFLICT(biz) DO UPDATE SET document=excluded.document,pending=excluded.pending",
        params![source.biz,serde_json::to_string(source)?,serde_json::to_string(pending)?])?;
    Ok(())
}
#[allow(clippy::too_many_arguments)] // 页码、父游标与证据在同一事务中保存。
fn save_cursor(
    tx: &Transaction<'_>,
    biz: &str,
    session: &str,
    parent: Option<i64>,
    source_page: Option<u32>,
    page: &NativePage,
    evidence: &str,
    ts: f64,
) -> Result<Option<i64>> {
    if page.is_end || page.cursor.is_empty() {
        return Ok(None);
    }
    ensure!(!evidence.is_empty(), "缺少受保护的游标证据文件");
    let hash = format!("{:x}", Sha256::digest(page.cursor.as_bytes()));
    tx.execute("INSERT INTO wb_cursors(biz,session_id,parent_id,hash,evidence_path,observed_at,source_page) VALUES(?,?,?,?,?,?,?) ON CONFLICT(biz,session_id,hash) DO NOTHING",params![biz,session,parent,hash,evidence,ts,source_page])?;
    Ok(Some(tx.query_row(
        "SELECT id FROM wb_cursors WHERE biz=? AND session_id=? AND hash=?",
        params![biz, session, hash],
        |r| r.get(0),
    )?))
}
fn upsert_page(tx: &Transaction<'_>, page: &NativePage) -> Result<u32> {
    let mut inserted = 0;
    for a in &page.articles {
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM articles WHERE biz=? AND mid=? AND idx=?)",
            params![page.biz, a.mid, a.idx],
            |r| r.get(0),
        )?;
        inserted += u32::from(!exists);
        tx.execute("INSERT INTO articles(biz,mid,idx,sn,title,digest,content_url,author,cover,published_at,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(biz,mid,idx) DO UPDATE SET title=COALESCE(excluded.title,articles.title),content_url=COALESCE(excluded.content_url,articles.content_url),cover=COALESCE(excluded.cover,articles.cover),updated_at=excluded.updated_at",params![page.biz,a.mid,a.idx,a.sn,a.title,a.digest,a.content_url,a.author,a.cover,a.published_at,now(),now()])?;
    }
    Ok(inserted)
}
fn key(a: &ParsedArticle) -> String {
    format!("{}:{}", a.mid, a.idx)
}
fn covered(tx: &Transaction<'_>, biz: &str, a: &ParsedArticle) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM wb_coverage WHERE biz=? AND mid=? AND idx=?)",
        params![biz, a.mid, a.idx],
        |r| r.get(0),
    )?)
}
impl Store {
    /// 只迁移实际已提交的旧游标，不通过页码或文章数推算 Offset。
    pub fn wb_legacy_checkpoint(
        &self,
        biz: &str,
        user: &str,
        before_id: i64,
    ) -> Result<Option<(i64, String, f64)>> {
        let conn = self.conn();
        let old:Option<(i64,String,f64)>=conn.query_row("SELECT id,cursor,updated_at FROM native_jobs WHERE biz=? AND user_name=? AND id<? AND pages>0 AND cursor IS NOT NULL AND cursor!='' ORDER BY id DESC LIMIT 1",params![biz,user,before_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if let Some((id, offset, fallback)) = old {
            let observed:Option<f64>=conn.query_row("SELECT MIN(ts+COALESCE(latency_ms,0)/1000.0) FROM native_call_log WHERE job_id=? AND outcome='ok' AND response_cursor_hash=?",params![id,crate::model::short_hash(&offset)],|r|r.get(0))?;
            Ok(Some((id, offset, observed.unwrap_or(fallback))))
        } else {
            Ok(None)
        }
    }
    pub fn wb_import_legacy_checkpoint(
        &self,
        biz: &str,
        offset: &str,
        evidence: &str,
        observed: f64,
    ) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if !source_delete::accept_observation(&tx, biz, observed)? {
            return Ok(());
        }
        let mut source: Source =
            tx.query_row("SELECT document FROM wb_sources WHERE biz=?", [biz], |r| {
                decode(r, 0)
            })?;
        ensure!(
            source.history_cursor_id == source.home_cursor_id && source.pending_count == 0,
            "已有新历史进度，不覆盖为旧检查点"
        );
        let hash = format!("{:x}", Sha256::digest(offset.as_bytes()));
        tx.execute("INSERT INTO wb_cursors(biz,session_id,hash,evidence_path,observed_at,state) VALUES(?,?,?,?,?,'legacy_unverified') ON CONFLICT(biz,session_id,hash) DO UPDATE SET observed_at=MIN(observed_at,excluded.observed_at)",params![biz,source.session_id,hash,evidence,observed])?;
        let id = tx.query_row(
            "SELECT id FROM wb_cursors WHERE biz=? AND session_id=? AND hash=?",
            params![biz, source.session_id, hash],
            |r| r.get(0),
        )?;
        source.history_cursor_id = Some(id);
        source.ended = false;
        source.boundary_keys.clear();
        // 旧页面文章关联证据缺失时，不编造稳定重建锚点；已有文章仍作为已覆盖集合去重。
        tx.execute("INSERT OR IGNORE INTO wb_coverage(biz,mid,idx) SELECT biz,mid,idx FROM articles WHERE biz=? AND mid IS NOT NULL AND idx IS NOT NULL",[biz])?;
        save_source(&tx, &source, &[])?;
        tx.commit()?;
        Ok(())
    }
    pub fn wb_policy(&self) -> Result<Policy> {
        Ok(self
            .conn()
            .query_row("SELECT document FROM wb_policy WHERE id=1", [], |r| {
                decode(r, 0)
            })
            .optional()?
            .unwrap_or_default())
    }
    pub fn wb_set_policy(&self, mut value: Policy) -> Result<Policy> {
        value.validate()?;
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let previous: Option<Policy> = tx
            .query_row("SELECT document FROM wb_policy WHERE id=1", [], |r| {
                decode(r, 0)
            })
            .optional()?;
        value.version = previous.map_or(2, |p| p.version + 1);
        // 冷却只能由实际响应设置，普通调频不能顺带清掉冷却。
        let old: Option<f64> = tx
            .query_row(
                "SELECT json_extract(document,'$.cooldown_until') FROM wb_policy WHERE id=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        value.cooldown_until = old.unwrap_or(0.);
        let text = serde_json::to_string(&value)?;
        tx.execute("INSERT INTO wb_policy VALUES(1,?) ON CONFLICT(id) DO UPDATE SET document=excluded.document",[&text])?;
        tx.execute(
            "INSERT INTO wb_policy_history VALUES(?,?,?)",
            params![value.version, now(), text],
        )?;
        super::system_logs::write(&tx, "policy", "info", "请求策略已更新", {
            let mut context = serde_json::to_value(&value)?;
            context["policy_version"] = json!(value.version);
            context
        })?;
        tx.commit()?;
        Ok(value)
    }
    pub fn wb_cooldown(&self, until: f64) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut p: Policy = tx
            .query_row("SELECT document FROM wb_policy WHERE id=1", [], |r| {
                decode(r, 0)
            })
            .optional()?
            .unwrap_or_default();
        if p.cooldown_until >= until {
            return Ok(());
        }
        p.cooldown_until = until;
        tx.execute("INSERT INTO wb_policy VALUES(1,?) ON CONFLICT(id) DO UPDATE SET document=excluded.document",[serde_json::to_string(&p)?])?;
        super::system_logs::write(
            &tx,
            "policy",
            "warn",
            "请求触发全局冷却",
            json!({"cooldown_until":p.cooldown_until,"policy_version":p.version}),
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn wb_sources(&self) -> Result<Vec<Source>> {
        let conn = self.conn();
        let mut q=conn.prepare("SELECT s.document FROM wb_sources s JOIN accounts a ON a.biz=s.biz WHERE a.is_deleted=0 ORDER BY json_extract(s.document,'$.updated_at') DESC")?;
        let rows = q
            .query_map([], |r| decode(r, 0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }
    pub fn wb_source(&self, biz: &str) -> Result<Option<Source>> {
        Ok(self
            .conn()
            .query_row("SELECT document FROM wb_sources WHERE biz=?", [biz], |r| {
                decode(r, 0)
            })
            .optional()?)
    }
    pub fn wb_session(&self, id: &str) -> Result<Session> {
        Ok(self.conn().query_row(
            "SELECT id,biz,path,config,created_at FROM wb_sessions WHERE id=?",
            [id],
            |r| {
                Ok(Session {
                    id: r.get(0)?,
                    biz: r.get(1)?,
                    path: r.get(2)?,
                    config: decode(r, 3)?,
                    created_at: r.get(4)?,
                })
            },
        )?)
    }
    pub fn wb_cursors(&self) -> Result<Vec<CursorRecord>> {
        let conn = self.conn();
        let mut q = conn.prepare("SELECT * FROM wb_cursors WHERE id IN (SELECT id FROM wb_cursors ORDER BY id DESC LIMIT 2000) OR id IN (SELECT json_extract(document,'$.history_cursor_id') FROM wb_sources) OR id IN (SELECT json_extract(document,'$.current_cursor_id') FROM wb_sources) ORDER BY id DESC")?;
        let rows = q
            .query_map([], cursor_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }
    pub fn wb_cursor(&self, id: i64) -> Result<CursorRecord> {
        Ok(self
            .conn()
            .query_row("SELECT * FROM wb_cursors WHERE id=?", [id], cursor_row)?)
    }
    pub fn wb_cursor_result(&self, id: i64, outcome: &str, ts: f64) -> Result<()> {
        self.conn().execute("UPDATE wb_cursors SET state=?,last_success_at=CASE WHEN ?='valid' THEN ? ELSE last_success_at END WHERE id=?",params![outcome,outcome,ts,id])?;
        Ok(())
    }
    /// 首次登记只初始化一次历史检查点，后续首页只更新会话及首页快照。
    pub fn wb_capture(
        &self,
        page: &NativePage,
        config: &NativeConfig,
        session_path: &str,
        evidence: &str,
        ts: f64,
    ) -> Result<()> {
        self.wb_capture_inner(page, config, session_path, evidence, ts, None)
            .map(|_| ())
    }

    /// 同步运行验证、会话保存及原任务恢复共用同一事务。
    #[allow(clippy::too_many_arguments)]
    pub fn wb_discovery_capture(
        &self,
        run_id: i64,
        request_ts: f64,
        page: &NativePage,
        config: &NativeConfig,
        session_path: &str,
        evidence: &str,
        ts: f64,
    ) -> Result<bool> {
        self.wb_capture_inner(
            page,
            config,
            session_path,
            evidence,
            ts,
            Some((run_id, request_ts)),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn wb_capture_inner(
        &self,
        page: &NativePage,
        config: &NativeConfig,
        session_path: &str,
        evidence: &str,
        ts: f64,
        observation: Option<(i64, f64)>,
    ) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if let Some((id, _)) = observation {
            home_sync::ensure_running(&tx, id)?;
        }
        if !source_delete::accept_observation(
            &tx,
            &page.biz,
            observation.map_or(ts, |(_, request_ts)| request_ts),
        )? {
            return Ok(false);
        }
        tx.execute("INSERT INTO accounts(biz,nickname,first_seen,last_seen) VALUES(?,?,?,?) ON CONFLICT(biz) DO UPDATE SET nickname=excluded.nickname,is_deleted=0,last_seen=excluded.last_seen",params![page.biz,page.nickname,ts,ts])?;
        if observation.is_none() {
            upsert_page(&tx, page)?;
        }
        let session_id = format!("{:x}", Sha256::digest(session_path.as_bytes()));
        tx.execute(
            "INSERT OR IGNORE INTO wb_sessions VALUES(?,?,?,?,?)",
            params![
                session_id,
                page.biz,
                session_path,
                serde_json::to_string(config)?,
                ts
            ],
        )?;
        let cursor = save_cursor(
            &tx,
            &page.biz,
            &session_id,
            None,
            Some(1),
            page,
            evidence,
            ts,
        )?;
        let existing: Option<Source> = tx
            .query_row(
                "SELECT document FROM wb_sources WHERE biz=?",
                [&page.biz],
                |r| decode(r, 0),
            )
            .optional()?;
        let first = existing.as_ref().is_none_or(|s| s.session_id.is_empty());
        let mut s = existing.unwrap_or(Source {
            biz: page.biz.clone(),
            nickname: page.nickname.clone().unwrap_or_default(),
            user_name: page.user_name.clone(),
            session_id: session_id.clone(),
            current_cursor_id: None,
            current_cursor_at: 0.,
            home_cursor_id: cursor,
            history_cursor_id: cursor,
            pending_count: 0,
            ended: page.is_end,
            article_count: 0,
            updated_at: ts,
            boundary_keys: page.articles.iter().rev().take(5).map(key).collect(),
        });
        let pending: Vec<ParsedArticle> = tx
            .query_row(
                "SELECT pending FROM wb_sources WHERE biz=?",
                [&page.biz],
                |r| decode(r, 0),
            )
            .optional()?
            .unwrap_or_default();
        if first {
            s.history_cursor_id = cursor;
            s.ended = page.is_end;
            s.boundary_keys = page.articles.iter().rev().take(5).map(key).collect();
        }
        s.nickname = page.nickname.clone().unwrap_or(s.nickname);
        s.user_name = page.user_name.clone();
        let current_created: Option<f64> = tx
            .query_row(
                "SELECT created_at FROM wb_sessions WHERE id=?",
                [&s.session_id],
                |r| r.get(0),
            )
            .optional()?;
        if current_created.is_none_or(|created| ts >= created) {
            s.session_id = session_id;
            s.home_cursor_id = cursor;
            promote_current(&tx, &mut s, cursor, ts)?;
        }
        s.updated_at = s.updated_at.max(ts);
        s.article_count =
            tx.query_row("SELECT COUNT(*) FROM articles WHERE biz=?", [&s.biz], |r| {
                r.get(0)
            })?;
        if first && observation.is_none() {
            for a in &page.articles {
                tx.execute(
                    "INSERT OR IGNORE INTO wb_coverage VALUES(?,?,?)",
                    params![s.biz, a.mid, a.idx],
                )?;
            }
        }
        save_source(&tx, &s, &pending)?;
        if let Some((run_id, request_ts)) = observation {
            home_sync::resume_for_capture(&tx, run_id, &s, request_ts, ts)?;
        }
        super::system_logs::write(
            &tx,
            "discovery",
            "info",
            "公众号首页游标与独立请求会话已登记",
            json!({"biz":s.biz,"nickname":s.nickname,"received_articles":page.articles.len(),"cursor_id":cursor,"is_end":page.is_end}),
        )?;
        tx.commit()?;
        Ok(true)
    }
    /// 先登记监测到的公众号，会话材料未到齐时也显示在列表。
    pub fn wb_observe_home(&self, page: &NativePage, ts: f64) -> Result<()> {
        self.wb_observe_home_inner(page, ts, None).map(|_| ())
    }
    /// 监测必须比较请求发出时间，删除前发出的旧响应不能重新登记公众号。
    pub fn wb_discovery_observe_home(
        &self,
        run_id: i64,
        page: &NativePage,
        request_ts: f64,
        ts: f64,
    ) -> Result<bool> {
        self.wb_observe_home_inner(page, ts, Some((run_id, request_ts)))
    }
    fn wb_observe_home_inner(
        &self,
        page: &NativePage,
        ts: f64,
        observation: Option<(i64, f64)>,
    ) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if let Some((id, _)) = observation {
            home_sync::ensure_running(&tx, id)?;
        }
        if !source_delete::accept_observation(
            &tx,
            &page.biz,
            observation.map_or(ts, |(_, request_ts)| request_ts),
        )? {
            return Ok(false);
        }
        tx.execute("INSERT INTO accounts(biz,nickname,first_seen,last_seen) VALUES(?,?,?,?) ON CONFLICT(biz) DO UPDATE SET nickname=excluded.nickname,is_deleted=0,last_seen=excluded.last_seen",params![page.biz,page.nickname,ts,ts])?;
        let existing: Option<Source> = tx
            .query_row(
                "SELECT document FROM wb_sources WHERE biz=?",
                [&page.biz],
                |r| decode(r, 0),
            )
            .optional()?;
        let mut source = existing.unwrap_or(Source {
            biz: page.biz.clone(),
            nickname: String::new(),
            user_name: page.user_name.clone(),
            session_id: String::new(),
            current_cursor_id: None,
            current_cursor_at: 0.,
            home_cursor_id: None,
            history_cursor_id: None,
            pending_count: 0,
            ended: page.is_end,
            article_count: 0,
            updated_at: ts,
            boundary_keys: Vec::new(),
        });
        source.nickname = page.nickname.clone().unwrap_or(source.nickname);
        source.updated_at = source.updated_at.max(ts);
        source.article_count = tx.query_row(
            "SELECT COUNT(*) FROM articles WHERE biz=?",
            [&page.biz],
            |r| r.get(0),
        )?;
        let pending: Vec<ParsedArticle> = tx
            .query_row(
                "SELECT pending FROM wb_sources WHERE biz=?",
                [&page.biz],
                |r| decode(r, 0),
            )
            .optional()?
            .unwrap_or_default();
        save_source(&tx, &source, &pending)?;
        tx.commit()?;
        Ok(true)
    }
    pub fn wb_discovery(&self) -> Result<Option<Discovery>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT document FROM wb_discovery ORDER BY id DESC LIMIT 1",
                [],
                |r| decode(r, 0),
            )
            .optional()?)
    }
    pub fn wb_discovery_start(&self, target: Option<HistoryTarget>) -> Result<i64> {
        ensure!(
            target.is_none(),
            "监测不设置采集范围，请在公众号操作列创建任务"
        );
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let active: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM wb_discovery WHERE json_extract(document,'$.state') IN ('starting','running','stopping'))",[],|r|r.get(0))?;
        ensure!(!active, "识别已经启动，请先停止当前识别");
        let native: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM native_jobs WHERE status IN ('running','paused'))",
            [],
            |r| r.get(0),
        )?;
        ensure!(!native, "请先结束原有首页识别任务");
        tx.execute("INSERT INTO wb_discovery(document) VALUES('{}')", [])?;
        let id = tx.last_insert_rowid();
        let run = Discovery {
            id,
            state: "starting".into(),
            target,
            scope_started_at: 0.,
            started_at: now(),
            updated_at: now(),
            reason: None,
            observed_requests: 0,
            accounts: Vec::new(),
            phases: Vec::new(),
        };
        put(&tx, "wb_discovery", id, &run)?;
        tx.commit()?;
        Ok(id)
    }
    /// 仅保存固定阶段名，不记录密钥、消息正文或完整异常字符串。
    pub fn wb_discovery_phase(&self, id: i64, phase: &str) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut run: Discovery =
            tx.query_row("SELECT document FROM wb_discovery WHERE id=?", [id], |r| {
                decode(r, 0)
            })?;
        run.phases.push(DiscoveryPhase {
            phase: phase.into(),
            ts: now(),
        });
        if run.phases.len() > 100 {
            run.phases.drain(..run.phases.len() - 100);
        }
        put(&tx, "wb_discovery", id, &run)?;
        super::system_logs::write(
            &tx,
            "discovery",
            "info",
            "首页观察器生命周期阶段已记录",
            json!({"discovery_id":id,"reason_code":phase}),
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn wb_discovery_state(&self, id: i64, state: &str, reason: Option<&str>) -> Result<()> {
        ensure!(
            ["starting", "running", "stopping", "stopped", "failed"].contains(&state),
            "未知同步状态"
        );
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut run: Discovery =
            tx.query_row("SELECT document FROM wb_discovery WHERE id=?", [id], |r| {
                decode(r, 0)
            })?;
        if matches!(state, "starting" | "running")
            && matches!(run.state.as_str(), "stopping" | "stopped" | "failed")
        {
            return Ok(());
        }
        run.state = state.into();
        run.reason = reason.map(str::to_string);
        run.updated_at = now();
        put(&tx, "wb_discovery", id, &run)?;
        home_sync::sync_state_changed(&tx, id, state)?;
        tx.commit()?;
        Ok(())
    }
    /// 用户在微信发出的请求无法被许可闸门拦截，但必须计入软件的共享预算。
    pub fn wb_discovery_request(&self, run_id: i64, request_id: &str, ts: f64) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let event = RequestEvent {
            id: request_id.into(),
            category: "list".into(),
            purpose: "observed_home".into(),
            owner_id: run_id,
            owner_kind: "discovery".into(),
            biz: String::new(),
            started_at: ts,
            finished_at: Some(ts),
            outcome: "observed".into(),
            policy_version: 0,
            gap_ms: None,
            detail: json!({"user_initiated":true}),
        };
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO wb_requests VALUES(?,?)",
            params![request_id, serde_json::to_string(&event)?],
        )?;
        if inserted > 0 {
            tx.execute("UPDATE wb_discovery SET document=json_set(document,'$.observed_requests',json_extract(document,'$.observed_requests')+1,'$.updated_at',?) WHERE id=?",params![ts,run_id])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn wb_discovery_response(&self, id: &str, biz: &str, outcome: &str, ts: f64) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let changed=tx.execute("UPDATE wb_requests SET document=json_set(document,'$.biz',?,'$.outcome',?,'$.finished_at',?) WHERE id=? AND json_extract(document,'$.owner_kind')='discovery'",params![biz,outcome,ts,id])?;
        if changed > 0 {
            super::system_logs::write(
                &tx,
                "discovery",
                if outcome == "ok" { "info" } else { "warn" },
                "首页识别请求返回结果",
                json!({"request_id":id,"biz":biz,"outcome":outcome}),
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    /// 登记本轮监测到的公众号与会话状态，不自动创建采集任务。
    pub fn wb_discovery_admit(&self, run_id: i64, biz: &str, ready: bool, ts: f64) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut run: Discovery = tx.query_row(
            "SELECT document FROM wb_discovery WHERE id=?",
            [run_id],
            |r| decode(r, 0),
        )?;
        ensure!(
            matches!(run.state.as_str(), "starting" | "running"),
            "识别已停止，忽略晚到响应"
        );
        if !source_delete::accept_observation(&tx, biz, ts)? {
            return Ok(());
        }
        let source: Source =
            tx.query_row("SELECT document FROM wb_sources WHERE biz=?", [biz], |r| {
                decode(r, 0)
            })?;
        if ready {
            ensure!(!source.session_id.is_empty(), "会话尚未准备好");
        }
        let index = match run.accounts.iter().position(|a| a.biz == biz) {
            Some(i) => i,
            None => {
                run.accounts.push(DiscoveryAccount {
                    biz: biz.into(),
                    observed_at: ts,
                    session_ready: false,
                    task_id: None,
                });
                run.accounts.len() - 1
            }
        };
        let account = &mut run.accounts[index];
        account.observed_at = ts;
        account.session_ready |= ready;
        run.updated_at = ts;
        put(&tx, "wb_discovery", run_id, &run)?;
        tx.commit()?;
        Ok(())
    }
    pub fn wb_tasks(&self) -> Result<Vec<Task>> {
        let conn = self.conn();
        let mut q = conn.prepare("SELECT document FROM wb_tasks ORDER BY id DESC LIMIT 1000")?;
        let rows = q
            .query_map([], |r| decode(r, 0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }
    pub fn wb_task(&self, id: i64) -> Result<Task> {
        Ok(self
            .conn()
            .query_row("SELECT document FROM wb_tasks WHERE id=?", [id], |r| {
                decode(r, 0)
            })?)
    }
    pub fn wb_history_create(
        &self,
        bizzes: Vec<String>,
        target: HistoryTarget,
    ) -> Result<Vec<i64>> {
        target.validate()?;
        ensure!(!bizzes.is_empty(), "请选择公众号");
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut ids = Vec::new();
        let mut unique = HashSet::new();
        for biz in bizzes {
            if !unique.insert(biz.clone()) {
                continue;
            }
            let s = source_actions::ensure_source(&tx, &biz)?;
            let active:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM wb_tasks WHERE json_extract(document,'$.kind')='history' AND json_extract(document,'$.biz')=? AND json_extract(document,'$.state') IN ('queued','running','paused','needs_action'))",[&biz],|r|r.get(0))?;
            ensure!(!active, "公众号已有未结束的历史任务：{}", s.nickname);
            let mut bound_target = target.clone();
            if bound_target.pages.is_some() {
                let current = s
                    .current_cursor_id
                    .context("当前页没有下一页游标，请先识别公众号主页")?;
                ensure!(
                    bound_target.cursor_id.is_none_or(|id| id == current),
                    "当前游标已更新，请重试"
                );
                bound_target.cursor_id = Some(current);
            }
            let id = create_task(
                &tx,
                "history",
                Some(biz),
                serde_json::to_value(&bound_target)?,
            )?;
            ids.push(id);
        }
        tx.commit()?;
        Ok(ids)
    }
    pub fn wb_archive_create(&self, _ids: Vec<i64>) -> Result<i64> {
        anyhow::bail!("ZIP 归档已移除，请使用正文下载或 Markdown 导出")
    }
    /// 全库计数，不依赖首页快照缓存，也不受前端分页限制。
    pub fn wb_source_counts(&self) -> Result<std::collections::HashMap<String, i64>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT biz,COUNT(*) FROM articles WHERE is_deleted=0 GROUP BY biz")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    /// 所有下载入口共用同一事务：固定集合、正文去重、队列去重同时完成。
    pub fn wb_download_enqueue(&self, ids: Vec<i64>) -> Result<DownloadReceipt> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let receipt = enqueue_download(&tx, ids, None)?;
        tx.commit()?;
        Ok(receipt)
    }
    pub fn wb_download_source(&self, biz: &str) -> Result<DownloadReceipt> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let ids = {
            let mut stmt =
                tx.prepare("SELECT id FROM articles WHERE biz=? AND is_deleted=0 ORDER BY id")?;
            let rows = stmt.query_map([biz], |r| r.get::<_, i64>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let receipt = enqueue_download(&tx, ids, Some(biz.to_string()))?;
        tx.commit()?;
        Ok(receipt)
    }
    pub fn wb_body_available(&self, id: i64) -> Result<bool> {
        Ok(self.conn().query_row("SELECT EXISTS(SELECT 1 FROM articles WHERE id=? AND is_deleted=0 AND length(trim(COALESCE(content_md,''),char(9)||char(10)||char(13)||char(32)))>0)",[id],|r|r.get(0))?)
    }
    /// 下载任务未结束时整体拒绝删除，防止后台写回使被删条目重新出现。
    pub fn wb_delete_articles(&self, mut ids: Vec<i64>) -> Result<usize> {
        ids.sort_unstable();
        ids.dedup();
        ensure!(
            !ids.is_empty() && ids.len() <= 100000,
            "请选择一至十万篇文章"
        );
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let queued = unfinished_download_ids(&tx)?;
        ensure!(
            !ids.iter().any(|id| queued.contains(id)),
            "所选文章仍在未结束的正文任务中，请先到任务页取消相关任务再删除"
        );
        for id in &ids {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM articles WHERE id=? AND is_deleted=0)",
                [id],
                |r| r.get(0),
            )?;
            ensure!(exists, "文章已删除或不存在，请刷新后重试");
        }
        for id in &ids {
            tx.execute("UPDATE articles SET is_deleted=1 WHERE id=?", [id])?;
        }
        tx.execute("UPDATE wb_sources SET document=json_set(document,'$.article_count',(SELECT COUNT(*) FROM articles WHERE articles.biz=wb_sources.biz AND is_deleted=0))",[])?;
        super::system_logs::write(
            &tx,
            "storage",
            "info",
            "用户所选文章已标记删除",
            json!({"count":ids.len()}),
        )?;
        tx.commit()?;
        Ok(ids.len())
    }
    pub fn wb_task_state(&self, id: i64, state: &str, reason: Option<&str>) -> Result<()> {
        ensure!(
            [
                "queued",
                "running",
                "paused",
                "needs_action",
                "done",
                "cancelled",
                "failed"
            ]
            .contains(&state),
            "未知任务状态"
        );
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut task: Task =
            tx.query_row("SELECT document FROM wb_tasks WHERE id=?", [id], |r| {
                decode(r, 0)
            })?;
        ensure!(
            !["done", "cancelled"].contains(&task.state.as_str()),
            "任务已经结束"
        );
        ensure!(
            state != "queued" || task.kind != "archive" || task.target["download_only"] == true,
            "ZIP 归档已移除，不能恢复旧归档任务"
        );
        if state == "queued" && ["history", "home"].contains(&task.kind.as_str()) {
            if let Some(blocked) = &task.progress.blocked_session_id {
                let source: Source = tx.query_row(
                    "SELECT document FROM wb_sources WHERE biz=?",
                    [&task.biz],
                    |r| decode(r, 0),
                )?;
                ensure!(
                    source.session_id != *blocked,
                    "当前仍是已失效的会话。{}",
                    SESSION_RECOVERY_HINT
                );
            }
        }
        task.state = state.into();
        if let Some(recovery) = &mut task.progress.session_recovery {
            recovery.phase = if state == "queued" {
                "resumed"
            } else {
                "interrupted"
            }
            .into();
        }
        task.reason = reason.map(str::to_string);
        task.updated_at = now();
        task.next_action_at = now();
        put(&tx, "wb_tasks", id, &task)?;
        tx.commit()?;
        Ok(())
    }
    /// 会话失败与任务暂停在同一事务内保存，不能覆盖用户的暂停或取消。
    pub fn wb_task_block_session(&self, id: i64, session_id: &str, outcome: &str) -> Result<()> {
        ensure!(
            ["session_timeout", "authentication_failed"].contains(&outcome),
            "不是可确认的会话故障"
        );
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut task: Task =
            tx.query_row("SELECT document FROM wb_tasks WHERE id=?", [id], |r| {
                decode(r, 0)
            })?;
        if !["queued", "running"].contains(&task.state.as_str()) {
            return Ok(());
        }
        let biz = task.biz.as_deref().context("任务没有公众号")?;
        let session_biz: String = tx.query_row(
            "SELECT biz FROM wb_sessions WHERE id=?",
            [session_id],
            |r| r.get(0),
        )?;
        ensure!(
            ["history", "home"].contains(&task.kind.as_str()) && session_biz == biz,
            "会话与采集任务不匹配"
        );
        task.progress.blocked_session_id = Some(session_id.into());
        task.state = "needs_action".into();
        let reason = if outcome == "session_timeout" {
            "会话已过期（服务端返回 H5 Session Timeout）"
        } else {
            "会话认证失败"
        };
        if let Some(recovery) = &mut task.progress.session_recovery {
            recovery.phase = "interrupted".into();
        }
        task.reason = Some(format!("请求未完成：{}。{}", reason, SESSION_RECOVERY_HINT));
        task.updated_at = now();
        put(&tx, "wb_tasks", id, &task)?;
        tx.commit()?;
        Ok(())
    }
    pub fn wb_save_task(&self, task: &Task) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let previous: Task =
            tx.query_row("SELECT document FROM wb_tasks WHERE id=?", [task.id], |r| {
                decode(r, 0)
            })?;
        if !["cancelled", "paused"].contains(&previous.state.as_str()) {
            // 删除期间改变了混合下载的目标集合时，旧快照的身份、原因、统计一并丢弃。
            // worker 随后重读持久化目标，尚未计入的成功正文会走本地跳过，不会重复联网。
            let next = if task.kind == "archive" && task.target != previous.target {
                previous.clone()
            } else {
                task.clone()
            };
            put(&tx, "wb_tasks", task.id, &next)?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn wb_consume(&self, id: i64) -> Result<Task> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut task: Task =
            tx.query_row("SELECT document FROM wb_tasks WHERE id=?", [id], |r| {
                decode(r, 0)
            })?;
        if !["queued", "running"].contains(&task.state.as_str()) {
            return Ok(task);
        }
        let biz = task.biz.as_deref().context("历史任务没有公众号")?;
        let (mut s, pending): (Source, Vec<ParsedArticle>) = tx.query_row(
            "SELECT document,pending FROM wb_sources WHERE biz=?",
            [biz],
            |r| Ok((decode(r, 0)?, decode(r, 1)?)),
        )?;
        let target: HistoryTarget = serde_json::from_value(task.target.clone())?;
        if let Some(pages) = target.pages {
            // 手动下一页不消费已有缓冲，不改变按篇数任务的覆盖边界。
            task.state = if task.progress.pages >= pages {
                "done"
            } else {
                "running"
            }
            .into();
            task.updated_at = now();
            task.reason = None;
            put(&tx, "wb_tasks", id, &task)?;
            tx.commit()?;
            return Ok(task);
        }
        let mut remaining = Vec::new();
        let mut boundary = Vec::new();
        for a in pending {
            if covered(&tx, biz, &a)? {
                continue;
            }
            let fits = target.count.map_or_else(
                || {
                    a.published_at
                        .is_some_and(|p| p >= target.cutoff_at.unwrap_or(0))
                },
                |n| task.progress.completed < n,
            );
            if fits {
                tx.execute(
                    "INSERT OR IGNORE INTO wb_coverage VALUES(?,?,?)",
                    params![biz, a.mid, a.idx],
                )?;
                task.progress.completed += 1;
                boundary.push(key(&a));
            } else {
                remaining.push(a)
            }
        }
        if !boundary.is_empty() {
            s.boundary_keys = boundary.into_iter().rev().take(5).collect();
        }
        s.pending_count = remaining.len() as i64;
        s.updated_at = now();
        let satisfied = target.count.is_some_and(|n| task.progress.completed >= n)
            || target.cutoff_at.is_some() && task.progress.date_boundary_pages >= 2;
        task.state = if satisfied || s.ended {
            "done"
        } else {
            "running"
        }
        .into();
        task.reason = if s.ended && !satisfied {
            Some("已到历史末尾".into())
        } else {
            None
        };
        task.updated_at = now();
        save_source(&tx, &s, &remaining)?;
        put(&tx, "wb_tasks", id, &task)?;
        tx.commit()?;
        Ok(task)
    }
    pub fn wb_history_commit(
        &self,
        id: i64,
        page: &NativePage,
        session_id: &str,
        evidence: &str,
        ts: f64,
    ) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut task: Task =
            tx.query_row("SELECT document FROM wb_tasks WHERE id=?", [id], |r| {
                decode(r, 0)
            })?;
        if task.state == "cancelled" {
            return Ok(());
        }
        ensure!(
            task.kind == "history"
                && ["running", "paused", "queued"].contains(&task.state.as_str()),
            "历史任务已结束"
        );
        ensure!(
            task.biz.as_deref() == Some(page.biz.as_str()),
            "历史响应公众号不一致"
        );
        let (mut s, mut pending): (Source, Vec<ParsedArticle>) = tx.query_row(
            "SELECT document,pending FROM wb_sources WHERE biz=?",
            [&page.biz],
            |r| Ok((decode(r, 0)?, decode(r, 1)?)),
        )?;
        let target: HistoryTarget = serde_json::from_value(task.target.clone())?;
        let parent = if target.pages.is_some() {
            target.cursor_id
        } else if task.progress.rebuilding {
            task.progress.rebuild_cursor
        } else {
            s.history_cursor_id
        };
        let source_page = if let Some(parent) = parent {
            tx.query_row(
                "SELECT source_page FROM wb_cursors WHERE id=? AND biz=?",
                params![parent, page.biz],
                |r| r.get::<_, Option<u32>>(0),
            )?
            .and_then(|n| n.checked_add(1))
        } else if task.progress.rebuilding {
            Some(1)
        } else {
            None
        };
        let cursor = save_cursor(
            &tx,
            &page.biz,
            session_id,
            parent,
            source_page,
            page,
            evidence,
            ts,
        )?;
        // 旧任务缺少此前响应篇数，续采后仍不能把局部计数当成完整总数。
        task.progress.received_articles = task
            .progress
            .received_articles
            .or((task.progress.pages == 0).then_some(0))
            .map(|count| count + page.articles.len() as u32);
        task.progress.pages += 1;
        let added = upsert_page(&tx, page)?;
        task.progress.new_articles += added;
        super::system_logs::write(
            &tx,
            "history",
            "info",
            "列表页面已保存；新入库为零表示文章已存在",
            json!({"task_id":id,"biz":page.biz,"cursor_id":parent,"next_cursor_id":cursor,"received_articles":page.articles.len(),"new_articles":added,"source_page":source_page,"is_end":page.is_end,"outcome":"page_saved"}),
        )?;
        promote_current(&tx, &mut s, cursor, ts)?;
        // 手动翻当前页的分支不能覆盖历史任务的检查点或已保存缓冲。
        task.progress.session_recovery = None;
        task.progress.blocked_session_id = None;
        if target.pages.is_some() && parent != s.history_cursor_id {
            s.updated_at = s.updated_at.max(ts);
            s.article_count =
                tx.query_row("SELECT COUNT(*) FROM articles WHERE biz=?", [&s.biz], |r| {
                    r.get(0)
                })?;
            task.updated_at = ts;
            save_source(&tx, &s, &pending)?;
            put(&tx, "wb_tasks", id, &task)?;
            tx.commit()?;
            return Ok(());
        }
        task.progress.session_recovery = None;
        task.progress.blocked_session_id = None;
        let mut articles = page.articles.clone();
        if task.progress.rebuilding {
            let matched: Vec<String> = articles
                .iter()
                .map(key)
                .filter(|k| task.progress.rebuild_anchor.contains(k))
                .collect();
            task.progress.rebuilt_seen.extend(matched);
            let all_found = task
                .progress
                .rebuild_anchor
                .iter()
                .all(|k| task.progress.rebuilt_seen.contains(k));
            if all_found {
                let last = articles
                    .iter()
                    .rposition(|a| task.progress.rebuild_anchor.contains(&key(a)))
                    .context("历史定位边界不明确")?;
                articles = articles.into_iter().skip(last + 1).collect();
                task.progress.rebuilding = false;
            } else {
                task.progress.rebuild_cursor = cursor;
                if page.is_end {
                    task.state = "needs_action".into();
                    task.reason = Some("重建到达末尾仍未找到完整历史边界，请人工核对".into());
                }
                task.updated_at = ts;
                s.updated_at = s.updated_at.max(ts);
                s.article_count =
                    tx.query_row("SELECT COUNT(*) FROM articles WHERE biz=?", [&s.biz], |r| {
                        r.get(0)
                    })?;
                save_source(&tx, &s, &pending)?;
                put(&tx, "wb_tasks", id, &task)?;
                tx.commit()?;
                return Ok(());
            }
        }
        let mut known: HashSet<String> = pending.iter().map(key).collect();
        for a in articles {
            if !covered(&tx, &page.biz, &a)? && known.insert(key(&a)) {
                pending.push(a)
            }
        }
        let target: HistoryTarget = serde_json::from_value(task.target.clone())?;
        if let Some(cutoff) = target.cutoff_at {
            let fully_old = !page.articles.is_empty()
                && page
                    .articles
                    .iter()
                    .all(|a| a.published_at.is_some_and(|p| p < cutoff));
            task.progress.date_boundary_pages = if fully_old {
                task.progress.date_boundary_pages + 1
            } else {
                0
            };
        }
        s.history_cursor_id = cursor;
        s.ended = page.is_end;
        s.pending_count = pending.len() as i64;
        s.updated_at = ts;
        s.article_count =
            tx.query_row("SELECT COUNT(*) FROM articles WHERE biz=?", [&s.biz], |r| {
                r.get(0)
            })?;
        task.updated_at = ts;
        save_source(&tx, &s, &pending)?;
        put(&tx, "wb_tasks", id, &task)?;
        tx.commit()?;
        Ok(())
    }
    pub fn wb_begin_rebuild(&self, id: i64) -> Result<()> {
        let limit = self.wb_policy()?.rebuild_limit;
        let mut task = self.wb_task(id)?;
        ensure!(task.progress.rebuilds < limit, "已达到本任务自动重建上限");
        let source = self
            .wb_source(task.biz.as_deref().context("公众号缺失")?)?
            .context("公众号不存在")?;
        ensure!(
            !source.boundary_keys.is_empty(),
            "缺少稳定历史边界，不能自动重建"
        );
        task.progress.rebuilds += 1;
        task.progress.rebuilding = true;
        task.progress.rebuild_cursor = None;
        task.progress.rebuild_anchor = source.boundary_keys;
        task.progress.rebuilt_seen.clear();
        task.reason = Some("原游标已确认不可复用，正在从首页重建定位；额外请求计入预算".into());
        self.wb_save_task(&task)
    }
    pub fn wb_experiment_create(&self, cursor_id: i64, hours: Vec<f64>, ts: f64) -> Result<i64> {
        self.wb_experiment_create_with_session(cursor_id, hours, None, ts)
    }
    /// 新实验默认固定当前公众号最新会话，也允许显式选择原会话用于对照。
    pub fn wb_experiment_create_with_session(
        &self,
        cursor_id: i64,
        mut hours: Vec<f64>,
        session_id: Option<&str>,
        ts: f64,
    ) -> Result<i64> {
        ensure!(
            hours.len() <= 24,
            "最多设置 24 个后续验证时点；空列表表示仅验证当前可用性"
        );
        ensure!(
            hours.iter().all(|v| v.is_finite() && *v > 0. && *v <= 24.),
            "验证时点须在未来 24 小时内"
        );
        hours.sort_by(f64::total_cmp);
        hours.dedup();
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let cursor = tx.query_row(
            "SELECT * FROM wb_cursors WHERE id=?",
            [cursor_id],
            cursor_row,
        )?;
        let source: Source = tx.query_row("SELECT s.document FROM wb_sources s JOIN accounts a ON a.biz=s.biz WHERE s.biz=? AND a.is_deleted=0", [&cursor.biz], |r| decode(r,0)).context("实验公众号不存在，请刷新后重试")?;
        ensure!(
            source_delete::accept_observation(&tx, &cursor.biz, cursor.observed_at)?,
            "游标属于删除前的公众号，请重新打开主页获取新游标"
        );
        let session_id = session_id.map(str::to_string).unwrap_or(source.session_id);
        let session_biz: String = tx.query_row(
            "SELECT biz FROM wb_sessions WHERE id=?",
            [&session_id],
            |r| r.get(0),
        )?;
        ensure!(session_biz == cursor.biz, "验证会话与游标不属于同一公众号");
        tx.execute("INSERT INTO wb_experiments(document) VALUES('{}')", [])?;
        let id = tx.last_insert_rowid();
        let mut schedule = vec![0.];
        schedule.extend(hours);
        let exp = Experiment {
            phase: "baseline_pending".into(),
            next_action_at: Some(ts),
            verdict: None,
            id,
            cursor_id,
            session_id,
            schedule_hours: schedule,
            next_index: 0,
            state: "running".into(),
            started_at: ts,
            deadline_at: ts + 86400.,
            conclusion: None,
            report_path: None,
        };
        put(&tx, "wb_experiments", id, &exp)?;
        tx.commit()?;
        Ok(id)
    }
    pub fn wb_experiments(&self) -> Result<Vec<Experiment>> {
        let conn = self.conn();
        let mut q =
            conn.prepare("SELECT document FROM wb_experiments ORDER BY id DESC LIMIT 1000")?;
        let rows = q
            .query_map([], |r| decode(r, 0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }
    pub fn wb_experiment(&self, id: i64) -> Result<Experiment> {
        Ok(self.conn().query_row(
            "SELECT document FROM wb_experiments WHERE id=?",
            [id],
            |r| decode(r, 0),
        )?)
    }
    pub fn wb_save_experiment(&self, e: &Experiment) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let previous: Experiment = tx.query_row(
            "SELECT document FROM wb_experiments WHERE id=?",
            [e.id],
            |r| decode(r, 0),
        )?;
        let mut next = e.clone();
        // 阶段由独立原子入口更新，不能被等待请求前的旧副本回写。
        next.phase = previous.phase.clone();
        next.next_action_at = previous.next_action_at;
        next.next_index = next.next_index.max(previous.next_index);
        if ["cancelled", "done", "failed"].contains(&previous.state.as_str()) {
            next.state = previous.state;
            // 已定稿的结论不被旧副本回写；但旧值为空时允许补上推导结果，
            // 否则按截止时间结束的实验永远存不进 write_report 推导的 verdict。
            if previous.conclusion.is_some() {
                next.conclusion = previous.conclusion;
            }
            if previous.verdict.is_some() {
                next.verdict = previous.verdict;
            }
        }
        if next.state != "running" {
            next.phase = if next.state == "cancelled" {
                "cancelled"
            } else {
                match next.verdict.as_deref() {
                    Some("confirmed_unusable") => "confirmed_unusable",
                    Some("session_unavailable") => "session_blocked",
                    _ => "completed",
                }
            }
            .into();
            next.next_action_at = None;
        }
        put(&tx, "wb_experiments", e.id, &next)?;
        tx.commit()?;
        Ok(())
    }
    /// 阶段和等待原因独立提交，长时间对照请求期间界面也能看到真实过程。
    pub fn wb_experiment_phase(&self, id: i64, phase: &str, next_at: Option<f64>) -> Result<()> {
        ensure!(
            [
                "baseline_pending",
                "scheduled_wait",
                "requesting",
                "gate_wait",
                "control_home",
                "control_page",
                "recheck"
            ]
            .contains(&phase),
            "未知实验阶段"
        );
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut e: Experiment = tx.query_row(
            "SELECT document FROM wb_experiments WHERE id=?",
            [id],
            |r| decode(r, 0),
        )?;
        if e.state != "running" {
            return Ok(());
        }
        e.phase = phase.into();
        e.next_action_at = next_at;
        put(&tx, "wb_experiments", id, &e)?;
        tx.commit()?;
        Ok(())
    }
    pub fn wb_probe_add(&self, mut p: Probe) -> Result<i64> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM wb_experiments WHERE id=?)",
            [p.experiment_id],
            |r| r.get(0),
        )?;
        ensure!(exists, "实验不存在");
        if p.role == "scheduled" {
            let existing:Option<i64>=tx.query_row("SELECT id FROM wb_probes WHERE experiment_id=? AND json_extract(document,'$.role')='scheduled' AND json_extract(document,'$.scheduled_at')=?",params![p.experiment_id,p.scheduled_at],|r|r.get(0)).optional()?;
            if let Some(id) = existing {
                return Ok(id);
            }
        }
        tx.execute(
            "INSERT INTO wb_probes(experiment_id,document) VALUES(?,'{}')",
            [p.experiment_id],
        )?;
        p.id = tx.last_insert_rowid();
        tx.execute(
            "UPDATE wb_probes SET document=? WHERE id=?",
            params![serde_json::to_string(&p)?, p.id],
        )?;
        if p.role == "scheduled" {
            let mut e: Experiment = tx.query_row(
                "SELECT document FROM wb_experiments WHERE id=?",
                [p.experiment_id],
                |r| decode(r, 0),
            )?;
            let index = e
                .schedule_hours
                .iter()
                .position(|h| (e.started_at + h * 3600. - p.scheduled_at).abs() < 0.01)
                .context("验证记录不属于实验时点")?;
            e.next_index = e.next_index.max(index + 1);
            put(&tx, "wb_experiments", e.id, &e)?;
        }
        super::system_logs::write(
            &tx,
            "experiment",
            if p.outcome == "valid" { "info" } else { "warn" },
            "固定游标验证结果已保存",
            json!({"experiment_id":p.experiment_id,"request_id":p.request_id,"cursor_age_seconds":p.cursor_age_seconds,"outcome":p.outcome,"role":p.role,"scheduled_at":p.scheduled_at}),
        )?;
        tx.commit()?;
        Ok(p.id)
    }
    pub fn wb_probes(&self, id: i64) -> Result<Vec<Probe>> {
        let conn = self.conn();
        let mut q =
            conn.prepare("SELECT document FROM wb_probes WHERE experiment_id=? ORDER BY id")?;
        let rows = q
            .query_map([id], |r| decode(r, 0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }
    pub fn wb_request_permit(
        &self,
        category: &str,
        purpose: &str,
        owner: i64,
        biz: &str,
        ts: f64,
    ) -> Result<Permit> {
        ensure!(
            ["list", "body", "image"].contains(&category),
            "未知请求类别"
        );
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let p: Policy = tx
            .query_row("SELECT document FROM wb_policy WHERE id=1", [], |r| {
                decode(r, 0)
            })
            .optional()?
            .unwrap_or_default();
        let previous:Option<f64>=tx.query_row("SELECT MAX(json_extract(document,'$.started_at')) FROM wb_requests WHERE json_extract(document,'$.category')=?",[category],|r|r.get(0))?;
        let native_previous: Option<f64> = if category == "list" {
            tx.query_row("SELECT MAX(ts) FROM native_call_log", [], |r| r.get(0))?
        } else {
            None
        };
        let last = previous
            .into_iter()
            .chain(native_previous)
            .max_by(f64::total_cmp);
        let gap = match category {
            "list" => crate::rng::between(p.list_min_ms, p.list_max_ms),
            "body" => p.body_gap_ms,
            _ => p.image_gap_ms,
        };
        let mut next = last
            .map_or(ts, |v| v + gap as f64 / 1000.)
            .max(p.cooldown_until);
        let mut reason = (next > ts).then(|| "请求间隔或全局冷却".to_string());
        if category == "list" {
            let (count,oldest):(i64,Option<f64>)=tx.query_row("SELECT COUNT(*),MIN(t) FROM (SELECT json_extract(document,'$.started_at') t FROM wb_requests WHERE json_extract(document,'$.category')='list' UNION ALL SELECT ts FROM native_call_log) WHERE t>?",[ts-86400.],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if count >= p.list_daily_budget {
                next = next.max(oldest.unwrap_or(ts) + 86400. + 0.01);
                reason = Some("近 24 小时列表预算用完".into());
            }
        }
        if next > ts {
            return Ok(Permit {
                request_id: None,
                next_at: next,
                reason,
            });
        }
        let id = format!(
            "{}-{}-{}",
            category,
            (ts * 1000000.) as u64,
            tx.query_row("SELECT COUNT(*) FROM wb_requests", [], |r| r
                .get::<_, i64>(0))?
        );
        let event = RequestEvent {
            id: id.clone(),
            category: category.into(),
            purpose: purpose.into(),
            owner_id: owner,
            owner_kind: if purpose.starts_with("cursor_") {
                "experiment"
            } else {
                "task"
            }
            .into(),
            biz: biz.into(),
            started_at: ts,
            finished_at: None,
            outcome: "pending".into(),
            policy_version: p.version,
            gap_ms: last.map(|v| ((ts - v) * 1000.) as i64),
            detail: json!({"min_ms":if category=="list"{p.list_min_ms}else{gap},"max_ms":gap}),
        };
        tx.execute(
            "INSERT INTO wb_requests VALUES(?,?)",
            params![id, serde_json::to_string(&event)?],
        )?;
        super::system_logs::write(
            &tx,
            request_module(&event),
            "info",
            "请求已获得许可并开始发送",
            request_context(&event),
        )?;
        tx.commit()?;
        Ok(Permit {
            request_id: Some(id),
            next_at: ts,
            reason: None,
        })
    }
    pub fn wb_request_finish(&self, id: &str, outcome: &str, detail: Value, ts: f64) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let event: RequestEvent =
            tx.query_row("SELECT document FROM wb_requests WHERE id=?", [id], |r| {
                decode(r, 0)
            })?;
        let changed=tx.execute("UPDATE wb_requests SET document=json_set(document,'$.outcome',?,'$.finished_at',?,'$.detail',json_patch(json_extract(document,'$.detail'),json(?))) WHERE id=? AND json_extract(document,'$.outcome')='pending'",params![outcome,ts,serde_json::to_string(&detail)?,id])?;
        if changed > 0 {
            let mut context = request_context(&event);
            context["duration_ms"] = json!(((ts - event.started_at) * 1000.).max(0.));
            context["outcome"] = json!(outcome);
            for name in ["http_status", "business_ret", "outer_ret"] {
                context[name] = detail[name].clone();
            }
            super::system_logs::write(
                &tx,
                request_module(&event),
                if outcome == "ok" { "info" } else { "warn" },
                "请求已完成，业务页面是否入库以页面保存记录为准",
                context,
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    /// HTTP 成功后再保存内容判定；保留原始状态码、发送时间和策略证据。
    pub fn wb_request_content(&self, id: &str, outcome: &str) -> Result<()> {
        ensure!(
            [
                "ok",
                "verification_required",
                "unavailable",
                "empty_body",
                "invalid_encoding",
                "invalid_image"
            ]
            .contains(&outcome),
            "内容判定无效"
        );
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let changed=tx.execute("UPDATE wb_requests SET document=json_set(document,'$.outcome',?,'$.detail.content_outcome',?) WHERE id=? AND json_extract(document,'$.outcome')='ok'",params![outcome,outcome,id])?;
        if changed > 0 && outcome != "ok" {
            let event: RequestEvent =
                tx.query_row("SELECT document FROM wb_requests WHERE id=?", [id], |r| {
                    decode(r, 0)
                })?;
            let mut context = request_context(&event);
            context["outcome"] = json!(outcome);
            super::system_logs::write(
                &tx,
                request_module(&event),
                "warn",
                "HTTP返回后内容校验未通过，未计为下载成功",
                context,
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn wb_requests(&self, since: f64) -> Result<Vec<RequestEvent>> {
        let conn = self.conn();
        let mut q=conn.prepare("SELECT document FROM wb_requests WHERE json_extract(document,'$.started_at')>=? ORDER BY json_extract(document,'$.started_at') DESC")?;
        let mut events = q
            .query_map([since], |r| decode(r, 0))?
            .collect::<rusqlite::Result<Vec<RequestEvent>>>()?;
        drop(q);
        drop(conn);
        // 原生采集只读 native_call_log，不再拼接其镜像 list_call_log。
        for c in self.native_calls_since(since, 200000)? {
            events.push(RequestEvent{id:c.request_id,category:"list".into(),purpose:if c.action==Some(0){"home"}else{"legacy_page"}.into(),owner_id:c.job_id,owner_kind:"native".into(),biz:String::new(),started_at:c.ts,finished_at:c.latency_ms.map(|ms|c.ts+ms as f64/1000.),outcome:c.outcome,policy_version:c.policy_version,gap_ms:c.gap_ms,detail:json!({"transport":c.transport,"outer_ret":c.outer_ret,"jsapi_ret":c.jsapi_ret,"inner_ret":c.inner_ret,"reason":c.reason})});
        }
        events.sort_by(|a, b| b.started_at.total_cmp(&a.started_at));
        Ok(events)
    }
    pub fn wb_snapshot(&self) -> Result<Snapshot> {
        let since = now() - 86400.;
        let mut requests = self.wb_requests(since)?;
        requests.truncate(2000);
        let conn = self.conn();
        let count:i64=conn.query_row("SELECT (SELECT COUNT(*) FROM wb_requests WHERE json_extract(document,'$.category')='list' AND json_extract(document,'$.started_at')>?)+(SELECT COUNT(*) FROM native_call_log WHERE ts>?)",params![since,since],|r|r.get(0))?;
        drop(conn);
        let tasks = self.wb_tasks()?;
        let download_queue = self.wb_download_queue()?;
        Ok(Snapshot {
            active_queues: self.wb_active_queues()?,
            download_queue,
            discovery: self.wb_discovery()?,
            sources: self.wb_sources()?,
            cursors: self.wb_cursors()?,
            tasks,
            experiments: self.wb_experiments()?,
            requests,
            policy: self.wb_policy()?,
            list_calls_24h: count,
            archives: self.wb_archives()?,
        })
    }
    pub fn wb_list_window(&self, since: f64) -> Result<(i64, Option<f64>)> {
        Ok(self.conn().query_row("SELECT COUNT(*),MIN(t) FROM (SELECT json_extract(document,'$.started_at') t FROM wb_requests WHERE json_extract(document,'$.category')='list' UNION ALL SELECT ts FROM native_call_log) WHERE t>?",[since],|r|Ok((r.get(0)?,r.get(1)?)))?)
    }
    pub fn wb_last_list_request(&self) -> Result<Option<f64>> {
        Ok(self.conn().query_row("SELECT MAX(t) FROM (SELECT json_extract(document,'$.started_at') t FROM wb_requests WHERE json_extract(document,'$.category')='list' UNION ALL SELECT ts FROM native_call_log)",[],|r|r.get(0))?)
    }
    pub fn wb_restart(&self, ts: f64) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let legacy = {
            let mut query = tx.prepare("SELECT document FROM wb_tasks WHERE json_extract(document,'$.kind')='archive' AND COALESCE(json_extract(document,'$.target.download_only'),0)<>1 AND json_extract(document,'$.state') NOT IN ('done','cancelled')")?;
            let values = query
                .query_map([], |r| decode::<Task>(r, 0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            values
        };
        for mut task in legacy {
            task.state = "cancelled".into();
            task.reason = Some("ZIP 归档功能已移除，已有文件与数据保留".into());
            if let Some(recovery) = &mut task.progress.session_recovery {
                recovery.phase = "interrupted".into();
            }
            task.updated_at = ts;
            put(&tx, "wb_tasks", task.id, &task)?;
        }
        let discoveries = {
            let mut q=tx.prepare("SELECT document FROM wb_discovery WHERE json_extract(document,'$.state') IN ('starting','running','stopping')")?;
            let values = q
                .query_map([], |r| decode::<Discovery>(r, 0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            values
        };
        for mut run in discoveries {
            run.state = "stopped".into();
            run.reason = Some("应用已重启，请手动开始识别".into());
            run.updated_at = ts;
            put(&tx, "wb_discovery", run.id, &run)?;
        }
        let tasks = {
            let mut q = tx.prepare("SELECT document FROM wb_tasks WHERE json_extract(document,'$.state')='running' OR json_extract(document,'$.progress.session_recovery.phase') IN ('starting','waiting_home','preparing_session')")?;
            let values = q
                .query_map([], |r| decode::<Task>(r, 0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            values
        };
        for mut task in tasks {
            if let Some(recovery) = &mut task.progress.session_recovery {
                recovery.phase = "interrupted".into();
            }
            task.state = "paused".into();
            task.reason = Some("应用重启，点击继续从检查点恢复".into());
            put(&tx, "wb_tasks", task.id, &task)?;
        }
        let pending = {
            let mut q=tx.prepare("SELECT document FROM wb_requests WHERE json_extract(document,'$.outcome')='pending'")?;
            let values = q
                .query_map([], |r| decode::<RequestEvent>(r, 0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            values
        };
        for event in pending {
            let mut context = request_context(&event);
            context["outcome"] = json!("interrupted");
            super::system_logs::write(
                &tx,
                request_module(&event),
                "warn",
                "应用重启时发现未完成请求，已保留预算消耗和业务检查点",
                context,
            )?;
        }
        tx.execute("UPDATE wb_requests SET document=json_set(document,'$.outcome','interrupted','$.finished_at',?) WHERE json_extract(document,'$.outcome')='pending'",[ts])?;
        tx.commit()?;
        Ok(())
    }
    pub fn wb_archive(&self, id: i64) -> Result<Option<ArchiveRecord>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT document FROM wb_archives WHERE article_id=?",
                [id],
                |r| decode(r, 0),
            )
            .optional()?)
    }
    pub fn wb_archives(&self) -> Result<Vec<ArchiveRecord>> {
        let conn = self.conn();
        let mut q = conn.prepare("SELECT document FROM wb_archives")?;
        let rows = q
            .query_map([], |r| decode(r, 0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }
    pub fn wb_save_archive(&self, a: &ArchiveRecord) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        source_delete::ensure_live_article(&tx, a.article_id)?;
        tx.execute("INSERT INTO wb_archives VALUES(?,?) ON CONFLICT(article_id) DO UPDATE SET document=excluded.document",params![a.article_id,serde_json::to_string(a)?])?;
        super::system_logs::write(
            &tx,
            "archive",
            if a.state == "failed" { "error" } else { "info" },
            "文章正文与图片处理结果已保存",
            json!({"article_id":a.article_id,"state":a.state,"partial":a.missing_images.len(),"reason":a.error}),
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn wb_article_cover(&self, id: i64) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT cover FROM articles WHERE id=?", [id], |r| r.get(0))?)
    }
    pub fn wb_article_sn(&self, id: i64) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT sn FROM articles WHERE id=?", [id], |r| r.get(0))?)
    }
    pub fn wb_cached_html(&self, id: i64) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT content_html FROM articles WHERE id=?", [id], |r| {
                r.get(0)
            })?)
    }
    pub fn wb_control_url(&self, except: i64) -> Result<Option<String>> {
        Ok(self.conn().query_row("SELECT content_url FROM articles WHERE id!=? AND detail_done=1 AND is_deleted=0 AND content_url IS NOT NULL ORDER BY updated_at DESC LIMIT 1",[except],|r|r.get(0)).optional()?)
    }
}
fn unfinished_download_ids(tx: &Transaction<'_>) -> Result<HashSet<i64>> {
    let mut result = HashSet::new();
    let mut stmt=tx.prepare("SELECT document FROM wb_tasks WHERE json_extract(document,'$.kind')='archive' AND json_extract(document,'$.state') NOT IN ('done','cancelled','failed')")?;
    let rows = stmt.query_map([], |r| decode::<Task>(r, 0))?;
    for task in rows {
        let task = task?;
        let ids: Vec<i64> = serde_json::from_value(task.target["article_ids"].clone())?;
        result.extend(
            ids.into_iter()
                .filter(|id| !task.progress.finished_ids.contains(id)),
        );
    }
    Ok(result)
}
fn request_module(event: &RequestEvent) -> &str {
    if event.owner_kind == "experiment" {
        "experiment"
    } else if event.category == "list" {
        "history"
    } else {
        "archive"
    }
}
fn request_context(event: &RequestEvent) -> Value {
    let mut value = json!({"request_id":event.id,"biz":event.biz,"category":event.category,"purpose":event.purpose,"policy_version":event.policy_version});
    value[if event.owner_kind == "experiment" {
        "experiment_id"
    } else {
        "task_id"
    }] = json!(event.owner_id);
    value
}

fn enqueue_download(
    tx: &Transaction<'_>,
    mut ids: Vec<i64>,
    biz: Option<String>,
) -> Result<DownloadReceipt> {
    ids.sort_unstable();
    ids.dedup();
    ensure!(!ids.is_empty(), "没有可下载的已入库文章");
    ensure!(ids.len() <= 100000, "一次最多下载十万篇文章，请分批操作");
    let occupied = unfinished_download_ids(tx)?;
    let mut receipt = DownloadReceipt {
        requested: ids.len(),
        ..Default::default()
    };
    let mut pending = Vec::new();
    for id in ids {
        let available:Option<bool>=tx.query_row("SELECT length(trim(COALESCE(content_md,''),char(9)||char(10)||char(13)||char(32)))>0 FROM articles WHERE id=? AND is_deleted=0",[id],|r|r.get(0)).optional()?;
        let available = available.context("文章已删除或不存在，请刷新后重试")?;
        if available {
            receipt.already_downloaded += 1;
        } else if occupied.contains(&id) {
            receipt.already_queued += 1;
        } else {
            pending.push(id);
        }
    }
    receipt.queued = pending.len();
    if !pending.is_empty() {
        receipt.task_id = Some(create_task(
            tx,
            "archive",
            biz,
            json!({"article_ids":pending,"download_only":true}),
        )?);
    }
    super::system_logs::write(
        tx,
        "archive",
        "info",
        "正文下载请求已去重并加入队列",
        json!({"task_id":receipt.task_id,"requested":receipt.requested,"queued":receipt.queued,"already_downloaded":receipt.already_downloaded,"already_queued":receipt.already_queued}),
    )?;
    Ok(receipt)
}

fn create_task(
    tx: &Transaction<'_>,
    kind: &str,
    biz: Option<String>,
    target: Value,
) -> Result<i64> {
    tx.execute("INSERT INTO wb_tasks(document) VALUES('{}')", [])?;
    let id = tx.last_insert_rowid();
    let ts = now();
    put(
        tx,
        "wb_tasks",
        id,
        &Task {
            id,
            kind: kind.into(),
            biz,
            state: "queued".into(),
            target,
            progress: TaskProgress::default(),
            reason: None,
            next_action_at: ts,
            created_at: ts,
            updated_at: ts,
            result_path: None,
        },
    )?;
    Ok(id)
}

#[cfg(test)]
mod current_migration_tests {
    use super::*;
    #[test]
    fn migration_uses_observation_time_and_preserves_explicit_current_end() {
        let store = Store::open_in_memory().unwrap();
        let page = NativePage {
            biz: "FAKE".into(),
            user_name: "fake-user".into(),
            nickname: Some("虚构公众号".into()),
            avatar: None,
            cursor: "synthetic-home".into(),
            is_end: false,
            articles: Vec::new(),
        };
        store
            .wb_capture(
                &page,
                &NativeConfig::default(),
                "fake-session.dat",
                "fake-home.dat",
                1000.,
            )
            .unwrap();
        let home = store.wb_source("FAKE").unwrap().unwrap().home_cursor_id;
        store
            .wb_import_legacy_checkpoint("FAKE", "synthetic-legacy", "fake-legacy.dat", 100.)
            .unwrap();
        let legacy = store.wb_source("FAKE").unwrap().unwrap().history_cursor_id;
        assert!(legacy > home);
        store.conn().execute("UPDATE wb_sources SET document=json_remove(document,'$.current_cursor_id','$.current_cursor_at')",[]).unwrap();
        store.conn().execute_batch(CURRENT_MIGRATION).unwrap();
        let migrated = store.wb_source("FAKE").unwrap().unwrap();
        assert_eq!(migrated.current_cursor_id, home);
        assert_eq!(migrated.current_cursor_at, 1000.);
        assert_eq!(migrated.history_cursor_id, legacy);
        store.conn().execute("UPDATE wb_sources SET document=json_set(document,'$.current_cursor_id',NULL,'$.current_cursor_at',2000)",[]).unwrap();
        store.conn().execute_batch(CURRENT_MIGRATION).unwrap();
        let ended = store.wb_source("FAKE").unwrap().unwrap();
        assert_eq!(ended.current_cursor_id, None);
        assert_eq!(ended.current_cursor_at, 2000.);
    }
}
