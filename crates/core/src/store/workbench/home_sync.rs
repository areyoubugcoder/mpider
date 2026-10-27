//! 主页同步与会话恢复：在事务内绑定用户意图，只有正确运行的新请求可以续采。
use super::*;

const WAITING: [&str; 3] = ["starting", "waiting_home", "preparing_session"];

pub(super) fn ensure_running(tx: &Transaction<'_>, id: i64) -> Result<()> {
    let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM wb_discovery WHERE id=? AND id=(SELECT MAX(id) FROM wb_discovery) AND json_extract(document,'$.state')='running')", [id], |r| r.get(0))?;
    ensure!(valid, "主页同步已停止，忽略晚到会话");
    Ok(())
}
fn ensure_sync(tx: &Transaction<'_>) -> Result<(Discovery, bool)> {
    let existing: Option<Discovery> = tx
        .query_row(
            "SELECT document FROM wb_discovery ORDER BY id DESC LIMIT 1",
            [],
            |r| decode(r, 0),
        )
        .optional()?;
    if let Some(run) = existing {
        ensure!(run.state != "stopping", "主页同步正在停止，请稍后重试");
        if ["starting", "running"].contains(&run.state.as_str()) {
            return Ok((run, false));
        }
    }
    let native: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM native_jobs WHERE status IN ('running','paused'))",
        [],
        |r| r.get(0),
    )?;
    ensure!(!native, "请先结束原有首页识别任务");
    tx.execute("INSERT INTO wb_discovery(document) VALUES('{}')", [])?;
    let run = Discovery {
        id: tx.last_insert_rowid(),
        state: "starting".into(),
        target: None,
        scope_started_at: 0.,
        started_at: now(),
        updated_at: now(),
        reason: None,
        observed_requests: 0,
        accounts: vec![],
        phases: vec![],
    };
    put(tx, "wb_discovery", run.id, &run)?;
    Ok((run, true))
}
fn waiting_tasks(tx: &Transaction<'_>) -> Result<Vec<Task>> {
    let mut q = tx.prepare("SELECT document FROM wb_tasks WHERE json_extract(document,'$.state')='needs_action' AND json_extract(document,'$.progress.session_recovery.phase') IN ('starting','waiting_home','preparing_session')")?;
    let result = q
        .query_map([], |r| decode(r, 0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(result)
}
pub(super) fn sync_state_changed(tx: &Transaction<'_>, id: i64, state: &str) -> Result<()> {
    for mut task in waiting_tasks(tx)? {
        let r = task
            .progress
            .session_recovery
            .as_mut()
            .context("恢复意图不存在")?;
        if r.discovery_id != id {
            continue;
        }
        if ["stopping", "stopped", "failed"].contains(&state) {
            r.phase = "interrupted".into();
            task.reason = Some(
                "主页同步已停止，自动继续已取消；可点击更新会话并继续重新准备，原进度保留".into(),
            );
        } else if state == "running" && r.phase == "starting" {
            r.phase = "waiting_home".into();
            task.reason =
                Some("主页监测已就绪，请在微信打开目标公众号主页；收到新会话后自动继续".into());
        } else {
            continue;
        }
        task.updated_at = now();
        put(tx, "wb_tasks", task.id, &task)?;
    }
    Ok(())
}
pub(super) fn resume_for_capture(
    tx: &Transaction<'_>,
    run_id: i64,
    source: &Source,
    request_ts: f64,
    ts: f64,
) -> Result<()> {
    let created: f64 = tx.query_row(
        "SELECT created_at FROM wb_sessions WHERE id=?",
        [&source.session_id],
        |r| r.get(0),
    )?;
    for mut task in waiting_tasks(tx)? {
        let r = task
            .progress
            .session_recovery
            .as_mut()
            .context("恢复意图不存在")?;
        if task.biz.as_deref() != Some(&source.biz)
            || r.discovery_id != run_id
            || request_ts < r.requested_at
            || created < r.requested_at
            || r.previous_session_id == source.session_id
        {
            continue;
        }
        r.phase = "resumed".into();
        task.state = "queued".into();
        task.reason = Some("新会话已保存，正在继续原任务".into());
        task.updated_at = ts;
        task.next_action_at = now();
        put(tx, "wb_tasks", task.id, &task)?;
        super::super::system_logs::write(
            tx,
            "history",
            "info",
            "目标公众号新会话已保存，原任务自动继续",
            json!({"task_id":task.id,"biz":source.biz,"discovery_id":run_id}),
        )?;
    }
    Ok(())
}
impl Store {
    /// 同步服务幂等开启；显式应用采集范围时才开始新的识别轮次。
    pub fn wb_home_sync_start(
        &self,
        target: Option<HistoryTarget>,
        configure: bool,
    ) -> Result<(i64, bool)> {
        ensure!(
            target.is_none(),
            "监测不设置采集范围，请在公众号操作列创建任务"
        );
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let (mut run, created) = ensure_sync(&tx)?;
        if configure || run.target.is_some() {
            run.target = target;
            run.scope_started_at = now();
            run.accounts.clear();
            run.updated_at = now();
            put(&tx, "wb_discovery", run.id, &run)?;
        }
        tx.commit()?;
        Ok((run.id, created))
    }
    /// 注册一次恢复，不依赖页面是否挂载；重复点击不会重复启动或重新计时。
    pub fn wb_recover_session(&self, id: i64, ts: f64) -> Result<(Option<i64>, bool)> {
        ensure!(ts.is_finite() && ts > 0., "恢复时间无效");
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut task: Task =
            tx.query_row("SELECT document FROM wb_tasks WHERE id=?", [id], |r| {
                decode(r, 0)
            })?;
        ensure!(
            ["history", "home"].contains(&task.kind.as_str())
                && ["needs_action", "paused", "failed"].contains(&task.state.as_str()),
            "当前任务无需更新会话"
        );
        let source: Source = tx.query_row(
            "SELECT document FROM wb_sources WHERE biz=?",
            [&task.biz],
            |r| decode(r, 0),
        )?;
        let legacy = task.reason.as_deref().is_some_and(|s| {
            s.contains("H5 Session Timeout")
                || s.contains("authentication_failed")
                || s.contains("会话认证失败")
                || s.contains("等待") && s.contains("会话")
        });
        ensure!(
            task.progress.blocked_session_id.is_some()
                || source.session_id.is_empty()
                || legacy
                || task.progress.session_recovery.is_some(),
            "该任务未记录会话故障，请使用继续"
        );
        if task
            .progress
            .blocked_session_id
            .as_ref()
            .is_some_and(|s| !source.session_id.is_empty() && *s != source.session_id)
        {
            task.state = "queued".into();
            task.reason = Some("新会话已就绪，正在继续原任务".into());
            task.next_action_at = now();
            if let Some(r) = &mut task.progress.session_recovery {
                r.phase = "resumed".into();
            }
            put(&tx, "wb_tasks", id, &task)?;
            tx.commit()?;
            return Ok((None, false));
        }
        let (run, created) = ensure_sync(&tx)?;
        if let Some(r) = &task.progress.session_recovery {
            if r.discovery_id == run.id && WAITING.contains(&r.phase.as_str()) {
                return Ok((Some(run.id), false));
            }
        }
        task.progress.session_recovery = Some(SessionRecovery {
            discovery_id: run.id,
            requested_at: ts,
            previous_session_id: source.session_id,
            phase: if run.state == "running" {
                "waiting_home"
            } else {
                "starting"
            }
            .into(),
        });
        task.state = "needs_action".into();
        task.reason = Some(
            if run.state == "running" {
                "主页监测已就绪，请在微信打开目标公众号主页；收到新会话后自动继续"
            } else {
                "正在启动主页监测，监测就绪后请打开目标公众号主页"
            }
            .into(),
        );
        task.updated_at = now();
        task.next_action_at = now();
        put(&tx, "wb_tasks", id, &task)?;
        tx.commit()?;
        Ok((Some(run.id), created))
    }
    pub fn wb_recovery_home(&self, run_id: i64, biz: &str, request_ts: f64) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        ensure_running(&tx, run_id)?;
        for mut task in waiting_tasks(&tx)? {
            let r = task
                .progress
                .session_recovery
                .as_mut()
                .context("恢复意图不存在")?;
            if task.biz.as_deref() == Some(biz)
                && r.discovery_id == run_id
                && request_ts >= r.requested_at
            {
                r.phase = "preparing_session".into();
                task.reason = Some("已收到目标首页，正在准备新会话".into());
                put(&tx, "wb_tasks", task.id, &task)?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}
