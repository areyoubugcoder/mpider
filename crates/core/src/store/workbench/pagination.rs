//! 工作台真实总数与分页：独立于快照条数上限，计数和数据在同一读事务中取得。
use super::*;
fn validate(limit: i64, offset: i64) -> Result<()> {
    ensure!((1..=100).contains(&limit) && offset >= 0, "分页参数无效");
    Ok(())
}
impl Store {
    pub fn wb_source_page(
        &self,
        search: &str,
        limit: i64,
        offset: i64,
    ) -> Result<WorkbenchPage<SourceListItem>> {
        validate(limit, offset)?;
        ensure!(search.len() <= 1000, "搜索词过长");
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let base="FROM accounts a LEFT JOIN wb_sources s ON s.biz=a.biz WHERE a.is_deleted=0 AND (instr(lower(COALESCE(json_extract(s.document,'$.nickname'),a.nickname,'')),lower(?1))>0 OR instr(lower(a.biz),lower(?1))>0)";
        let total = tx.query_row(&format!("SELECT COUNT(*) {base}"), [search], |r| r.get(0))?;
        let items = {
            let mut q=tx.prepare(&format!("SELECT a.biz,COALESCE(a.nickname,''),COALESCE(a.last_seen,0),s.document,(SELECT COUNT(*) FROM articles art WHERE art.biz=a.biz AND art.is_deleted=0),(SELECT COUNT(*) FROM articles art WHERE art.biz=a.biz AND art.is_deleted=0 AND length(trim(COALESCE(art.content_md,''),char(9)||char(10)||char(13)||char(32)))>0) {base} ORDER BY COALESCE(json_extract(s.document,'$.updated_at'),a.last_seen,0) DESC,a.biz LIMIT ?2 OFFSET ?3"))?;
            let sources = q
                .query_map(params![search, limit, offset], |r| {
                    let doc: Option<String> = r.get(3)?;
                    let mut s = if let Some(text) = doc {
                        serde_json::from_str::<Source>(&text).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                3,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?
                    } else {
                        Source {
                            biz: r.get(0)?,
                            nickname: r.get(1)?,
                            user_name: String::new(),
                            session_id: String::new(),
                            current_cursor_id: None,
                            current_cursor_at: 0.,
                            home_cursor_id: None,
                            history_cursor_id: None,
                            pending_count: 0,
                            ended: false,
                            article_count: 0,
                            updated_at: r.get(2)?,
                            boundary_keys: vec![],
                        }
                    };
                    s.article_count = r.get(4)?;
                    Ok((s, r.get::<_, i64>(5)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut result = Vec::new();
            for (source, body_count) in sources {
                let home_task=tx.query_row("SELECT document FROM wb_tasks WHERE json_extract(document,'$.biz')=? AND json_extract(document,'$.kind')='home' ORDER BY id DESC LIMIT 1",[&source.biz],|r|decode(r,0)).optional()?;
                let history_task=tx.query_row("SELECT document FROM wb_tasks WHERE json_extract(document,'$.biz')=? AND json_extract(document,'$.kind')='history' AND json_extract(document,'$.target.pages') IS NULL AND json_extract(document,'$.state') NOT IN ('done','cancelled') ORDER BY id DESC LIMIT 1",[&source.biz],|r|decode(r,0)).optional()?;
                result.push(SourceListItem {
                    source,
                    body_count,
                    home_task,
                    history_task,
                });
            }
            result
        };
        tx.commit()?;
        Ok(WorkbenchPage { items, total })
    }
    pub fn wb_task_page(&self, kind: &str, limit: i64, offset: i64) -> Result<WorkbenchPage<Task>> {
        validate(limit, offset)?;
        ensure!(
            ["home", "history", "archive"].contains(&kind),
            "任务类型无效"
        );
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let base="FROM wb_tasks WHERE json_extract(document,'$.kind')=?1 AND (?1<>'archive' OR json_extract(document,'$.target.download_only')=1)";
        let total = tx.query_row(&format!("SELECT COUNT(*) {base}"), [kind], |r| r.get(0))?;
        let items = {
            let mut q = tx.prepare(&format!(
                "SELECT document {base} ORDER BY id DESC LIMIT ?2 OFFSET ?3"
            ))?;
            let v = q
                .query_map(params![kind, limit, offset], |r| decode(r, 0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            v
        };
        tx.commit()?;
        Ok(WorkbenchPage { items, total })
    }
    pub fn wb_runnable_tasks(&self, download: bool, ts: f64) -> Result<Vec<Task>> {
        let conn = self.conn();
        let mut q=conn.prepare("SELECT document FROM wb_tasks WHERE json_extract(document,'$.state') IN ('queued','running') AND COALESCE(json_extract(document,'$.next_action_at'),0)<=?1 AND ((?2=1 AND json_extract(document,'$.kind')='archive' AND json_extract(document,'$.target.download_only')=1) OR (?2=0 AND json_extract(document,'$.kind') IN ('home','history'))) ORDER BY json_extract(document,'$.updated_at'),id")?;
        let v = q
            .query_map(params![ts, download], |r| decode(r, 0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }
    pub fn wb_download_queue(&self) -> Result<DownloadQueue> {
        Ok(self.conn().query_row("WITH jobs AS (SELECT COALESCE(json_array_length(document,'$.target.article_ids'),0) n,MIN(COALESCE(json_extract(document,'$.progress.completed'),0),COALESCE(json_array_length(document,'$.target.article_ids'),0)) c,COALESCE(json_extract(document,'$.progress.skipped'),0) s,COALESCE(json_extract(document,'$.progress.failed'),0) f,json_extract(document,'$.state') state FROM wb_tasks WHERE json_extract(document,'$.kind')='archive' AND json_extract(document,'$.target.download_only')=1) SELECT COUNT(*),COALESCE(SUM(n),0),COALESCE(SUM(c),0),COALESCE(SUM(s),0),COALESCE(SUM(f),0),COALESCE(SUM(CASE WHEN state='cancelled' THEN n-c ELSE 0 END),0),COALESCE(SUM(CASE WHEN state<>'cancelled' THEN n-c ELSE 0 END),0) FROM jobs",[],|r|Ok(DownloadQueue{task_count:r.get(0)?,total:r.get(1)?,completed:r.get(2)?,skipped:r.get(3)?,failed:r.get(4)?,cancelled:r.get(5)?,pending:r.get(6)?}))?)
    }
    /// 在同一读事务内汇总全库当前队列，终结任务不污染进度。
    pub fn wb_active_queues(&self) -> Result<ActiveQueues> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut queues = ActiveQueues::default();
        for (kind, target) in [
            ("archive", &mut queues.download),
            ("history", &mut queues.history),
        ] {
            let sql = "WITH jobs AS (SELECT CASE WHEN ?1='archive' THEN json_array_length(document,'$.target.article_ids') WHEN json_extract(document,'$.target.pages') IS NULL THEN json_extract(document,'$.target.count') END n,COALESCE(json_extract(document,'$.progress.completed'),0) c FROM wb_tasks WHERE json_extract(document,'$.kind')=?1 AND json_extract(document,'$.state') IN ('queued','running','paused','needs_action') AND (?1<>'archive' OR json_extract(document,'$.target.download_only')=1)) SELECT COUNT(*),COALESCE(SUM(CASE WHEN n IS NULL THEN c ELSE MIN(c,n) END),0),CASE WHEN COUNT(*)=COUNT(n) THEN COALESCE(SUM(n),0) END FROM jobs";
            *target = tx.query_row(sql, [kind], |r| {
                Ok(QueueProgress {
                    task_count: r.get(0)?,
                    completed: r.get(1)?,
                    total: r.get(2)?,
                })
            })?;
        }
        tx.commit()?;
        Ok(queues)
    }
    /// 勾选集合跨页查询正文状态，不向界面传递正文内容。
    pub fn wb_article_body_states(
        &self,
        ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, bool>> {
        ensure!(ids.len() <= 100000, "所选文章超过十万篇");
        let mut states = ids
            .iter()
            .map(|id| (*id, false))
            .collect::<std::collections::HashMap<_, _>>();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        for chunk in ids.chunks(500) {
            let marks = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let mut q = tx.prepare(&format!("SELECT id,length(trim(COALESCE(content_md,''),char(9)||char(10)||char(13)||char(32)))>0 FROM articles WHERE is_deleted=0 AND id IN ({marks})"))?;
            for row in q.query_map(rusqlite::params_from_iter(chunk), |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, bool>(1)?))
            })? {
                let (id, ready) = row?;
                states.insert(id, ready);
            }
        }
        tx.commit()?;
        Ok(states)
    }
    pub fn wb_request_page(
        &self,
        category: &str,
        since: f64,
        limit: i64,
        offset: i64,
    ) -> Result<WorkbenchPage<RequestEvent>> {
        validate(limit, offset)?;
        ensure!(
            since.is_finite() && ["", "list", "body", "image"].contains(&category),
            "请求筛选无效"
        );
        let native="json_object('id',request_id,'category','list','purpose',CASE WHEN action=0 THEN 'home' ELSE 'legacy_page' END,'owner_id',job_id,'owner_kind','native','biz','','started_at',ts,'finished_at',CASE WHEN latency_ms IS NULL THEN NULL ELSE ts+latency_ms/1000.0 END,'outcome',outcome,'policy_version',policy_version,'gap_ms',gap_ms,'detail',json_object('transport',transport,'outer_ret',outer_ret,'jsapi_ret',jsapi_ret,'inner_ret',inner_ret,'reason',reason))";
        let base=format!("FROM (SELECT id,document,json_extract(document,'$.started_at') ts,json_extract(document,'$.category') category FROM wb_requests UNION ALL SELECT request_id,{native},ts,'list' FROM native_call_log) WHERE ts>=?1 AND (?2='' OR category=?2)");
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let total = tx.query_row(
            &format!("SELECT COUNT(*) {base}"),
            params![since, category],
            |r| r.get(0),
        )?;
        let items = {
            let mut q = tx.prepare(&format!(
                "SELECT document {base} ORDER BY ts DESC,id DESC LIMIT ?3 OFFSET ?4"
            ))?;
            let v = q
                .query_map(params![since, category, limit, offset], |r| decode(r, 0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            v
        };
        tx.commit()?;
        Ok(WorkbenchPage { items, total })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pagination_reads_complete_sources_tasks_and_old_runnable_jobs() {
        let s = Store::open_in_memory().unwrap();
        for n in 0..23 {
            s.upsert_account(
                &format!("PAGE_{n:02}"),
                Some(&format!("虚构号{n:02}")),
                None,
            )
            .unwrap();
        }
        let first = s.wb_source_page("PAGE_", 10, 0).unwrap();
        let second = s.wb_source_page("PAGE_", 10, 10).unwrap();
        let last = s.wb_source_page("PAGE_", 10, 20).unwrap();
        assert_eq!(
            (
                first.total,
                first.items.len(),
                second.items.len(),
                last.items.len()
            ),
            (23, 10, 10, 3)
        );
        let ids: std::collections::HashSet<_> = first
            .items
            .iter()
            .chain(&second.items)
            .chain(&last.items)
            .map(|r| &r.source.biz)
            .collect();
        assert_eq!(ids.len(), 23);
        assert_eq!(s.wb_source_page("虚构号05", 10, 0).unwrap().total, 1);
        assert!(s.wb_source_page("", 0, 0).is_err());
        let old = s.wb_home_create("PAGE_00").unwrap();
        let template = s.wb_task(old).unwrap();
        {
            let mut conn = s.conn();
            let tx = conn.transaction().unwrap();
            for id in 2..=1005 {
                let mut task = template.clone();
                task.id = id;
                task.biz = Some("PAGE_01".into());
                task.state = "done".into();
                tx.execute(
                    "INSERT INTO wb_tasks VALUES(?,?)",
                    rusqlite::params![id, serde_json::to_string(&task).unwrap()],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }
        assert!(!s.wb_tasks().unwrap().iter().any(|t| t.id == old));
        let paged = s.wb_task_page("home", 10, 1000).unwrap();
        assert_eq!(paged.total, 1005);
        assert_eq!(paged.items.len(), 5);
        assert!(paged.items.iter().any(|t| t.id == old));
        assert_eq!(
            s.wb_runnable_tasks(false, crate::model::now()).unwrap()[0].id,
            old
        );
        assert_eq!(
            s.wb_source_page("PAGE_00", 10, 0).unwrap().items[0]
                .home_task
                .as_ref()
                .unwrap()
                .id,
            old
        );
    }
    #[test]
    fn request_and_log_numbered_pages_are_complete_filtered_and_stable() {
        let s = Store::open_in_memory().unwrap();
        {
            let mut conn = s.conn();
            let tx = conn.transaction().unwrap();
            for n in 0..2105 {
                let event = RequestEvent {
                    id: format!("fake-{n:04}"),
                    category: "body".into(),
                    purpose: "body".into(),
                    owner_id: 1,
                    owner_kind: "task".into(),
                    biz: "FAKE".into(),
                    started_at: 1000. + n as f64,
                    finished_at: Some(2000.),
                    outcome: "ok".into(),
                    policy_version: 1,
                    gap_ms: None,
                    detail: json!({}),
                };
                tx.execute(
                    "INSERT INTO wb_requests VALUES(?,?)",
                    rusqlite::params![event.id, serde_json::to_string(&event).unwrap()],
                )
                .unwrap();
            }
            tx.execute("INSERT INTO native_call_log(job_id,request_id,list_call_id,ts,cgi,action,dll_sha256,gate_min_ms,gate_max_ms,policy_version) VALUES(1,'fake-native',1,4000,5814,0,'fake',0,0,1)",[]).unwrap();
            tx.commit().unwrap();
        }
        let page = s.wb_request_page("body", 0., 10, 2100).unwrap();
        assert_eq!((page.total, page.items.len()), (2105, 5));
        assert_eq!(page.items.last().unwrap().id, "fake-0000");
        let native = s.wb_request_page("list", 0., 10, 0).unwrap();
        assert_eq!(native.total, 1);
        assert_eq!(native.items[0].owner_kind, "native");
        assert_eq!(s.wb_request_page("", 0., 10, 0).unwrap().total, 2106);
        for n in 0..25 {
            s.system_log("history", "info", &format!("分页记录{n}"), json!({}))
                .unwrap();
        }
        let filter = crate::store::system_logs::LogFilter {
            module: Some("history".into()),
            ..Default::default()
        };
        let last = s.system_logs_offset(&filter, 20, 10).unwrap();
        assert_eq!((last.total, last.rows.len()), (25, 5));
        assert!(s.system_logs_offset(&filter, -1, 10).is_err());
        let first = s.system_logs(&filter, None, 10).unwrap();
        let next = s.system_logs(&filter, first.next_before_id, 10).unwrap();
        assert!(first.rows.last().unwrap().id > next.rows[0].id);
    }
    #[test]
    fn removed_zip_jobs_cannot_start_or_resume_and_downloads_remain_available() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_account("NO_ZIP", Some("虚构公众号"), None)
            .unwrap();
        for mid in ["one", "two"] {
            s.upsert_article("NO_ZIP", mid, 1, &crate::ArticleFields::default())
                .unwrap();
        }
        let aid = s.list_articles(None, 10, 0, false).unwrap()[0].id;
        assert!(s
            .wb_archive_create(vec![aid])
            .unwrap_err()
            .to_string()
            .contains("已移除"));
        let download = s.wb_download_enqueue(vec![aid]).unwrap().task_id.unwrap();
        let mut legacy = s.wb_task(download).unwrap();
        legacy.id = download + 1;
        legacy.target = json!({"article_ids":[aid]});
        legacy.progress.session_recovery = Some(crate::workbench::SessionRecovery {
            discovery_id: 1,
            requested_at: 1000.,
            previous_session_id: "fake-session".into(),
            phase: "waiting_home".into(),
        });
        s.conn()
            .execute(
                "INSERT INTO wb_tasks VALUES(?,?)",
                rusqlite::params![legacy.id, serde_json::to_string(&legacy).unwrap()],
            )
            .unwrap();
        assert_eq!(s.wb_task_page("archive", 10, 0).unwrap().total, 1);
        assert!(s.wb_task_state(legacy.id, "queued", None).is_err());
        assert_eq!(
            s.wb_runnable_tasks(true, crate::model::now())
                .unwrap()
                .len(),
            1
        );
        assert_eq!(s.wb_download_queue().unwrap().total, 1);
        s.wb_restart(crate::model::now()).unwrap();
        assert_eq!(s.wb_task(legacy.id).unwrap().state, "cancelled");
        assert_eq!(s.count_articles(None).unwrap(), 2);
    }
    #[test]
    fn source_body_count_matches_export_requirements() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_account("BODY_COUNT", Some("正文状态样本"), None)
            .unwrap();
        s.upsert_account("EMPTY_SOURCE", Some("空公众号"), None)
            .unwrap();
        for (mid, done, md) in [
            ("empty", 1, " \n\t"),
            ("body", 0, "已下载正文"),
            ("deleted", 1, "删除的正文"),
        ] {
            s.upsert_article(
                "BODY_COUNT",
                mid,
                1,
                &crate::ArticleFields {
                    detail_done: Some(done),
                    content_md: Some(md.into()),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        s.conn()
            .execute("UPDATE articles SET is_deleted=1 WHERE mid='deleted'", [])
            .unwrap();
        let page = s.wb_source_page("BODY_COUNT", 10, 0).unwrap();
        assert_eq!(
            (page.items[0].source.article_count, page.items[0].body_count),
            (2, 1)
        );
        assert_eq!(
            s.wb_source_page("EMPTY_SOURCE", 10, 0).unwrap().items[0].body_count,
            0
        );
        s.conn()
            .execute(
                "UPDATE articles SET content_md='新正文' WHERE mid='empty'",
                [],
            )
            .unwrap();
        let page = s.wb_source_page("BODY_COUNT", 10, 0).unwrap();
        assert_eq!(
            (page.items[0].source.article_count, page.items[0].body_count),
            (2, 2)
        );
        // 完整计数不依赖默认每页十条或前端快照中的文章数量。
        for n in 0..1010 {
            s.upsert_article(
                "BODY_COUNT",
                &format!("extra-{n}"),
                1,
                &crate::ArticleFields {
                    content_md: Some("正文".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let page = s.wb_source_page("BODY_COUNT", 10, 0).unwrap();
        assert_eq!(
            (page.items[0].source.article_count, page.items[0].body_count),
            (1012, 1012)
        );
    }
    #[test]
    fn active_queues_and_body_flags_use_actual_data_beyond_snapshot_limits() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_account("ACTIVE", Some("虚构"), None).unwrap();
        let mut ids = Vec::new();
        for (mid, done, md) in [("not-downloaded", 1, " \n\t"), ("downloaded", 0, "正文")] {
            ids.push(
                s.upsert_article(
                    "ACTIVE",
                    mid,
                    1,
                    &crate::ArticleFields {
                        detail_done: Some(done),
                        content_md: Some(md.into()),
                        ..Default::default()
                    },
                )
                .unwrap(),
            );
        }
        let rows = s.list_articles(Some("ACTIVE"), 10, 0, false).unwrap();
        assert!(!rows.iter().find(|r| r.id == ids[0]).unwrap().has_body);
        assert!(rows.iter().find(|r| r.id == ids[1]).unwrap().has_body);
        let states = s.wb_article_body_states(&[ids[0], ids[1], 99999]).unwrap();
        assert_eq!(
            (states[&ids[0]], states[&ids[1]], states[&99999]),
            (false, true, false)
        );
        let download = s
            .wb_download_enqueue(vec![ids[0]])
            .unwrap()
            .task_id
            .unwrap();
        let template = s.wb_task(download).unwrap();
        {
            let mut conn = s.conn();
            let tx = conn.transaction().unwrap();
            for id in 2..=1005 {
                let mut task = template.clone();
                task.id = id;
                task.state = "done".into();
                tx.execute(
                    "INSERT INTO wb_tasks VALUES(?,?)",
                    params![id, serde_json::to_string(&task).unwrap()],
                )
                .unwrap();
            }
            for (id, target, completed, state) in [
                (1006, json!({"count":100}), 25, "paused"),
                (1007, json!({"cutoff_at":1000}), 3, "needs_action"),
            ] {
                let mut task = template.clone();
                task.id = id;
                task.kind = "history".into();
                task.target = target;
                task.progress.completed = completed;
                task.state = state.into();
                tx.execute(
                    "INSERT INTO wb_tasks VALUES(?,?)",
                    params![id, serde_json::to_string(&task).unwrap()],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }
        assert!(!s.wb_tasks().unwrap().iter().any(|t| t.id == download));
        let q = s.wb_active_queues().unwrap();
        assert_eq!(
            (
                q.download.task_count,
                q.download.completed,
                q.download.total
            ),
            (1, 0, Some(1))
        );
        assert_eq!(
            (q.history.task_count, q.history.completed, q.history.total),
            (2, 28, None)
        );
        s.wb_task_state(1007, "cancelled", None).unwrap();
        let q = s.wb_active_queues().unwrap();
        assert_eq!((q.history.completed, q.history.total), (25, Some(100)));
        s.wb_task_state(1006, "failed", None).unwrap();
        s.wb_task_state(download, "done", None).unwrap();
        let q = s.wb_active_queues().unwrap();
        assert_eq!((q.download.task_count, q.history.task_count), (0, 0));
    }
}
