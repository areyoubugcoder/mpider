//! 原生任务与请求账本；所有 SQLite 访问仍通过 Store，文章与游标在同一事务提交。

use super::*;
use crate::native::protocol::{NativePage, NativeRequest, ResponseCodes};
use crate::native::{NativeCall, NativeConfig, NativeJob};
use anyhow::{ensure, Context};

pub(super) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS native_jobs (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 status TEXT NOT NULL,
 config_json TEXT NOT NULL,
 biz TEXT, user_name TEXT, nickname TEXT, cursor TEXT,
 pages INTEGER NOT NULL DEFAULT 0, total_pages INTEGER NOT NULL DEFAULT 0,
 new_articles INTEGER NOT NULL DEFAULT 0,
 last_request_at REAL, next_action_at REAL, pause_reason TEXT,
 started_at REAL NOT NULL, updated_at REAL NOT NULL, finished_at REAL
);
CREATE UNIQUE INDEX IF NOT EXISTS native_one_active
 ON native_jobs((1)) WHERE status IN ('running','paused');
CREATE TABLE IF NOT EXISTS native_call_log (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 job_id INTEGER NOT NULL, request_id TEXT NOT NULL UNIQUE, list_call_id INTEGER NOT NULL,
 ts REAL NOT NULL, cgi INTEGER NOT NULL, action INTEGER,
 outcome TEXT NOT NULL DEFAULT 'pending', latency_ms INTEGER, gap_ms INTEGER,
 outer_ret INTEGER, jsapi_ret INTEGER, inner_ret INTEGER,
 request_cursor_hash TEXT, response_cursor_hash TEXT, cursor_advanced INTEGER,
 articles INTEGER NOT NULL DEFAULT 0, new_articles INTEGER NOT NULL DEFAULT 0,
 dll_sha256 TEXT NOT NULL, gate_min_ms INTEGER NOT NULL, gate_max_ms INTEGER NOT NULL,
 reason TEXT, transport TEXT NOT NULL DEFAULT 'native'
);
CREATE INDEX IF NOT EXISTS native_call_ts ON native_call_log(ts);
CREATE INDEX IF NOT EXISTS native_call_job ON native_call_log(job_id);
"#;

fn job_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<NativeJob> {
    let config: String = r.get(2)?;
    let config = serde_json::from_str(&config).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(NativeJob {
        id: r.get(0)?,
        status: r.get(1)?,
        config,
        biz: r.get(3)?,
        user_name: r.get(4)?,
        nickname: r.get(5)?,
        cursor: r.get(6)?,
        pages: r.get(7)?,
        total_pages: r.get(8)?,
        new_articles: r.get(9)?,
        last_request_at: r.get(10)?,
        next_action_at: r.get(11)?,
        pause_reason: r.get(12)?,
        started_at: r.get(13)?,
        updated_at: r.get(14)?,
        finished_at: r.get(15)?,
    })
}

const JOB_COLS: &str = "id,status,config_json,biz,user_name,nickname,cursor,pages,total_pages,new_articles,last_request_at,next_action_at,pause_reason,started_at,updated_at,finished_at";

/// 首次会话响应绑定名称；恢复或后续响应必须保持同一个账号。
fn target_config(job: &NativeJob, page: &NativePage) -> Result<NativeConfig> {
    let name = page
        .nickname
        .as_deref()
        .filter(|v| !v.trim().is_empty())
        .context("响应缺少公众号名称")?;
    ensure!(
        (job.config.transport == "session" && job.config.window_title.is_empty())
            || job.config.window_title == name,
        "响应公众号名称与任务不一致"
    );
    if let Some(biz) = &job.biz {
        ensure!(biz == &page.biz, "任务公众号发生变化");
    }
    if let Some(user) = &job.user_name {
        ensure!(user == &page.user_name, "任务客户端身份发生变化");
    }
    let mut config = job.config.clone();
    config.window_title = name.to_owned();
    Ok(config)
}

impl Store {
    pub fn native_create(&self, cfg: &NativeConfig) -> Result<NativeJob> {
        cfg.validate()?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let observing: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM wb_discovery WHERE json_extract(document,'$.state') IN ('starting','running','stopping'))",[],|r|r.get(0))?;
        ensure!(!observing, "持续识别正在运行，请先停止识别");
        let n: i64 = tx.query_row(
            "SELECT COUNT(*) FROM native_jobs WHERE status IN ('running','paused')",
            [],
            |r| r.get(0),
        )?;
        ensure!(n == 0, "已有原生采集任务，请先完成或取消");
        let old: i64 = tx.query_row(
            "SELECT COUNT(*) FROM history_jobs WHERE status IN ('running','paused')",
            [],
            |r| r.get(0),
        )?;
        ensure!(old == 0, "有旧历史任务，请先完成或取消");
        let t = now();
        tx.execute("INSERT INTO native_jobs(status,config_json,biz,started_at,updated_at) VALUES('running',?,?,?,?)",
            params![serde_json::to_string(cfg)?,cfg.expected_biz,t,t])?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        drop(conn);
        self.native_job(id)?.context("新建任务未找到")
    }

    pub fn native_job(&self, id: i64) -> Result<Option<NativeJob>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {JOB_COLS} FROM native_jobs WHERE id=?"),
                [id],
                job_row,
            )
            .optional()?)
    }
    pub fn native_active(&self) -> Result<Option<NativeJob>> {
        Ok(self.conn().query_row(&format!("SELECT {JOB_COLS} FROM native_jobs WHERE status IN ('running','paused') ORDER BY id DESC LIMIT 1"),[],job_row).optional()?)
    }
    pub fn native_recent(&self, limit: i64) -> Result<Vec<NativeJob>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {JOB_COLS} FROM native_jobs ORDER BY id DESC LIMIT ?"
        ))?;
        let rows = stmt.query_map([limit.clamp(1, 100)], job_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    pub fn native_set_state(&self, id: i64, status: &str, reason: Option<&str>) -> Result<()> {
        ensure!(
            ["running", "paused", "done", "cancelled", "failed"].contains(&status),
            "未知任务状态"
        );
        let conn = self.conn();
        let terminal = matches!(status, "done" | "cancelled" | "failed");
        let n=conn.execute("UPDATE native_jobs SET status=?,pause_reason=?,next_action_at=NULL,updated_at=?,finished_at=? WHERE id=? AND status IN ('running','paused')",
            params![status,reason,now(),terminal.then(now),id])?;
        ensure!(n == 1, "任务已结束或不存在");
        Ok(())
    }
    /// 会话模式重提取后沿用检查点；页面模式从首页重新核对并去重。
    pub fn native_resume(&self, id: i64) -> Result<()> {
        let n=self.conn().execute("UPDATE native_jobs SET status='running',pages=CASE WHEN json_extract(config_json,'$.transport')='session' THEN pages ELSE 0 END,pause_reason=NULL,next_action_at=NULL,updated_at=? WHERE id=? AND status='paused'",params![now(),id])?;
        ensure!(n == 1, "任务未暂停或已结束");
        Ok(())
    }
    pub fn native_pause_on_restart(&self) -> Result<()> {
        self.conn().execute("UPDATE native_jobs SET status='paused',pause_reason='restart',next_action_at=NULL,updated_at=? WHERE status='running'",[now()])?;
        Ok(())
    }
    pub fn native_next_action(&self, id: i64, at: Option<f64>) -> Result<()> {
        self.conn().execute(
            "UPDATE native_jobs SET next_action_at=?,updated_at=? WHERE id=? AND status='running'",
            params![at, now(), id],
        )?;
        Ok(())
    }
    pub fn native_calls_window(&self, since: f64) -> Result<(i64, Option<f64>)> {
        Ok(self.conn().query_row(
            "SELECT COUNT(*),MIN(ts) FROM native_call_log WHERE ts>?",
            [since],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }
    pub fn native_last_request(&self) -> Result<Option<f64>> {
        Ok(self
            .conn()
            .query_row("SELECT MAX(ts) FROM native_call_log", [], |r| r.get(0))?)
    }
    pub fn native_record_request(
        &self,
        job: &NativeJob,
        request: &NativeRequest,
        dll: &str,
    ) -> Result<bool> {
        ensure!([5814, 26365].contains(&request.cgi), "不记录未知 CGI");
        ensure!(request.ts.is_finite() && request.ts > 0., "请求时间无效");
        let policy = self.wb_policy()?;
        let gate_min = if policy.version > 1 {
            policy.list_min_ms
        } else {
            job.config.min_gap_seconds * 1000
        };
        let gate_max = if policy.version > 1 {
            policy.list_max_ms
        } else {
            job.config.max_gap_seconds * 1000
        };
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM native_call_log WHERE request_id=?)",
            [&request.request_id],
            |r| r.get(0),
        )?;
        if exists {
            return Ok(false);
        }
        let previous: Option<f64> =
            tx.query_row("SELECT MAX(ts) FROM native_call_log", [], |r| r.get(0))?;
        let gap = previous.map(|t| ((request.ts - t).max(0.) * 1000.) as i64);
        let source = if request.capture_only {
            "session_refresh"
        } else if request.transport == "mmtls" {
            "session_list"
        } else if request.cgi == 5814 {
            "native_list"
        } else {
            "native_aux"
        };
        tx.execute("INSERT INTO list_call_log(ts,biz,source,page,outcome,latency_ms,articles,new_articles,gap_ms,gate_min_ms,gate_max_ms,transport,cgi_cmdid) VALUES(?,?,?,?, 'pending',0,0,0,?,?,?,?,?)",
            params![request.ts,job.biz.as_deref().unwrap_or(""),source,job.pages+1,gap,gate_min,gate_max,request.transport,request.cgi])?;
        let list_id = tx.last_insert_rowid();
        tx.execute("INSERT INTO native_call_log(job_id,request_id,list_call_id,ts,cgi,action,gap_ms,request_cursor_hash,dll_sha256,gate_min_ms,gate_max_ms) VALUES(?,?,?,?,?,?,?,?,?,?,?)",
            params![job.id,request.request_id,list_id,request.ts,request.cgi,request.action,gap,request.offset.as_deref().and_then(crate::model::short_hash),dll,gate_min,gate_max])?;
        tx.execute(
            "UPDATE native_call_log SET transport=?,policy_version=? WHERE request_id=?",
            params![request.transport, policy.version, request.request_id],
        )?;
        tx.execute(
            "UPDATE native_jobs SET last_request_at=?,updated_at=? WHERE id=?",
            params![request.ts, now(), job.id],
        )?;
        tx.commit()?;
        Ok(true)
    }
    pub fn native_finish_call(
        &self,
        request_id: &str,
        ts: f64,
        codes: &ResponseCodes,
        outcome: &str,
        reason: Option<&str>,
    ) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let Some((list_id,started))=tx.query_row("SELECT list_call_id,ts FROM native_call_log WHERE request_id=? AND outcome='pending'",[request_id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,f64>(1)?))).optional()? else { return Ok(()) };
        let elapsed = ((ts - started).max(0.) * 1000.) as i64;
        tx.execute("UPDATE native_call_log SET outcome=?,latency_ms=?,outer_ret=?,jsapi_ret=?,inner_ret=?,reason=? WHERE request_id=?",
            params![outcome,elapsed,codes.outer,codes.jsapi,codes.inner,reason,request_id])?;
        let ret = codes.inner.or(codes.jsapi).or(codes.outer);
        tx.execute(
            "UPDATE list_call_log SET outcome=?,latency_ms=?,ret=?,note=? WHERE id=?",
            params![outcome, elapsed, ret, reason, list_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// 名称来自已关联的真实列表响应；只登记目标，不消费页面或移动检查点。
    pub fn native_bind_target(
        &self,
        id: i64,
        request: &NativeRequest,
        page: &NativePage,
    ) -> Result<()> {
        ensure!(
            request.cgi == 5814 && request.transport == "native" && request.action == Some(0),
            "目标识别必须来自微信首页请求"
        );
        ensure!(
            request.user_name.as_deref() == Some(page.user_name.as_str()),
            "请求与响应账号不一致"
        );
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let job = tx.query_row(
            &format!("SELECT {JOB_COLS} FROM native_jobs WHERE id=?"),
            [id],
            job_row,
        )?;
        ensure!(
            job.config.transport == "session"
                && matches!(job.status.as_str(), "running" | "paused"),
            "任务不能识别目标"
        );
        let pending: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM native_call_log WHERE job_id=? AND request_id=? AND cgi=5814 AND transport='native' AND outcome='pending')", params![id, request.request_id], |r| r.get(0))?;
        ensure!(pending, "目标识别无法关联在途请求");
        let config = target_config(&job, page)?;
        tx.execute("UPDATE native_jobs SET config_json=?,biz=?,user_name=?,nickname=?,updated_at=? WHERE id=?", params![serde_json::to_string(&config)?,page.biz,page.user_name,page.nickname,now(),id])?;
        tx.commit()?;
        Ok(())
    }

    /// 原子提交文章、游标、任务进度和请求结果；重放响应不重复计数。
    pub fn native_commit_page(
        &self,
        job_id: i64,
        request: &NativeRequest,
        page: &NativePage,
        ts: f64,
        codes: &ResponseCodes,
    ) -> Result<i64> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let job = tx.query_row(
            &format!("SELECT {JOB_COLS} FROM native_jobs WHERE id=?"),
            [job_id],
            job_row,
        )?;
        ensure!(
            matches!(job.status.as_str(), "running" | "paused"),
            "任务已结束"
        );
        let call=tx.query_row("SELECT list_call_id,ts FROM native_call_log WHERE job_id=? AND request_id=? AND cgi=5814 AND outcome='pending'",params![job_id,request.request_id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,f64>(1)?))).optional()?;
        let Some((list_id, started)) = call else {
            return Ok(0);
        };
        crate::native::protocol::validate_progress(
            job.pages,
            job.cursor.as_deref(),
            request,
            page,
        )?;
        let config = target_config(&job, page)?;
        tx.execute("INSERT INTO accounts(biz,nickname,round_head_img,first_seen,last_seen) VALUES(?,?,?,?,?) ON CONFLICT(biz) DO UPDATE SET nickname=COALESCE(excluded.nickname,accounts.nickname),round_head_img=COALESCE(excluded.round_head_img,accounts.round_head_img),last_seen=excluded.last_seen",params![page.biz,page.nickname,page.avatar,now(),now()])?;
        let mut new_articles = 0;
        for article in &page.articles {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM articles WHERE biz=? AND mid=? AND idx=?)",
                params![page.biz, article.mid, article.idx],
                |r| r.get(0),
            )?;
            if !exists {
                new_articles += 1;
            }
            tx.execute("INSERT INTO articles(biz,mid,idx,sn,title,digest,content_url,author,cover,published_at,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(biz,mid,idx) DO UPDATE SET sn=COALESCE(excluded.sn,articles.sn),title=COALESCE(excluded.title,articles.title),digest=COALESCE(excluded.digest,articles.digest),content_url=COALESCE(excluded.content_url,articles.content_url),author=COALESCE(excluded.author,articles.author),cover=COALESCE(excluded.cover,articles.cover),published_at=COALESCE(excluded.published_at,articles.published_at),updated_at=excluded.updated_at",
                params![page.biz,article.mid,article.idx,article.sn,article.title,article.digest,article.content_url,article.author,article.cover,article.published_at,now(),now()])?;
        }
        // 最后一页仍保留运行态，等待本页附带请求核对完成后由运行器收口。
        let status = if job.status == "paused" {
            "paused"
        } else {
            "running"
        };
        tx.execute("UPDATE native_jobs SET config_json=?,biz=?,user_name=?,nickname=?,cursor=?,pages=pages+1,total_pages=total_pages+1,new_articles=new_articles+?,status=?,next_action_at=NULL,updated_at=?,finished_at=? WHERE id=?",
            params![serde_json::to_string(&config)?,page.biz,page.user_name,page.nickname,page.cursor,new_articles,status,now(),(status=="done").then(now),job_id])?;
        let elapsed = ((ts - started).max(0.) * 1000.) as i64;
        let advanced = request.offset.as_deref() != Some(page.cursor.as_str());
        tx.execute("UPDATE native_call_log SET outcome='ok',latency_ms=?,outer_ret=?,jsapi_ret=?,inner_ret=?,response_cursor_hash=?,cursor_advanced=?,articles=?,new_articles=? WHERE request_id=?",
            params![elapsed,codes.outer,codes.jsapi,codes.inner,crate::model::short_hash(&page.cursor),advanced,page.articles.len() as i64,new_articles,request.request_id])?;
        tx.execute("UPDATE list_call_log SET biz=?,outcome='ok',latency_ms=?,ret=?,articles=?,new_articles=? WHERE id=?",
            params![page.biz,elapsed,codes.inner,page.articles.len() as i64,new_articles,list_id])?;
        tx.commit()?;
        Ok(new_articles)
    }
    pub fn native_interrupt_pending(&self, job_id: i64) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("UPDATE list_call_log SET outcome='interrupted',note='采样已结束，未收到可核对响应' WHERE id IN (SELECT list_call_id FROM native_call_log WHERE job_id=? AND outcome='pending')",[job_id])?;
        tx.execute("UPDATE native_call_log SET outcome='interrupted',reason='observer_stopped' WHERE job_id=? AND outcome='pending'",[job_id])?;
        tx.commit()?;
        Ok(())
    }
    pub fn native_calls_since(&self, since: f64, limit: i64) -> Result<Vec<NativeCall>> {
        let conn = self.conn();
        let mut stmt=conn.prepare("SELECT id,job_id,request_id,ts,cgi,action,outcome,latency_ms,gap_ms,outer_ret,jsapi_ret,inner_ret,request_cursor_hash,response_cursor_hash,cursor_advanced,articles,new_articles,dll_sha256,gate_min_ms,gate_max_ms,reason,transport,policy_version FROM native_call_log WHERE ts>=? ORDER BY id DESC LIMIT ?")?;
        let rows = stmt.query_map(
            params![since, limit.clamp(1, RATELIMIT_LOG_MAX_ROWS)],
            |r| {
                Ok(NativeCall {
                    id: r.get(0)?,
                    job_id: r.get(1)?,
                    request_id: r.get(2)?,
                    ts: r.get(3)?,
                    cgi: r.get(4)?,
                    action: r.get(5)?,
                    outcome: r.get(6)?,
                    latency_ms: r.get(7)?,
                    gap_ms: r.get(8)?,
                    outer_ret: r.get(9)?,
                    jsapi_ret: r.get(10)?,
                    inner_ret: r.get(11)?,
                    request_cursor_hash: r.get(12)?,
                    response_cursor_hash: r.get(13)?,
                    cursor_advanced: r.get(14)?,
                    articles: r.get(15)?,
                    new_articles: r.get(16)?,
                    dll_sha256: r.get(17)?,
                    gate_min_ms: r.get(18)?,
                    gate_max_ms: r.get(19)?,
                    reason: r.get(20)?,
                    transport: r.get(21)?,
                    policy_version: r.get(22)?,
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    pub fn native_purge_calls(&self) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM native_call_log WHERE ts<?",
            [now() - RATELIMIT_LOG_RETENTION_SECS as f64],
        )?;
        conn.execute("DELETE FROM native_call_log WHERE id IN (SELECT id FROM native_call_log ORDER BY id DESC LIMIT -1 OFFSET ?)",[RATELIMIT_LOG_MAX_ROWS])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> NativeConfig {
        NativeConfig {
            window_title: "虚构公众号".into(),
            max_pages: 2,
            ..Default::default()
        }
    }
    fn request(id: &str, action: i64, offset: &str) -> NativeRequest {
        NativeRequest {
            capture_only: false,
            transport: "native".into(),
            request_id: id.into(),
            ts: now(),
            cgi: 5814,
            action: Some(action),
            offset: Some(offset.into()),
            user_name: Some("fake-account".into()),
        }
    }
    fn page(cursor: &str) -> NativePage {
        NativePage {
            biz: "FAKE".into(),
            user_name: "fake-account".into(),
            nickname: Some("虚构公众号".into()),
            avatar: None,
            cursor: cursor.into(),
            is_end: false,
            articles: vec![crate::model::ParsedArticle {
                mid: "1".into(),
                idx: 1,
                sn: Some("fake".into()),
                title: Some("测试文章".into()),
                digest: None,
                author: None,
                cover: None,
                published_at: Some(100),
                content_url: Some(
                    "https://mp.weixin.qq.com/s?__biz=FAKE&mid=1&idx=1&sn=fake".into(),
                ),
            }],
        }
    }
    fn codes() -> ResponseCodes {
        ResponseCodes {
            outer: Some(0),
            jsapi: Some(0),
            inner: Some(0),
            rate_signal: false,
            session_timeout: false,
        }
    }
    #[test]
    fn auto_target_is_bound_before_articles_and_cannot_switch_identity() {
        let store = Store::open_in_memory().unwrap();
        let cfg = NativeConfig {
            transport: "session".into(),
            max_pages: 2,
            ..Default::default()
        };
        let job = store.native_create(&cfg).unwrap();
        let first = request("detected", 0, "");
        assert!(store
            .native_bind_target(job.id, &first, &page("next"))
            .is_err());
        store.native_record_request(&job, &first, "fake").unwrap();
        store
            .native_bind_target(job.id, &first, &page("next"))
            .unwrap();
        let bound = store.native_job(job.id).unwrap().unwrap();
        assert_eq!(bound.config.window_title, "虚构公众号");
        assert_eq!(bound.nickname.as_deref(), Some("虚构公众号"));
        assert_eq!(bound.pages, 0);
        assert_eq!(bound.new_articles, 0);
        assert_eq!(bound.cursor, None);
        assert_eq!(store.count_articles(None).unwrap(), 0);
        let mut wrong = page("next");
        wrong.user_name = "other-account".into();
        assert!(store.native_bind_target(job.id, &first, &wrong).is_err());
        wrong = page("next");
        wrong.biz = "OTHER".into();
        assert!(store.native_bind_target(job.id, &first, &wrong).is_err());
        assert_eq!(
            store
                .native_commit_page(job.id, &first, &page("next"), now(), &codes())
                .unwrap(),
            1
        );
        store
            .native_set_state(job.id, "paused", Some("manual"))
            .unwrap();
        store.native_resume(job.id).unwrap();
        let resumed = store.native_job(job.id).unwrap().unwrap();
        assert_eq!(resumed.config.window_title, "虚构公众号");
        assert_eq!(resumed.cursor.as_deref(), Some("next"));
    }
    #[test]
    fn atomic_checkpoint_duplicate_and_wrong_cursor() {
        let store = Store::open_in_memory().unwrap();
        let job = store.native_create(&config()).unwrap();
        let first = request("first", 0, "");
        assert!(store
            .native_record_request(&job, &first, "fake-hash")
            .unwrap());
        assert!(!store
            .native_record_request(&job, &first, "fake-hash")
            .unwrap());
        assert_eq!(
            store
                .native_commit_page(job.id, &first, &page("next"), now(), &codes())
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .native_commit_page(job.id, &first, &page("next"), now(), &codes())
                .unwrap(),
            0
        );
        let before = store.native_job(job.id).unwrap().unwrap();
        assert_eq!(before.pages, 1);
        assert_eq!(before.new_articles, 1);
        let wrong = request("second", 1, "wrong");
        store
            .native_record_request(&before, &wrong, "fake-hash")
            .unwrap();
        assert!(store
            .native_commit_page(job.id, &wrong, &page("further"), now(), &codes())
            .is_err());
        let after = store.native_job(job.id).unwrap().unwrap();
        assert_eq!(after.cursor, before.cursor);
        assert_eq!(after.pages, 1);
        assert_eq!(store.native_calls_window(now() - 86400.).unwrap().0, 2);
    }
    #[test]
    fn session_resume_keeps_cursor_and_existing_session() {
        let store = Store::open_in_memory().unwrap();
        let mut cfg = config();
        cfg.transport = "session".into();
        cfg.session_file = "fake-session.json".into();
        let job = store.native_create(&cfg).unwrap();
        let first = request("fake-first", 0, "");
        store.native_record_request(&job, &first, "fake").unwrap();
        store
            .native_commit_page(job.id, &first, &page("fake-checkpoint"), now(), &codes())
            .unwrap();
        store
            .native_set_state(job.id, "paused", Some("manual"))
            .unwrap();
        store.native_resume(job.id).unwrap();
        let after = store.native_job(job.id).unwrap().unwrap();
        assert_eq!(after.pages, 1);
        assert_eq!(after.cursor.as_deref(), Some("fake-checkpoint"));
        assert_eq!(after.config.session_file, "fake-session.json");
    }
    #[test]
    fn pause_race_restart_and_resume_keep_budget() {
        let store = Store::open_in_memory().unwrap();
        let job = store.native_create(&config()).unwrap();
        assert!(store.native_create(&config()).is_err());
        let req = request("pause", 0, "");
        store
            .native_record_request(&job, &req, "fake-hash")
            .unwrap();
        store
            .native_set_state(job.id, "paused", Some("manual"))
            .unwrap();
        store
            .native_commit_page(job.id, &req, &page("next"), now(), &codes())
            .unwrap();
        let paused = store.native_job(job.id).unwrap().unwrap();
        assert_eq!(paused.status, "paused");
        assert_eq!(paused.new_articles, 1);
        store.native_resume(job.id).unwrap();
        let resumed = store.native_job(job.id).unwrap().unwrap();
        assert_eq!(resumed.pages, 0);
        assert_eq!(resumed.total_pages, 1);
        let req = request("resume", 0, "");
        store
            .native_record_request(&resumed, &req, "fake-hash")
            .unwrap();
        assert_eq!(
            store
                .native_commit_page(job.id, &req, &page("next"), now(), &codes())
                .unwrap(),
            0
        );
        store.native_pause_on_restart().unwrap();
        assert_eq!(store.native_active().unwrap().unwrap().status, "paused");
        assert_eq!(store.native_calls_window(now() - 86400.).unwrap().0, 2);
    }
    #[test]
    fn pending_errors_survive_restart_and_are_mirrored_without_credentials() {
        let store = Store::open_in_memory().unwrap();
        let job = store.native_create(&config()).unwrap();
        let req = request("pending", 0, "");
        store
            .native_record_request(&job, &req, "fake-hash")
            .unwrap();
        store.native_interrupt_pending(job.id).unwrap();
        let calls = store.native_calls_since(0., 100).unwrap();
        assert_eq!(calls[0].outcome, "interrupted");
        let list = store.list_calls_since(0., 100).unwrap();
        assert_eq!(list[0].source, "native_list");
        assert_eq!(list[0].factors.transport.as_deref(), Some("native"));
        assert_eq!(list[0].factors.cgi_cmdid, Some(5814));
        assert!(list[0].factors.uin_hash.is_none());
        assert!(crate::history::start(
            &store,
            "FAKE",
            &crate::model::HistoryTarget::default(),
            &crate::history::HistoryConfig::default()
        )
        .is_err());
        assert!(crate::sweep::start_check(&store).is_err());
        assert_eq!(store.list_calls_window(None, 86400).unwrap().0, 0);
        assert_eq!(
            crate::ratelimit::stats(&store, 86400).unwrap().totals.calls,
            0
        );
    }
}
