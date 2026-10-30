//! 推送候选、事件与文章提交同事务；运行世代挡住停止/重启后的晚到结果。
use super::*;
use crate::workbench::push_monitor::{
    Candidate, Config, Metadata, MonitorEvent, Run, Scan, Status, MAX_CANDIDATES, PURPOSE,
};
use anyhow::{ensure, Context};
use rusqlite::Transaction;
use serde_json::json;

pub(super) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS push_monitor(id INTEGER PRIMARY KEY CHECK(id=1),document TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS push_candidates(
 id INTEGER PRIMARY KEY AUTOINCREMENT, key TEXT NOT NULL UNIQUE,url TEXT NOT NULL,
 first_seen_at REAL NOT NULL,state TEXT NOT NULL DEFAULT 'pending',attempts INTEGER NOT NULL DEFAULT 0,
 retry_at REAL NOT NULL DEFAULT 0,finished_at REAL,biz TEXT NOT NULL DEFAULT '',
 nickname TEXT NOT NULL DEFAULT '',title TEXT NOT NULL DEFAULT '',published_at INTEGER,
 article_id INTEGER,reason TEXT
);
CREATE INDEX IF NOT EXISTS push_pending ON push_candidates(state,retry_at,id);
CREATE INDEX IF NOT EXISTS push_event_time ON push_candidates(COALESCE(finished_at,first_seen_at) DESC,id DESC);
CREATE INDEX IF NOT EXISTS push_request_budget ON wb_requests(json_extract(document,'$.purpose'),json_extract(document,'$.started_at'));
"#;
fn run_from(conn: &Connection) -> Result<Run> {
    let text: Option<String> = conn
        .query_row("SELECT document FROM push_monitor WHERE id=1", [], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(text
        .map(|text| serde_json::from_str(&text))
        .transpose()?
        .unwrap_or_default())
}
fn save(tx: &Transaction<'_>, run: &Run) -> Result<()> {
    tx.execute("INSERT INTO push_monitor VALUES(1,?) ON CONFLICT(id) DO UPDATE SET document=excluded.document", [serde_json::to_string(run)?])?;
    Ok(())
}
fn active(tx: &Transaction<'_>, id: i64) -> Result<bool> {
    let r = run_from(tx)?;
    Ok(r.active && r.id == id)
}
fn terminal(
    tx: &Transaction<'_>,
    candidate: i64,
    state: &str,
    reason: Option<&str>,
    ts: f64,
) -> Result<()> {
    tx.execute(
        "UPDATE push_candidates SET state=?,reason=?,finished_at=? WHERE id=?",
        params![state, reason, ts, candidate],
    )?;
    Ok(())
}
impl Store {
    pub fn push_run(&self) -> Result<Run> {
        run_from(&self.conn())
    }
    pub fn push_is_active(&self, id: i64) -> Result<bool> {
        let r = self.push_run()?;
        Ok(r.active && r.id == id)
    }
    /// 此底层入口也供离线测试使用；桌面 IPC 在进入前检查 Windows 平台。
    pub fn push_start(&self, config: Config) -> Result<i64> {
        config.validate()?;
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let old = run_from(&tx)?;
        ensure!(!old.active, "推送监测已经开启");
        let run = Run {
            id: old.id.checked_add(1).context("监测运行编号已耗尽")?,
            active: true,
            phase: "starting".into(),
            config,
            started_at: now(),
            ..Default::default()
        };
        // 上次停止时正在处理的候选重新排队；终态和历史请求预算保留。
        tx.execute(
            "UPDATE push_candidates SET state='pending' WHERE state='fetching'",
            [],
        )?;
        tx.execute("DELETE FROM push_candidates WHERE state NOT IN ('pending','fetching') AND finished_at<?", [now()-30.*86400.])?;
        save(&tx, &run)?;
        system_logs::write(
            &tx,
            "push_monitor",
            "info",
            "推送监测已开启，已有待处理候选继续处理",
            json!({"monitor_run_id":run.id,"state":"starting"}),
        )?;
        tx.commit()?;
        Ok(run.id)
    }
    pub fn push_end(&self, id: i64, phase: &str, reason: Option<&str>) -> Result<()> {
        ensure!(["stopped", "failed"].contains(&phase), "监测状态无效");
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut run = run_from(&tx)?;
        if run.id != id || !run.active {
            return Ok(());
        }
        run.active = false;
        run.phase = phase.into();
        run.reason = reason.map(str::to_owned);
        run.next_scan_at = 0.;
        run.next_fetch_at = 0.;
        save(&tx, &run)?;
        system_logs::write(
            &tx,
            "push_monitor",
            if phase == "failed" { "error" } else { "info" },
            "推送监测已停止，候选队列已保留",
            json!({"monitor_run_id":id,"state":phase,"reason":reason}),
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn push_restart(&self) -> Result<()> {
        let id = self.push_run()?.id;
        self.push_end(
            id,
            "stopped",
            Some("应用已重启；待处理链接已保留，请手动开启推送监测"),
        )
    }
    pub fn push_scanned(&self, id: i64, scan: Scan, ts: f64) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if !active(&tx, id)? {
            return Ok(());
        }
        let mut run = run_from(&tx)?;
        if run.rounds.is_multiple_of(30) {
            tx.execute("DELETE FROM push_candidates WHERE state NOT IN ('pending','fetching') AND finished_at<?", [ts-30.*86400.])?;
        }
        let mut remaining = MAX_CANDIDATES.saturating_sub(tx.query_row(
            "SELECT COUNT(*) FROM push_candidates WHERE state IN ('pending','fetching')",
            [],
            |r| r.get::<_, usize>(0),
        )?);
        for link in scan.links.iter().take(MAX_CANDIDATES) {
            // 长链可在请求前按 biz 缩小范围；短链在解析出身份后再次检查。
            if !run.config.bizzes.is_empty()
                && link
                    .biz
                    .as_ref()
                    .is_some_and(|b| !run.config.bizzes.contains(b))
            {
                continue;
            }
            let known: Option<i64> = tx
                .query_row(
                    "SELECT id FROM push_candidates WHERE key=?",
                    [&link.key],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(candidate) = known {
                tx.execute("UPDATE push_candidates SET url=CASE WHEN instr(url,'chksm=')=0 AND instr(?,'chksm=')>0 THEN ? ELSE url END WHERE id=?",params![link.url,link.url,candidate])?;
            } else if remaining > 0 {
                tx.execute(
                    "INSERT INTO push_candidates(key,url,first_seen_at,biz) VALUES(?,?,?,?)",
                    params![link.key, link.url, ts, link.biz.as_deref().unwrap_or("")],
                )?;
                remaining -= 1;
            }
        }
        run.rounds += 1;
        run.last_scan_at = ts;
        run.next_scan_at = ts + run.config.scan_seconds as f64;
        run.processes = scan.processes;
        run.read_bytes = scan.read_bytes;
        run.phase = if scan.processes == 0 {
            "waiting_wechat"
        } else {
            "running"
        }
        .into();
        run.reason = if scan.processes > 0 && scan.inaccessible == scan.processes {
            Some("无法读取微信进程，请检查微信与 MPider 的权限是否一致".into())
        } else if remaining == 0 {
            Some("待处理候选已达上限，已保留当前队列；后续扫描继续发现其余链接".into())
        } else {
            None
        };
        save(&tx, &run)?;
        tx.commit()?;
        Ok(())
    }
    pub fn push_next(&self, id: i64, ts: f64) -> Result<Option<Candidate>> {
        let conn = self.conn();
        let run = run_from(&conn)?;
        if !run.active || run.id != id || ts < run.next_fetch_at {
            return Ok(None);
        }
        Ok(conn.query_row("SELECT id,url,attempts,first_seen_at FROM push_candidates WHERE state='pending' AND retry_at<=? ORDER BY (attempts=0) DESC,first_seen_at DESC,id DESC LIMIT 1",[ts],|r|Ok(Candidate{id:r.get(0)?,url:r.get(1)?,attempts:r.get(2)?,first_seen_at:r.get(3)?})).optional()?)
    }
    pub fn push_claim(&self, id: i64, candidate: i64) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if !active(&tx, id)? {
            return Ok(false);
        }
        let changed=tx.execute("UPDATE push_candidates SET state='fetching',attempts=attempts+1 WHERE id=? AND state='pending'",[candidate])?;
        let mut run = run_from(&tx)?;
        run.next_fetch_at = 0.;
        save(&tx, &run)?;
        tx.commit()?;
        Ok(changed == 1)
    }
    pub fn push_deferred(&self, id: i64, until: f64, reason: Option<&str>) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if !active(&tx, id)? {
            return Ok(());
        }
        let mut run = run_from(&tx)?;
        run.next_fetch_at = until;
        run.reason = reason.map(str::to_owned);
        save(&tx, &run)?;
        tx.commit()?;
        Ok(())
    }
    pub fn push_result(
        &self,
        id: i64,
        candidate: i64,
        outcome: &str,
        retry: bool,
        retry_at: f64,
    ) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if !active(&tx, id)? {
            return Ok(());
        }
        let attempts: u32 = tx.query_row(
            "SELECT attempts FROM push_candidates WHERE id=? AND state='fetching'",
            [candidate],
            |r| r.get(0),
        )?;
        let pending = retry && attempts < 3;
        tx.execute(
            "UPDATE push_candidates SET state=?,reason=?,retry_at=?,finished_at=? WHERE id=?",
            params![
                if pending { "pending" } else { outcome },
                outcome,
                retry_at,
                if pending { None } else { Some(now()) },
                candidate
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn push_import(&self, id: i64, candidate: i64, meta: &Metadata, ts: f64) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if !active(&tx, id)? {
            return Ok(());
        }
        let run = run_from(&tx)?;
        let first: f64 = tx.query_row(
            "SELECT first_seen_at FROM push_candidates WHERE id=? AND state='fetching'",
            [candidate],
            |r| r.get(0),
        )?;
        tx.execute(
            "UPDATE push_candidates SET biz=?,nickname=?,title=?,published_at=? WHERE id=?",
            params![
                meta.biz,
                meta.nickname,
                meta.title,
                meta.published_at,
                candidate
            ],
        )?;
        let ignored = if !run.config.bizzes.is_empty() && !run.config.bizzes.contains(&meta.biz) {
            Some("不在监测范围")
        } else if meta.published_at as f64 > ts + 1800. {
            Some("发布时间无效")
        } else if (meta.published_at as f64) < first - run.config.recent_hours as f64 * 3600. {
            Some("超出文章时间窗")
        } else {
            None
        };
        if let Some(reason) = ignored {
            terminal(&tx, candidate, "ignored", Some(reason), ts)?;
            tx.commit()?;
            return Ok(());
        }
        let deleted:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM wb_source_deletions WHERE biz=?1 AND deleted_at>=?2) OR EXISTS(SELECT 1 FROM accounts WHERE biz=?1 AND is_deleted=1) OR EXISTS(SELECT 1 FROM articles WHERE biz=?1 AND mid=?3 AND idx=?4 AND is_deleted=1)",params![meta.biz,first,meta.mid,meta.idx],|r|r.get(0))?;
        if deleted {
            terminal(
                &tx,
                candidate,
                "ignored",
                Some("公众号或文章已删除，忽略晚到结果"),
                ts,
            )?;
            tx.commit()?;
            return Ok(());
        }
        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM articles WHERE biz=? AND mid=? AND idx=?",
                params![meta.biz, meta.mid, meta.idx],
                |r| r.get(0),
            )
            .optional()?;
        tx.execute("INSERT INTO accounts(biz,nickname,first_seen,last_seen) VALUES(?,?,?,?) ON CONFLICT(biz) DO UPDATE SET nickname=excluded.nickname,last_seen=excluded.last_seen",params![meta.biz,meta.nickname,ts,ts])?;
        tx.execute("INSERT INTO articles(biz,mid,idx,sn,title,digest,content_url,cover,published_at,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(biz,mid,idx) DO UPDATE SET title=COALESCE(articles.title,excluded.title),digest=COALESCE(articles.digest,excluded.digest),content_url=excluded.content_url,updated_at=excluded.updated_at",params![meta.biz,meta.mid,meta.idx,meta.sn,meta.title,meta.digest,meta.url,meta.cover,meta.published_at,ts,ts])?;
        let article_id: i64 = tx.query_row(
            "SELECT id FROM articles WHERE biz=? AND mid=? AND idx=?",
            params![meta.biz, meta.mid, meta.idx],
            |r| r.get(0),
        )?;
        tx.execute(
            "UPDATE push_candidates SET article_id=? WHERE id=?",
            params![article_id, candidate],
        )?;
        terminal(
            &tx,
            candidate,
            if existing.is_some() {
                "existing"
            } else {
                "imported"
            },
            None,
            ts,
        )?;
        system_logs::write(
            &tx,
            "push_monitor",
            "info",
            "监测发现的文章已保存",
            json!({"monitor_run_id":id,"article_id":article_id,"biz":meta.biz,"new_articles":i64::from(existing.is_none())}),
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn push_status(&self) -> Result<Status> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let run = run_from(&tx)?;
        let pending = tx.query_row(
            "SELECT COUNT(*) FROM push_candidates WHERE state IN ('pending','fetching')",
            [],
            |r| r.get(0),
        )?;
        let imported = tx.query_row(
            "SELECT COUNT(*) FROM push_candidates WHERE state='imported'",
            [],
            |r| r.get(0),
        )?;
        let requests_hour=tx.query_row("SELECT COUNT(*) FROM wb_requests WHERE json_extract(document,'$.purpose')=? AND json_extract(document,'$.started_at')>?",params![PURPOSE,now()-3600.],|r|r.get(0))?;
        let events = {
            let mut q=tx.prepare("SELECT id,first_seen_at,finished_at,state,nickname,title,biz,article_id,published_at,reason FROM push_candidates ORDER BY COALESCE(finished_at,first_seen_at) DESC,id DESC LIMIT 100")?;
            let rows = q.query_map([], |r| {
                Ok(MonitorEvent {
                    id: r.get(0)?,
                    first_seen_at: r.get(1)?,
                    finished_at: r.get(2)?,
                    outcome: r.get(3)?,
                    nickname: r.get(4)?,
                    title: r.get(5)?,
                    biz: r.get(6)?,
                    article_id: r.get(7)?,
                    published_at: r.get(8)?,
                    reason: r.get(9)?,
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        tx.commit()?;
        Ok(Status {
            supported: cfg!(windows),
            run,
            pending,
            imported,
            requests_hour,
            events,
        })
    }
}
