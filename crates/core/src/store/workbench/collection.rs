use super::*;
use crate::workbench::collection::{self as jobs, Control};
use std::collections::HashMap;

#[derive(Clone, Default, serde::Serialize)]
pub struct CollectionStatus {
    pub interactions: bool,
    pub comments: bool,
    pub comments_complete: bool,
}
#[derive(Default, serde::Serialize)]
pub struct CollectionReceipt {
    pub task_id: Option<i64>,
    pub requested: usize,
    pub queued: usize,
    pub skipped: usize,
    pub already_queued: usize,
}

fn status(conn: &rusqlite::Connection, id: i64) -> Result<CollectionStatus> {
    Ok(conn.query_row("SELECT (read_num IS NOT NULL OR old_like_num IS NOT NULL OR like_num IS NOT NULL OR interaction_share_count IS NOT NULL OR interaction_read_display IS NOT NULL OR EXISTS(SELECT 1 FROM wb_article_collection c WHERE c.article_id=a.id AND interactions_at IS NOT NULL)),(comment_next_offset<0 OR EXISTS(SELECT 1 FROM comments c WHERE c.article_id=a.id)),comment_next_offset<0 FROM articles a WHERE id=? AND is_deleted=0", [id], |r| Ok(CollectionStatus { interactions:r.get(0)?,comments:r.get(1)?,comments_complete:r.get(2)? }))?)
}
fn active(task: &Task, control: Control) -> bool {
    jobs::is_collection(&task.kind)
        && task.state == "running"
        && task
            .progress
            .collection
            .as_ref()
            .is_some_and(|p| p.epoch == control.epoch && p.article_id == Some(control.article_id))
        && task.target["article_ids"]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|id| id.as_i64() == Some(control.article_id)))
}
pub(crate) fn guard(tx: &Transaction<'_>, article_id: i64) -> Result<()> {
    if let Some(control) = jobs::current() {
        let task: Task = tx.query_row(
            "SELECT document FROM wb_tasks WHERE id=?",
            [control.task_id],
            |r| decode(r, 0),
        )?;
        ensure!(
            control.article_id == article_id && active(&task, control),
            "任务已暂停或取消"
        );
    }
    Ok(())
}
impl Store {
    pub fn wb_collection_statuses(&self, ids: &[i64]) -> Result<HashMap<i64, CollectionStatus>> {
        ensure!(ids.len() <= 100000, "所选文章超过十万篇");
        let conn = self.conn();
        ids.iter()
            .map(|&id| Ok((id, status(&conn, id).unwrap_or_default())))
            .collect()
    }
    pub fn wb_collection_enqueue(
        &self,
        kind: &str,
        biz: Option<String>,
        ids: Vec<i64>,
        refresh: bool,
    ) -> Result<CollectionReceipt> {
        ensure!(jobs::is_collection(kind), "未知采集类型");
        ensure!(biz.is_none() || ids.is_empty(), "请选择公众号或文章集合");
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut ids = if let Some(biz) = &biz {
            let mut q=tx.prepare("SELECT id FROM articles WHERE biz=? AND is_deleted=0 ORDER BY published_at DESC,id DESC")?;
            let rows = q
                .query_map([biz], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<i64>>>()?;
            rows
        } else {
            ids
        };
        ids.sort_unstable();
        ids.dedup();
        ensure!(ids.len() <= 100000, "一次最多采集十万篇文章");
        let mut receipt = CollectionReceipt {
            requested: ids.len(),
            ..Default::default()
        };
        let occupied: HashSet<i64> = {
            let mut q=tx.prepare("SELECT value FROM wb_tasks,json_each(document,'$.target.article_ids') WHERE json_extract(document,'$.kind')=? AND json_extract(document,'$.state') NOT IN ('done','cancelled') AND value NOT IN (SELECT value FROM json_each(document,'$.progress.finished_ids'))")?;
            let rows = q
                .query_map([kind], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            rows
        };
        let mut pending = Vec::new();
        for id in ids {
            let current = status(&tx, id).context("文章已删除或不存在，请刷新后重试")?;
            if !refresh
                && if kind == "comments" {
                    current.comments_complete
                } else {
                    current.interactions
                }
            {
                receipt.skipped += 1;
            } else if occupied.contains(&id) {
                receipt.already_queued += 1;
            } else {
                pending.push(id);
            }
        }
        receipt.queued = pending.len();
        if !pending.is_empty() {
            receipt.task_id = Some(create_task(
                &tx,
                kind,
                biz,
                json!({"article_ids":pending,"refresh":refresh}),
            )?);
        }
        tx.commit()?;
        Ok(receipt)
    }
    pub fn wb_article_collection_task(&self, article_id: i64, kind: &str) -> Result<Option<Task>> {
        ensure!(jobs::is_collection(kind), "未知采集类型");
        Ok(self.conn().query_row("SELECT document FROM wb_tasks WHERE json_extract(document,'$.kind')=?1 AND EXISTS(SELECT 1 FROM json_each(document,'$.target.article_ids') WHERE value=?2) ORDER BY CASE WHEN json_extract(document,'$.state') IN ('queued','running','paused','needs_action') AND NOT EXISTS(SELECT 1 FROM json_each(document,'$.progress.finished_ids') WHERE value=?2) THEN 0 ELSE 1 END,id DESC LIMIT 1", params![kind,article_id], |r| decode(r,0)).optional()?)
    }
    pub(crate) fn wb_collection_active(&self, control: Control) -> Result<bool> {
        Ok(active(&self.wb_task(control.task_id)?, control))
    }
    pub(crate) fn wb_collection_claim(&self) -> Result<Option<Task>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let candidate:Option<Task>=tx.query_row("SELECT document FROM wb_tasks WHERE json_extract(document,'$.kind') IN ('comments','interactions') AND json_extract(document,'$.state') IN ('queued','running') ORDER BY id LIMIT 1",[],|r|decode(r,0)).optional()?;
        let Some(mut task) = candidate else {
            return Ok(None);
        };
        let ids: Vec<i64> = serde_json::from_value(task.target["article_ids"].clone())?;
        for id in ids {
            if task.progress.finished_ids.contains(&id) {
                continue;
            }
            let current = status(&tx, id).ok();
            if current.as_ref().is_none_or(|s| {
                task.target["refresh"] != true
                    && if task.kind == "comments" {
                        s.comments_complete
                    } else {
                        s.interactions
                    }
            }) {
                task.progress.finished_ids.push(id);
                task.progress.completed += 1;
                task.progress.skipped += 1;
                continue;
            }
            let epoch = task.progress.collection.as_ref().map_or(1, |p| p.epoch + 1);
            let title = tx.query_row(
                "SELECT COALESCE(title,'无标题') FROM articles WHERE id=?",
                [id],
                |r| r.get(0),
            )?;
            task.progress.collection = Some(jobs::Progress {
                epoch,
                article_id: Some(id),
                title,
                ..Default::default()
            });
            task.state = "running".into();
            task.reason = None;
            task.updated_at = now();
            put(&tx, "wb_tasks", task.id, &task)?;
            tx.commit()?;
            return Ok(Some(task));
        }
        task.state = "done".into();
        task.updated_at = now();
        put(&tx, "wb_tasks", task.id, &task)?;
        tx.commit()?;
        Ok(None)
    }
    pub(crate) fn wb_collection_page(
        &self,
        control: Control,
        replies: bool,
        added: u32,
        updated: u32,
        unchanged: u32,
    ) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        guard(&tx, control.article_id)?;
        let mut task: Task = tx.query_row(
            "SELECT document FROM wb_tasks WHERE id=?",
            [control.task_id],
            |r| decode(r, 0),
        )?;
        ensure!(active(&task, control), "任务已暂停或取消");
        let p = task.progress.collection.as_mut().unwrap();
        if replies {
            p.reply_pages += 1;
        } else {
            p.pages += 1;
            task.progress.pages += 1;
        }
        p.added += added;
        p.updated += updated;
        p.unchanged += unchanged;
        task.updated_at = now();
        put(&tx, "wb_tasks", task.id, &task)?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn wb_collection_finish(
        &self,
        control: Control,
        error: Option<String>,
    ) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut task: Task = tx.query_row(
            "SELECT document FROM wb_tasks WHERE id=?",
            [control.task_id],
            |r| decode(r, 0),
        )?;
        if !active(&task, control) {
            return Ok(());
        }
        if let Some(error) = error {
            task.state = "needs_action".into();
            task.reason = Some(error);
        } else {
            task.progress.finished_ids.push(control.article_id);
            task.progress.completed += 1;
            task.state = if task.progress.finished_ids.len()
                == task.target["article_ids"].as_array().map_or(0, Vec::len)
            {
                "done"
            } else {
                "queued"
            }
            .into();
            if task.kind == "interactions" {
                tx.execute("INSERT INTO wb_article_collection(article_id,interactions_at) VALUES(?,?) ON CONFLICT(article_id) DO UPDATE SET interactions_at=excluded.interactions_at",params![control.article_id,now()])?;
            }
        }
        task.updated_at = now();
        put(&tx, "wb_tasks", task.id, &task)?;
        tx.commit()?;
        Ok(())
    }
}
