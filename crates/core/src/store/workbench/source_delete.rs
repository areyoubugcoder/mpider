//! 公众号删除：事务取消关联工作并物理移除文章，以删除时点和落盘屏障挡住晚到结果。
use super::*;
use std::path::Path;

pub(super) fn accept_observation(tx: &Transaction<'_>, biz: &str, request_ts: f64) -> Result<bool> {
    ensure!(
        request_ts.is_finite() && request_ts >= 0.,
        "主页请求时间无效"
    );
    let deleted: Option<f64> = tx
        .query_row(
            "SELECT deleted_at FROM wb_source_deletions WHERE biz=?",
            [biz],
            |r| r.get(0),
        )
        .optional()?;
    Ok(deleted.is_none_or(|at| request_ts > at))
}

pub(super) fn ensure_live_article(tx: &Transaction<'_>, id: i64) -> Result<()> {
    let live: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM articles WHERE id=? AND is_deleted=0)",
        [id],
        |r| r.get(0),
    )?;
    ensure!(live, "文章已删除，忽略晚到下载结果");
    Ok(())
}

impl Store {
    /// 归档的文件落盘与删除使用同一写事务屏障，检查后到写入之间不允许删除穿过。
    pub fn wb_with_live_article<T>(&self, id: i64, write: impl FnOnce() -> Result<T>) -> Result<T> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        ensure_live_article(&tx, id)?;
        let value = write()?;
        tx.commit()?;
        Ok(value)
    }

    /// 按不可复用的文章 ID 更新正文；不能使用 upsert 重建已经物理删除的旧文章。
    pub fn wb_save_article_body(
        &self,
        id: i64,
        html: &str,
        markdown: &str,
        title: Option<&str>,
    ) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        ensure_live_article(&tx, id)?;
        tx.execute("UPDATE articles SET content_html=?,content_md=?,title=COALESCE(?,title),detail_done=1,updated_at=? WHERE id=?", params![html,markdown,title,now(),id])?;
        tx.commit()?;
        Ok(())
    }

    /// 仅作用于已知文章目录 ID，不信任归档文档中的任意路径；失败项留队重试。
    pub fn wb_cleanup_deleted_archives(&self, root: &Path) -> Result<usize> {
        let mut conn = self.conn();
        let pending: usize =
            conn.query_row("SELECT COUNT(*) FROM wb_archive_cleanup", [], |r| r.get(0))?;
        if pending == 0 {
            return Ok(0);
        }
        if let Ok(meta) = std::fs::symlink_metadata(root) {
            ensure!(
                meta.is_dir() && !meta.file_type().is_symlink(),
                "缓存根目录不可用，稍后重试清理"
            );
        }
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let ids = {
            let mut q = tx.prepare("SELECT article_id FROM wb_archive_cleanup")?;
            let rows = q.query_map([], |r| r.get::<_, i64>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for id in ids {
            let directory = root.join("archive/articles").join(id.to_string());
            if let Some(parent) = directory.parent().filter(|parent| parent.exists()) {
                let safe = std::fs::canonicalize(root)
                    .ok()
                    .zip(std::fs::canonicalize(parent).ok())
                    .is_some_and(|(root, parent)| parent.starts_with(root));
                if !safe {
                    continue;
                }
            }
            let removed = match std::fs::symlink_metadata(&directory) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
                Ok(meta) if meta.file_type().is_symlink() => {
                    std::fs::remove_file(&directory).is_ok()
                }
                Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&directory).is_ok(),
                Ok(_) => std::fs::remove_file(&directory).is_ok(),
                Err(_) => false,
            };
            if removed {
                tx.execute("DELETE FROM wb_archive_cleanup WHERE article_id=?", [id])?;
            }
        }
        let pending = tx.query_row("SELECT COUNT(*) FROM wb_archive_cleanup", [], |r| r.get(0))?;
        tx.commit()?;
        Ok(pending)
    }

    /// 不可撤销地删除公众号全部文章与评论；保留任务、请求和受保护会话证据供审计。
    pub fn wb_delete_source(&self, biz: &str, root: &Path) -> Result<SourceDeleteReceipt> {
        ensure!(
            !biz.trim().is_empty() && biz.len() <= 1024,
            "公众号标识无效"
        );
        let mut receipt = SourceDeleteReceipt::default();
        {
            let mut conn = self.conn();
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM accounts WHERE biz=? AND is_deleted=0)",
                [biz],
                |r| r.get(0),
            )?;
            ensure!(exists, "公众号已删除或不存在，请刷新后重试");
            let ids = {
                let mut q = tx.prepare("SELECT id FROM articles WHERE biz=?")?;
                let rows = q.query_map([biz], |r| r.get::<_, i64>(0))?;
                rows.collect::<rusqlite::Result<HashSet<_>>>()?
            };
            let ts = now();
            tx.execute("INSERT INTO wb_source_deletions VALUES(?,?) ON CONFLICT(biz) DO UPDATE SET deleted_at=excluded.deleted_at", params![biz,ts])?;
            let tasks: Vec<Task> = {
                let mut q = tx.prepare("SELECT document FROM wb_tasks WHERE json_extract(document,'$.state') NOT IN ('done','cancelled')")?;
                let rows = q.query_map([], |r| decode(r, 0))?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            for mut task in tasks {
                let associated = task.biz.as_deref() == Some(biz);
                let mut affected = associated;
                if task.kind == "archive" {
                    let original: Vec<i64> =
                        serde_json::from_value(task.target["article_ids"].clone())
                            .unwrap_or_default();
                    let remaining: Vec<i64> = original
                        .iter()
                        .copied()
                        .filter(|id| !ids.contains(id))
                        .collect();
                    if remaining.len() != original.len() {
                        affected = true;
                        task.target["article_ids"] = json!(remaining);
                        task.progress.finished_ids.retain(|id| !ids.contains(id));
                        task.progress.completed = task.progress.finished_ids.len() as u32;
                        // 从剩余文章的归档证据重算，不能丢掉其他公众号的失败/缺图重试入口。
                        task.progress.failed = 0;
                        task.progress.skipped = 0;
                        task.progress.partial = 0;
                        let mut verified = Vec::new();
                        for id in &task.progress.finished_ids {
                            let state: Option<String> = tx.query_row("SELECT json_extract(document,'$.state') FROM wb_archives WHERE article_id=?", [id], |r| r.get(0)).optional()?;
                            let has_body: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM articles WHERE id=? AND is_deleted=0 AND length(trim(COALESCE(content_md,''),char(9)||char(10)||char(13)||char(32)))>0)", [id], |r| r.get(0))?;
                            match state.as_deref() {
                                Some("failed") => task.progress.failed += 1,
                                Some("partial") => task.progress.partial += 1,
                                Some("complete") if has_body => {}
                                _ if has_body => task.progress.skipped += 1,
                                // 无法确认完成的项重新排队，已下载正文会走本地跳过。
                                _ => continue,
                            }
                            verified.push(*id);
                        }
                        task.progress.finished_ids = verified;
                        task.progress.completed = task.progress.finished_ids.len() as u32;
                        if !remaining.is_empty() {
                            task.biz = None;
                        }
                    }
                    if !affected {
                        continue;
                    }
                    if !remaining.is_empty() {
                        task.reason = Some("已移除被删除公众号的文章，其余下载继续".into());
                        task.updated_at = ts;
                        put(&tx, "wb_tasks", task.id, &task)?;
                        continue;
                    }
                } else if !associated {
                    continue;
                }
                task.state = "cancelled".into();
                task.reason = Some("公众号已删除，关联任务已取消".into());
                task.updated_at = ts;
                task.next_action_at = 0.;
                if let Some(r) = &mut task.progress.session_recovery {
                    r.phase = "interrupted".into();
                }
                put(&tx, "wb_tasks", task.id, &task)?;
                receipt.cancelled_tasks += 1;
            }
            let experiments: Vec<Experiment> = {
                let mut q = tx.prepare("SELECT e.document FROM wb_experiments e JOIN wb_cursors c ON c.id=json_extract(e.document,'$.cursor_id') WHERE c.biz=? AND json_extract(e.document,'$.state')='running'")?;
                let rows = q.query_map([biz], |r| decode(r, 0))?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            for mut e in experiments {
                e.state = "cancelled".into();
                e.phase = "cancelled".into();
                e.next_action_at = None;
                e.conclusion = Some("公众号已删除，实验已取消".into());
                put(&tx, "wb_experiments", e.id, &e)?;
                receipt.cancelled_experiments += 1;
            }
            tx.execute(
                "INSERT OR IGNORE INTO wb_archive_cleanup SELECT id FROM articles WHERE biz=?",
                [biz],
            )?;
            tx.execute(
                "DELETE FROM wb_archives WHERE article_id IN (SELECT id FROM articles WHERE biz=?)",
                [biz],
            )?;
            tx.execute(
                "DELETE FROM comments WHERE article_id IN (SELECT id FROM articles WHERE biz=?)",
                [biz],
            )?;
            receipt.deleted_articles = tx.execute("DELETE FROM articles WHERE biz=?", [biz])?;
            tx.execute("DELETE FROM wb_coverage WHERE biz=?", [biz])?;
            tx.execute("DELETE FROM wb_sources WHERE biz=?", [biz])?;
            tx.execute(
                "UPDATE accounts SET is_deleted=1,sweep_enabled=0 WHERE biz=?",
                [biz],
            )?;
            tx.execute("UPDATE history_jobs SET status='cancelled',finished_at=?,paused_reason=NULL,resume_at=NULL,next_page_at=NULL WHERE biz=? AND status IN ('running','paused')", params![ts,biz])?;
            tx.execute("UPDATE native_jobs SET status='cancelled' WHERE biz=? AND status IN ('running','paused')", [biz])?;
            super::super::system_logs::write(
                &tx,
                "storage",
                "info",
                "公众号与全部文章已删除，关联工作已停止",
                json!({"biz":biz,"count":receipt.deleted_articles,"cancelled_tasks":receipt.cancelled_tasks,"cancelled_experiments":receipt.cancelled_experiments}),
            )?;
            tx.commit()?;
        }
        // 数据删除一旦提交不因文件占用回滚；队列跨重启保留并由归档循环重试。
        receipt.cleanup_pending = self.wb_cleanup_deleted_archives(root).unwrap_or_else(|_| {
            self.conn()
                .query_row("SELECT COUNT(*) FROM wb_archive_cleanup", [], |r| r.get(0))
                .unwrap_or(1)
        });
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ArticleFields;
    use std::{path::PathBuf, sync::Arc};

    fn root() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "mpider-delete-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
    fn page(biz: &str) -> NativePage {
        NativePage {
            biz: biz.into(),
            user_name: format!("fake-{biz}"),
            nickname: Some("虚构公众号".into()),
            avatar: None,
            cursor: format!("fake-cursor-{biz}"),
            is_end: false,
            articles: vec![ParsedArticle {
                mid: "fake-mid".into(),
                idx: 1,
                title: Some("虚构文章".into()),
                content_url: Some("https://example.invalid/article".into()),
                ..Default::default()
            }],
        }
    }
    fn register(s: &Store, biz: &str) -> (Source, i64) {
        s.wb_capture(
            &page(biz),
            &NativeConfig::default(),
            &format!("fake-session-{biz}"),
            &format!("fake-evidence-{biz}"),
            1000.,
        )
        .unwrap();
        (
            s.wb_source(biz).unwrap().unwrap(),
            s.list_articles(Some(biz), 10, 0, false).unwrap()[0].id,
        )
    }

    #[test]
    fn source_delete_removes_all_bodies_comments_and_cache_and_only_related_work() {
        let s = Store::open_in_memory().unwrap();
        let dir = root();
        let (a, aid) = register(&s, "DELETE_A");
        let (_, bid) = register(&s, "KEEP_B");
        s.upsert_article(
            "DELETE_A",
            "deleted-before",
            1,
            &ArticleFields {
                is_deleted: Some(1),
                content_md: Some("旧删除正文".into()),
                ..Default::default()
            },
        )
        .unwrap();
        s.conn()
            .execute(
                "INSERT INTO comments(article_id,content_id,content) VALUES(?,'fake','虚构评论')",
                [aid],
            )
            .unwrap();
        let cache = dir.join("archive/articles").join(aid.to_string());
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("article.md"), "虚构正文").unwrap();
        let keep = dir.join("archive/articles").join(bid.to_string());
        std::fs::create_dir_all(&keep).unwrap();
        std::fs::write(keep.join("article.md"), "保留正文").unwrap();
        let history = s
            .wb_history_create(
                vec![a.biz.clone()],
                HistoryTarget {
                    count: Some(10),
                    ..Default::default()
                },
            )
            .unwrap()[0];
        let home = s.wb_home_create(&a.biz).unwrap();
        let experiment = s
            .wb_experiment_create(a.current_cursor_id.unwrap(), vec![1.], 1000.)
            .unwrap();
        let download = s
            .wb_download_enqueue(vec![aid, bid])
            .unwrap()
            .task_id
            .unwrap();
        let mut old = s.wb_task(download).unwrap();
        let result = s.wb_delete_source(&a.biz, &dir).unwrap();
        assert_eq!(result.deleted_articles, 2);
        assert_eq!(result.cancelled_tasks, 2);
        assert_eq!(result.cancelled_experiments, 1);
        assert_eq!(result.cleanup_pending, 0);
        assert!(s.wb_source(&a.biz).unwrap().is_none());
        assert!(s
            .list_articles(Some(&a.biz), 10, 0, false)
            .unwrap()
            .is_empty());
        assert!(s.get_article_md(aid).unwrap().is_none());
        assert_eq!(
            s.conn()
                .query_row("SELECT COUNT(*) FROM comments", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(!cache.exists());
        assert!(keep.join("article.md").exists());
        assert_eq!(s.wb_task(history).unwrap().state, "cancelled");
        assert_eq!(s.wb_task(home).unwrap().state, "cancelled");
        assert_eq!(s.wb_experiment(experiment).unwrap().state, "cancelled");
        assert_eq!(
            s.wb_task(download).unwrap().target["article_ids"],
            json!([bid])
        );
        assert_eq!(s.wb_task(download).unwrap().state, "queued");
        old.biz = Some(a.biz.clone());
        old.progress.finished_ids = vec![aid, bid];
        old.progress.completed = 2;
        old.progress.failed = 1;
        old.progress.partial = 1;
        old.progress.skipped = 1;
        s.wb_save_task(&old).unwrap();
        let saved = s.wb_task(download).unwrap();
        assert_eq!(saved.target["article_ids"], json!([bid]));
        assert!(saved.progress.finished_ids.is_empty());
        assert_eq!(saved.progress.completed, 0);
        assert_eq!(saved.progress.failed, 0);
        assert_eq!(saved.progress.partial, 0);
        assert_eq!(saved.progress.skipped, 0);
        assert_eq!(saved.biz, None);
        assert!(s.wb_delete_source(&a.biz, &dir).is_err());
        assert_eq!(
            s.list_articles(Some("KEEP_B"), 10, 0, false).unwrap().len(),
            1
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn source_delete_rejects_old_observations_and_old_article_workers_after_readding() {
        let s = Store::open_in_memory().unwrap();
        let dir = root();
        let (source, aid) = register(&s, "DELETE_LATE");
        let history = s
            .wb_history_create(
                vec![source.biz.clone()],
                HistoryTarget {
                    count: Some(10),
                    ..Default::default()
                },
            )
            .unwrap()[0];
        let run = s.wb_discovery_start(None).unwrap();
        s.wb_discovery_state(run, "running", None).unwrap();
        let request_ts = now();
        s.wb_delete_source(&source.biz, &dir).unwrap();
        let after = now() + 1.;
        s.wb_discovery_admit(run, &source.biz, true, request_ts)
            .unwrap();
        s.wb_import_legacy_checkpoint(&source.biz, "fake-legacy", "fake-evidence", 1000.)
            .unwrap();
        assert_eq!(s.wb_discovery().unwrap().unwrap().state, "running");
        assert!(!s
            .wb_discovery_observe_home(run, &page(&source.biz), request_ts, after)
            .unwrap());
        assert!(!s
            .wb_discovery_capture(
                run,
                request_ts,
                &page(&source.biz),
                &NativeConfig::default(),
                "fake-late-session",
                "fake-late-evidence",
                after
            )
            .unwrap());
        // 缺少请求时间的兼容消息也不能把已经删除的公众号复活。
        assert!(!s
            .wb_discovery_observe_home(run, &page(&source.biz), 0., after)
            .unwrap());
        assert!(s.wb_source(&source.biz).unwrap().is_none());
        assert!(s
            .wb_experiment_create_with_session(
                source.current_cursor_id.unwrap(),
                vec![1.],
                Some(&source.session_id),
                after
            )
            .is_err());
        assert!(s
            .wb_discovery_observe_home(run, &page(&source.biz), after, after)
            .unwrap());
        assert!(s
            .wb_discovery_capture(
                run,
                after,
                &page(&source.biz),
                &NativeConfig::default(),
                "fake-new-session",
                "fake-new-evidence",
                after
            )
            .unwrap());
        s.wb_discovery_admit(run, &source.biz, true, request_ts)
            .unwrap();
        s.wb_import_legacy_checkpoint(&source.biz, "fake-legacy", "fake-evidence", 1000.)
            .unwrap();
        s.wb_history_commit(
            history,
            &page(&source.biz),
            &source.session_id,
            "fake-old-history",
            after,
        )
        .unwrap();
        assert!(s
            .wb_experiment_create_with_session(
                source.current_cursor_id.unwrap(),
                vec![1.],
                Some(&source.session_id),
                after
            )
            .is_err());
        assert!(s
            .list_articles(Some(&source.biz), 10, 0, false)
            .unwrap()
            .is_empty());
        assert!(s
            .wb_save_article_body(aid, "旧HTML", "旧正文", None)
            .is_err());
        let file = dir
            .join("archive/articles")
            .join(aid.to_string())
            .join("article.md");
        assert!(s
            .wb_with_live_article(aid, || crate::workbench::archive::atomic_write(
                &file, b"old"
            ))
            .is_err());
        assert!(!file.exists());
        let fresh = s
            .upsert_article(&source.biz, "fake-mid", 1, &ArticleFields::default())
            .unwrap();
        assert!(fresh > aid);
        assert!(s
            .wb_save_article_body(aid, "旧HTML", "旧正文", None)
            .is_err());
        assert!(!s.wb_body_available(fresh).unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn source_delete_waits_for_current_cache_write_and_blocks_later_write() {
        let dir = root();
        let s = Arc::new(Store::open(dir.join("barrier.db")).unwrap());
        let deleter = Store::open(dir.join("barrier.db")).unwrap();
        let (_, aid) = register(&s, "DELETE_WRITE");
        let file = dir
            .join("archive/articles")
            .join(aid.to_string())
            .join("article.md");
        let (entered, ready) = std::sync::mpsc::channel();
        let (release, resume) = std::sync::mpsc::channel();
        let writer = s.clone();
        let destination = file.clone();
        let worker = std::thread::spawn(move || {
            writer
                .wb_with_live_article(aid, || {
                    entered.send(()).unwrap();
                    resume.recv().unwrap();
                    crate::workbench::archive::atomic_write(&destination, b"fake-body")
                })
                .unwrap()
        });
        ready.recv().unwrap();
        let target = dir.clone();
        let deletion =
            std::thread::spawn(move || deleter.wb_delete_source("DELETE_WRITE", &target).unwrap());
        release.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(deletion.join().unwrap().cleanup_pending, 0);
        assert!(!file.exists());
        assert!(s
            .wb_with_live_article(aid, || crate::workbench::archive::atomic_write(
                &file, b"late"
            ))
            .is_err());
        assert!(!file.exists());
        drop(s);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn source_delete_persists_failed_cache_cleanup_and_retries_after_restart() {
        let dir = root();
        let db = dir.join("cleanup.db");
        let s = Store::open(&db).unwrap();
        register(&s, "DELETE_RETRY");
        // 用普通文件阻挡缓存根目录，模拟路径不可用；不依赖测试进程权限。
        let blocked_root = dir.join("blocked-root");
        std::fs::write(&blocked_root, b"fake-obstacle").unwrap();
        let receipt = s.wb_delete_source("DELETE_RETRY", &blocked_root).unwrap();
        assert_eq!(receipt.cleanup_pending, 1);
        assert!(s
            .list_articles(Some("DELETE_RETRY"), 10, 0, false)
            .unwrap()
            .is_empty());
        drop(s);
        std::fs::remove_file(&blocked_root).unwrap();
        std::fs::create_dir_all(&blocked_root).unwrap();
        let reopened = Store::open(&db).unwrap();
        assert_eq!(
            reopened.wb_cleanup_deleted_archives(&blocked_root).unwrap(),
            0
        );
        drop(reopened);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn source_delete_retains_other_source_failure_and_partial_retry_evidence() {
        let dir = root();
        let s = Store::open_in_memory().unwrap();
        let (_, aid) = register(&s, "DELETE_STATS_A");
        let (_, bid) = register(&s, "KEEP_STATS_B");
        let cid = s
            .upsert_article("KEEP_STATS_B", "partial", 1, &ArticleFields::default())
            .unwrap();
        let task_id = s
            .wb_download_enqueue(vec![aid, bid, cid])
            .unwrap()
            .task_id
            .unwrap();
        s.wb_save_article_body(cid, "虚构HTML", "虚构缺图正文", None)
            .unwrap();
        for (id, state) in [(bid, "failed"), (cid, "partial")] {
            s.wb_save_archive(&ArchiveRecord {
                article_id: id,
                state: state.into(),
                directory: String::new(),
                missing_images: vec![],
                updated_at: now(),
                error: None,
            })
            .unwrap();
        }
        let mut task = s.wb_task(task_id).unwrap();
        task.progress.finished_ids = vec![bid, cid];
        task.progress.completed = 2;
        task.progress.failed = 1;
        task.progress.partial = 1;
        s.wb_save_task(&task).unwrap();
        s.wb_delete_source("DELETE_STATS_A", &dir).unwrap();
        let kept = s.wb_task(task_id).unwrap();
        assert_eq!(kept.target["article_ids"], json!([bid, cid]));
        assert_eq!(kept.progress.finished_ids, vec![bid, cid]);
        assert_eq!(kept.progress.completed, 2);
        assert_eq!(kept.progress.failed, 1);
        assert_eq!(kept.progress.partial, 1);
        assert!(s.wb_archive(bid).unwrap().is_some());
        assert!(s.wb_archive(cid).unwrap().is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
