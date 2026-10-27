//! 按公众号创建首页同步，首页文章仅在显式任务成功后入库。
use super::*;

pub(super) fn ensure_source(tx: &Transaction<'_>, biz: &str) -> Result<Source> {
    if let Some(source) = tx
        .query_row("SELECT document FROM wb_sources WHERE biz=?", [biz], |r| {
            decode(r, 0)
        })
        .optional()?
    {
        return Ok(source);
    }
    let (nickname, last_seen): (Option<String>, f64) = tx
        .query_row(
            "SELECT nickname,COALESCE(last_seen,0) FROM accounts WHERE biz=? AND is_deleted=0",
            [biz],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .context("公众号不存在，请先开启监测并打开其主页")?;
    let source = Source {
        biz: biz.into(),
        nickname: nickname.unwrap_or_default(),
        user_name: String::new(),
        session_id: String::new(),
        current_cursor_id: None,
        current_cursor_at: 0.,
        home_cursor_id: None,
        history_cursor_id: None,
        pending_count: 0,
        ended: false,
        article_count: tx.query_row("SELECT COUNT(*) FROM articles WHERE biz=?", [biz], |r| {
            r.get(0)
        })?,
        updated_at: last_seen,
        boundary_keys: vec![],
    };
    save_source(tx, &source, &[])?;
    Ok(source)
}

impl Store {
    /// 重复点击复用原任务，不重置暂停状态或重复发送首页。
    pub fn wb_home_create(&self, biz: &str) -> Result<i64> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        ensure_source(&tx, biz)?;
        let existing: Option<i64> = tx.query_row("SELECT id FROM wb_tasks WHERE json_extract(document,'$.kind')='home' AND json_extract(document,'$.biz')=? AND json_extract(document,'$.state') IN ('queued','running','paused','needs_action') ORDER BY id DESC LIMIT 1", [biz], |r| r.get(0)).optional()?;
        let id = match existing {
            Some(id) => id,
            None => create_task(&tx, "home", Some(biz.into()), json!({}))?,
        };
        tx.commit()?;
        Ok(id)
    }
    /// 同一事务校验任务状态及响应身份，更新首页而保留历史检查点和缓冲。
    pub fn wb_home_commit(
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
        ensure!(task.kind == "home", "不是首页同步任务");
        if !["queued", "running"].contains(&task.state.as_str()) {
            return Ok(());
        }
        ensure!(
            task.biz.as_deref() == Some(&page.biz),
            "首页响应公众号不一致"
        );
        let session_biz: String = tx.query_row(
            "SELECT biz FROM wb_sessions WHERE id=?",
            [session_id],
            |r| r.get(0),
        )?;
        ensure!(session_biz == page.biz, "首页会话公众号不一致");
        let (mut source, pending): (Source, Vec<ParsedArticle>) = tx.query_row(
            "SELECT document,pending FROM wb_sources WHERE biz=?",
            [&page.biz],
            |r| Ok((decode(r, 0)?, decode(r, 1)?)),
        )?;
        let inserted = upsert_page(&tx, page)?;
        let cursor = save_cursor(
            &tx,
            &page.biz,
            session_id,
            None,
            Some(1),
            page,
            evidence,
            ts,
        )?;
        if source.session_id == session_id {
            source.home_cursor_id = cursor;
            promote_current(&tx, &mut source, cursor, ts)?;
        }
        source.nickname = page.nickname.clone().unwrap_or(source.nickname);
        source.article_count = tx.query_row(
            "SELECT COUNT(*) FROM articles WHERE biz=?",
            [&page.biz],
            |r| r.get(0),
        )?;
        source.updated_at = source.updated_at.max(ts);
        save_source(&tx, &source, &pending)?;
        task.progress.pages = 1;
        task.progress.received_articles = Some(page.articles.len() as u32);
        task.progress.new_articles = inserted;
        task.progress.completed = page.articles.len() as u32;
        task.progress.session_recovery = None;
        task.progress.blocked_session_id = None;
        task.state = "done".into();
        task.reason = None;
        task.updated_at = ts;
        put(&tx, "wb_tasks", id, &task)?;
        super::super::system_logs::write(
            &tx,
            "history",
            "info",
            "首页同步完成，历史进度保持不变",
            json!({"task_id":id,"biz":page.biz,"received_articles":page.articles.len(),"new_articles":inserted,"cursor_id":cursor}),
        )?;
        tx.commit()?;
        Ok(())
    }
}
