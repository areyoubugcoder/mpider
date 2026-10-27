//! 独立请求调度：首页同步、历史任务与实验共享列表闸门，正文使用独立队列。
use super::*;
use crate::{
    model::now,
    native::{protocol, runtime},
    Store,
};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::io::AsyncWriteExt;

/// 桌面与命令行调度器共享进程锁；数据库仍允许只读状态查询和事务入队。
pub fn instance_lock(db: &Path) -> Result<std::fs::File> {
    if let Some(parent) = db.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(db.with_extension("runtime.lock"))?;
    file.try_lock()
        .context("此数据目录已有运行中的采集调度器，请使用已打开的应用")?;
    Ok(file)
}

/// 旧版本只有原始检查点时，先保护封装；该操作不发送任何网络请求。
pub(crate) async fn seal_legacy_cursor(
    directory: &Path,
    python: &str,
    native_job: i64,
    offset: &str,
    observed: f64,
) -> Result<String> {
    let path = directory.join("sessions").join(format!(
        "legacy-{native_job}-{}.dat",
        (now() * 1000000.) as u64
    ));
    std::fs::create_dir_all(path.parent().context("保护目录无效")?)?;
    let mut command = tokio::process::Command::new(runtime::python_executable(python));
    command
        .arg(directory.join("session_probe.py"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command.spawn()?;
    let mut input = child.stdin.take().context("保护通道不可用")?;
    let value = json!({"mode":"seal_legacy_cursor","native_job_id":native_job,"offset":offset,"observed_at":observed,"evidence_file":path});
    input.write_all(format!("{value}\n").as_bytes()).await?;
    drop(input);
    let result = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output()).await??;
    let value: Value = serde_json::from_slice(&result.stdout)?;
    anyhow::ensure!(
        value["outcome"] == "cursor_sealed"
            && value["hash"] == format!("{:x}", Sha256::digest(offset.as_bytes())),
        "无法保护旧检查点，原始记录已保留"
    );
    Ok(path.display().to_string())
}

#[derive(Clone, Debug)]
pub struct Reply {
    pub request_id: Option<String>,
    pub sent_at: f64,
    pub outcome: String,
    pub payload: Value,
    pub evidence: Option<String>,
    pub next_at: f64,
}
impl Reply {
    pub fn page_for_cursor(
        &self,
        source: &Source,
        cursor: &CursorRecord,
    ) -> Result<protocol::NativePage> {
        let page = self.page(source)?;
        anyhow::ensure!(
            page.is_end || format!("{:x}", Sha256::digest(page.cursor.as_bytes())) != cursor.hash,
            "游标响应没有前进"
        );
        Ok(page)
    }
    pub fn page(&self, source: &Source) -> Result<protocol::NativePage> {
        anyhow::ensure!(self.outcome == "response", "请求未返回业务载荷");
        protocol::parse_page(&self.payload, &source.user_name, Some(&source.biz))
    }
    pub fn classification(&self) -> String {
        if self.outcome != "response" {
            return self.outcome.clone();
        }
        let codes = protocol::response_codes(&self.payload);
        if codes.rate_signal {
            "rate_signal"
        } else if codes.session_timeout {
            "session_timeout"
        } else if codes.inner != Some(0) {
            "business_error"
        } else {
            "schema_or_identity_error"
        }
        .into()
    }
}

/// 逐次申请许可；返回 deferred 时没有发送请求，也没有消耗预算。
pub async fn request_once(
    store: &Store,
    root: &Path,
    session: &Session,
    cursor: Option<&CursorRecord>,
    owner: i64,
    purpose: &str,
) -> Result<Reply> {
    let _guard = match list_lock().try_lock() {
        Ok(g) => g,
        Err(_) => {
            return Ok(Reply {
                request_id: None,
                sent_at: now(),
                outcome: "deferred".into(),
                payload: json!({"reason":"微信取样占用列表通道"}),
                evidence: None,
                next_at: now() + 0.5,
            })
        }
    };
    let permit = store.wb_request_permit("list", purpose, owner, &session.biz, now())?;
    let experiment_phase = purpose.strip_prefix("cursor_").map(|role| match role {
        "control_home" => "control_home",
        "control_page" => "control_page",
        "recheck" => "recheck",
        _ => "requesting",
    });
    let Some(id) = permit.request_id else {
        if let Some(phase) = experiment_phase {
            store.wb_experiment_phase(
                owner,
                if phase == "requesting" {
                    "gate_wait"
                } else {
                    phase
                },
                Some(permit.next_at),
            )?;
        }
        return Ok(Reply {
            request_id: None,
            sent_at: now(),
            outcome: "deferred".into(),
            payload: json!({"reason":permit.reason}),
            evidence: None,
            next_at: permit.next_at,
        });
    };
    if let Some(phase) = experiment_phase {
        store.wb_experiment_phase(owner, phase, None)?;
    }
    let sent_at = now();
    let mut context = json!({"request_id":id,"biz":session.biz,"cursor_id":cursor.map(|c|c.id),"cursor_age_seconds":cursor.map(|c|(sent_at-c.observed_at).max(0.)),"purpose":purpose});
    context[if purpose.starts_with("cursor_") {
        "experiment_id"
    } else {
        "task_id"
    }] = json!(owner);
    store.system_log(
        if purpose.starts_with("cursor_") {
            "experiment"
        } else {
            "history"
        },
        "info",
        "独立列表请求使用已保存游标与会话",
        context,
    )?;
    let result=async {
        let runtime_dir=root.join("probe-runtime");runtime::materialize(&runtime_dir,&session.config,None)?;
        let evidence_dir=root.join("private/pages");std::fs::create_dir_all(&evidence_dir)?;
        let evidence=evidence_dir.join(format!("{id}.dat"));
        let mut command=tokio::process::Command::new(runtime::python_executable(&session.config.python_path));
        command.arg(runtime_dir.join("session_probe.py")).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
        #[cfg(windows)] command.creation_flags(0x08000000);
        let mut child=command.spawn().context("无法启动独立请求环境")?;
        let mut input=child.stdin.take().context("请求通道不可用")?;
        let config=json!({"session_file":session.path,"cursor_file":cursor.map(|c|&c.evidence_path),"evidence_file":evidence,"timeout":session.config.response_timeout_seconds});
        input.write_all(format!("{}\n",config).as_bytes()).await?;drop(input);
        let output=tokio::time::timeout(Duration::from_secs(session.config.response_timeout_seconds+10),child.wait_with_output()).await.context("独立请求工作进程超时")??;
        anyhow::ensure!(output.stdout.len()<=16*1024*1024,"响应超出工作台上限");
        let value:Value=serde_json::from_slice(&output.stdout).context("独立请求返回格式异常")?;
        if value["outcome"]=="response" {anyhow::ensure!(value["request_cursor_hash"].as_str()==cursor.map(|c|c.hash.as_str()),"请求游标与选定证据不一致");}
        Ok::<_,anyhow::Error>(Reply{request_id:Some(id.clone()),sent_at,outcome:value["outcome"].as_str().unwrap_or("worker_error").into(),payload:value.get("payload").cloned().unwrap_or_else(||value.clone()),evidence:evidence.is_file().then(||evidence.display().to_string()),next_at:0.})
    }.await;
    let reply = result.unwrap_or_else(|_| Reply {
        request_id: Some(id.clone()),
        sent_at,
        outcome: "worker_error".into(),
        payload: json!({}),
        evidence: None,
        next_at: 0.,
    });
    let codes = protocol::response_codes(&reply.payload);
    let outcome = if reply.outcome == "response" && codes.inner == Some(0) {
        "ok".into()
    } else {
        reply.classification()
    };
    store.wb_request_finish(&id,&outcome,json!({"business_ret":codes.inner,"outer_ret":codes.outer,"session_id":session.id,"session_created_at":session.created_at,"cursor_id":cursor.map(|c|c.id),"evidence_path":reply.evidence,"http_status":reply.payload.get("http_status")}),now())?;
    if codes.rate_signal || reply.payload["http_status"] == 429 {
        store.wb_cooldown(now() + store.wb_policy()?.cooldown_seconds as f64)?;
    }
    Ok(reply)
}

fn temporary_cursor(page: &protocol::NativePage, session: &Session, reply: &Reply) -> CursorRecord {
    CursorRecord {
        id: 0,
        biz: session.biz.clone(),
        session_id: session.id.clone(),
        parent_id: None,
        source_page: Some(1),
        hash: format!("{:x}", Sha256::digest(page.cursor.as_bytes())),
        evidence_path: reply.evidence.clone().unwrap_or_default(),
        observed_at: reply.sent_at,
        last_success_at: None,
        state: "unverified".into(),
    }
}
fn interrupted(store: &Store, owner: i64, experiment: bool) -> bool {
    if experiment {
        store
            .wb_experiment(owner)
            .map_or(true, |e| e.state != "running")
    } else {
        store
            .wb_task(owner)
            .map_or(true, |t| !["queued", "running"].contains(&t.state.as_str()))
    }
}
#[allow(clippy::too_many_arguments)] // 显式固定实验对象和截止时间，避免借用生产检查点。
async fn wait_request(
    store: &Store,
    root: &Path,
    session: &Session,
    cursor: Option<&CursorRecord>,
    owner: i64,
    purpose: &str,
    experiment: bool,
    deadline: f64,
) -> Result<Reply> {
    loop {
        if interrupted(store, owner, experiment) || now() > deadline {
            return Ok(Reply {
                request_id: None,
                sent_at: now(),
                outcome: "interrupted".into(),
                payload: json!({}),
                evidence: None,
                next_at: 0.,
            });
        }
        if !experiment
            && store
                .wb_experiments()?
                .iter()
                .any(|e| e.state == "running" && experiment_due(e).is_some_and(|d| d <= now()))
        {
            return Ok(Reply {
                request_id: None,
                sent_at: now(),
                outcome: "deferred".into(),
                payload: json!({"reason":"让出到期实验"}),
                evidence: None,
                next_at: 0.,
            });
        }
        let reply = request_once(store, root, session, cursor, owner, purpose).await?;
        if reply.outcome != "deferred" {
            return Ok(reply);
        }
        if reply.next_at > deadline {
            return Ok(reply);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn record_probe(
    store: &Store,
    e: &Experiment,
    c: &CursorRecord,
    role: &str,
    due: f64,
    reply: &Reply,
    source: &Source,
) -> Result<()> {
    let checked = if role == "control_home" {
        reply.page(source)
    } else {
        reply.page_for_cursor(source, c)
    };
    let outcome = if checked.is_ok() {
        "valid".into()
    } else {
        reply.classification()
    };
    let detail = json!({"business_ret":protocol::response_codes(&reply.payload).inner,"session_id":e.session_id,"received_articles":checked.as_ref().ok().map(|p|p.articles.len()),"is_end":checked.as_ref().ok().map(|p|p.is_end),"http_status":reply.payload.get("http_status")});
    store.wb_probe_add(Probe {
        id: 0,
        experiment_id: e.id,
        request_id: reply.request_id.clone(),
        role: role.into(),
        scheduled_at: due,
        actual_at: reply.sent_at,
        cursor_age_seconds: (reply.sent_at - c.observed_at).max(0.),
        outcome,
        evidence_path: reply.evidence.clone(),
        detail,
    })?;
    Ok(())
}

/// 不猜错误码：同会话首页、新游标分页都成功且原游标复测失败，才确认不可复用。
#[allow(clippy::too_many_arguments)]
async fn confirm_unusable(
    store: &Store,
    root: &Path,
    session: &Session,
    cursor: &CursorRecord,
    source: &Source,
    owner: i64,
    experiment: Option<&Experiment>,
    deadline: f64,
) -> Result<bool> {
    let is_exp = experiment.is_some();
    let home = wait_request(
        store,
        root,
        session,
        None,
        owner,
        if is_exp {
            "cursor_control_home"
        } else {
            "history_control_home"
        },
        is_exp,
        deadline,
    )
    .await?;
    if let Some(e) = experiment {
        record_probe(store, e, cursor, "control_home", now(), &home, source)?;
    }
    let Ok(home_page) = home.page(source) else {
        return Ok(false);
    };
    if home_page.is_end {
        return Ok(false);
    }
    let fresh = temporary_cursor(&home_page, session, &home);
    let control = wait_request(
        store,
        root,
        session,
        Some(&fresh),
        owner,
        if is_exp {
            "cursor_control_page"
        } else {
            "history_control_page"
        },
        is_exp,
        deadline,
    )
    .await?;
    if let Some(e) = experiment {
        record_probe(store, e, &fresh, "control_page", now(), &control, source)?;
    }
    if control.page_for_cursor(source, &fresh).is_err() {
        return Ok(false);
    }
    let retry = wait_request(
        store,
        root,
        session,
        Some(cursor),
        owner,
        if is_exp {
            "cursor_recheck"
        } else {
            "history_recheck"
        },
        is_exp,
        deadline,
    )
    .await?;
    if let Some(e) = experiment {
        record_probe(store, e, cursor, "recheck", now(), &retry, source)?;
    }
    if retry.page_for_cursor(source, cursor).is_ok() {
        store.wb_cursor_result(cursor.id, "valid", retry.sent_at)?;
    }
    // 传输/认证失败不能证明游标被拒绝。
    Ok(retry.outcome == "response" && retry.classification() == "business_error")
}

pub(crate) fn record_task_finish(store: &Store, task: &Task) {
    let summary = if ["history", "home"].contains(&task.kind.as_str()) {
        format!(
            "已获取 {} 页，返回篇数 {}，新入库 {} 篇",
            task.progress.pages,
            task.progress
                .received_articles
                .map(|n| n.to_string())
                .unwrap_or_else(|| "未记录".into()),
            task.progress.new_articles
        )
    } else {
        format!(
            "已处理 {} 篇，跳过 {} 篇，缺图 {} 篇，失败 {} 篇",
            task.progress.completed,
            task.progress.skipped,
            task.progress.partial,
            task.progress.failed
        )
    };
    let calls = store
        .wb_requests(task.created_at)
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.owner_kind == "task" && e.owner_id == task.id)
        .collect::<Vec<_>>();
    let _ = store.append_run_log(&crate::model::RunLogEvent {
        kind: if task.kind == "archive" {
            "archive"
        } else if task.kind == "home" {
            "home_sync"
        } else {
            "history_native"
        }
        .into(),
        started_at: task.created_at,
        finished_at: now(),
        accounts: i64::from(task.biz.is_some()),
        ok: i64::from(task.state == "done"),
        failed: task.progress.failed as i64,
        new_articles: task.progress.new_articles as i64,
        urls: task.progress.completed as i64,
        list_calls: calls.iter().filter(|e| e.category == "list").count() as i64,
        rate_limited: calls
            .iter()
            .any(|e| e.outcome == "rate_signal" || e.outcome == "rate_limited"),
        note: Some(format!(
            "工作台任务 #{}：{}；{}",
            task.id, task.state, summary
        )),
        ..Default::default()
    });
    crate::applog::info(
        if ["history", "home"].contains(&task.kind.as_str()) {
            crate::applog::Stage::History
        } else {
            crate::applog::Stage::Archive
        },
        format!("工作台任务 #{} 已结束：{}", task.id, summary),
    );
}
fn start_task_recovery(
    store: &Arc<Store>,
    root: &Path,
    task: &Task,
    python: &str,
    before: Option<&Discovery>,
) -> Result<()> {
    let closed_during_request = store.wb_discovery()?.is_some_and(|run| {
        matches!(run.state.as_str(), "stopping" | "stopped")
            && before.is_some_and(|old| {
                old.id == run.id && matches!(old.state.as_str(), "starting" | "running")
            })
    });
    if task.progress.session_recovery.is_none() && !closed_during_request {
        if let Ok((Some(id), true)) = store.wb_recover_session(task.id, now()) {
            tokio::spawn(super::discovery::run(
                store.clone(),
                root.to_path_buf(),
                id,
                python.into(),
            ));
        }
    }
    Ok(())
}

async fn home_tick(store: &Arc<Store>, root: &Path, mut task: Task) -> Result<()> {
    task.state = "running".into();
    store.wb_save_task(&task)?;
    if store.wb_task(task.id)?.state != "running" {
        return Ok(());
    }
    let source = store
        .wb_source(task.biz.as_deref().context("任务没有公众号")?)?
        .context("公众号不存在")?;
    let sync_before = store.wb_discovery()?;
    if source.session_id.is_empty() {
        store.wb_task_state(
            task.id,
            "needs_action",
            Some("等待会话，请开启监测并在微信打开该公众号主页"),
        )?;
        start_task_recovery(store, root, &task, "python", sync_before.as_ref())?;
        return Ok(());
    }
    let session = store.wb_session(&source.session_id)?;
    let reply = request_once(store, root, &session, None, task.id, "home_sync").await?;
    if reply.outcome == "deferred" {
        task.next_action_at = reply.next_at;
        task.reason = reply.payload["reason"].as_str().map(str::to_owned);
        store.wb_save_task(&task)?;
        return Ok(());
    }
    match reply.page(&source) {
        Ok(page) => {
            store.wb_home_commit(
                task.id,
                &page,
                &session.id,
                reply.evidence.as_deref().context("缺少首页证据")?,
                now(),
            )?;
            let result = store.wb_task(task.id)?;
            if result.state == "done" {
                record_task_finish(store, &result);
            }
        }
        Err(_) if !interrupted(store, task.id, false) => {
            let kind = reply.classification();
            if ["authentication_failed", "session_timeout"].contains(&kind.as_str()) {
                store.wb_task_block_session(task.id, &session.id, &kind)?;
                start_task_recovery(
                    store,
                    root,
                    &task,
                    &session.config.python_path,
                    sync_before.as_ref(),
                )?;
            } else {
                if ["network_or_storage_error", "timeout", "http_error"].contains(&kind.as_str()) {
                    store.wb_cooldown(now() + store.wb_policy()?.cooldown_seconds as f64)?;
                }
                store.wb_task_state(
                    task.id,
                    "paused",
                    Some(&format!("首页同步未完成：{kind}；历史进度保留")),
                )?;
            }
        }
        Err(_) => {}
    }
    Ok(())
}

async fn history_tick(store: &Arc<Store>, root: &Path, task: Task) -> Result<()> {
    let mut task = store.wb_consume(task.id)?;
    if task.state != "running" {
        if task.state == "done" {
            record_task_finish(store, &task);
        }
        return Ok(());
    }
    let source = store
        .wb_source(task.biz.as_deref().context("任务没有公众号")?)?
        .context("公众号不存在")?;
    if source.session_id.is_empty() {
        store.wb_task_state(
            task.id,
            "needs_action",
            Some("等待会话，请在监测就绪后打开该公众号主页"),
        )?;
        start_task_recovery(store, root, &task, "python", None)?;
        return Ok(());
    }
    let sync_before = store.wb_discovery()?;
    let session = store.wb_session(&source.session_id)?;
    let target: HistoryTarget = serde_json::from_value(task.target.clone())?;
    let cursor_id = if target.pages.is_some() {
        target.cursor_id
    } else if task.progress.rebuilding {
        task.progress.rebuild_cursor
    } else {
        source.history_cursor_id
    };
    if cursor_id.is_none() && !task.progress.rebuilding {
        store.wb_task_state(
            task.id,
            "needs_action",
            Some("没有可恢复的历史游标，请重新识别公众号"),
        )?;
        return Ok(());
    }
    let cursor = cursor_id.map(|id| store.wb_cursor(id)).transpose()?;
    let reply = request_once(
        store,
        root,
        &session,
        cursor.as_ref(),
        task.id,
        if task.progress.rebuilding {
            "history_rebuild"
        } else {
            "history"
        },
    )
    .await?;
    if reply.outcome == "deferred" {
        task.next_action_at = reply.next_at;
        task.reason = reply.payload["reason"].as_str().map(str::to_string);
        store.wb_save_task(&task)?;
        return Ok(());
    }
    match reply.page(&source) {
        Ok(page) => {
            if !page.is_end
                && cursor.as_ref().is_some_and(|c| {
                    c.hash == format!("{:x}", Sha256::digest(page.cursor.as_bytes()))
                })
            {
                store.wb_task_state(
                    task.id,
                    "needs_action",
                    Some("响应游标未前进，请核对协议或页面"),
                )?;
                return Ok(());
            }
            if let Some(c) = &cursor {
                store.wb_cursor_result(c.id, "valid", now())?;
            }
            store.wb_history_commit(
                task.id,
                &page,
                &session.id,
                reply.evidence.as_deref().context("缺少页面证据")?,
                now(),
            )?;
        }
        Err(_) => {
            let kind = reply.classification();
            store.system_log("history","warn","未获取到有效页面；当前游标和获取时间保持不变",json!({"task_id":task.id,"biz":source.biz,"request_id":reply.request_id,"cursor_id":cursor.as_ref().map(|c|c.id),"cursor_age_seconds":cursor.as_ref().map(|c|(reply.sent_at-c.observed_at).max(0.)),"outcome":kind,"age_reset":false}))?;
            if kind == "business_error" && !task.progress.rebuilding && target.pages.is_none() {
                if let Some(c) = &cursor {
                    if confirm_unusable(
                        store,
                        root,
                        &session,
                        c,
                        &source,
                        task.id,
                        None,
                        now() + 86400.,
                    )
                    .await?
                    {
                        store.wb_cursor_result(c.id, "unusable", now())?;
                        match store.wb_begin_rebuild(task.id) {
                            Ok(()) => return Ok(()),
                            Err(error) => {
                                store.wb_task_state(
                                    task.id,
                                    "needs_action",
                                    Some(&format!("游标已确认不可复用；{}", error)),
                                )?;
                                return Ok(());
                            }
                        }
                    }
                }
            }
            if ["network_or_storage_error", "timeout", "http_error"].contains(&kind.as_str()) {
                store.wb_cooldown(now() + store.wb_policy()?.cooldown_seconds as f64)?;
            }
            if !interrupted(store, task.id, false) {
                if ["authentication_failed", "session_timeout"].contains(&kind.as_str()) {
                    store.wb_task_block_session(task.id, &session.id, &kind)?;
                    start_task_recovery(
                        store,
                        root,
                        &task,
                        &session.config.python_path,
                        sync_before.as_ref(),
                    )?;
                    return Ok(());
                }
                store.wb_task_state(
                    task.id,
                    "paused",
                    Some(&format!("请求未完成：{}；游标与历史进度已保留", kind)),
                )?;
            }
        }
    }
    Ok(())
}

pub fn experiment_due(e: &Experiment) -> Option<f64> {
    e.schedule_hours
        .get(e.next_index)
        .map(|h| e.started_at + h * 3600.)
}

/// 重启/休眠错过的点记录缺测；只执行当前最近的到期点，不突发补发。
pub fn mark_missed(store: &Store, e: &mut Experiment, c: &CursorRecord, ts: f64) -> Result<()> {
    while let Some(due) = experiment_due(e) {
        if due > ts || (ts <= e.deadline_at + 1. && ts <= due + 300.) {
            break;
        }
        store.wb_probe_add(Probe {
            id: 0,
            experiment_id: e.id,
            request_id: None,
            role: "scheduled".into(),
            scheduled_at: due,
            actual_at: ts,
            cursor_age_seconds: (ts - c.observed_at).max(0.),
            outcome: "missed".into(),
            evidence_path: None,
            detail: json!({"reason":"应用离线、休眠、预算或冷却导致错过时点"}),
        })?;
        e.next_index += 1;
    }
    store.wb_save_experiment(e)
}

pub fn write_report(store: &Store, root: &Path, e: &mut Experiment) -> Result<()> {
    let persisted = store.wb_experiment(e.id)?;
    if ["cancelled", "done", "failed"].contains(&persisted.state.as_str()) {
        *e = persisted;
    }
    let cursor = store.wb_cursor(e.cursor_id)?;
    let probes = store.wb_probes(e.id)?;
    let successes: Vec<_> = probes
        .iter()
        .filter(|p| p.outcome == "valid" && ["scheduled", "recheck"].contains(&p.role.as_str()))
        .collect();
    let last = successes
        .iter()
        .map(|p| p.cursor_age_seconds)
        .max_by(f64::total_cmp);
    if e.verdict.is_none() {
        let session_blocked = probes.iter().any(|p| {
            [
                "authentication_failed",
                "session_timeout",
                "protocol_or_session_error",
                "invalid_cursor_evidence",
            ]
            .contains(&p.outcome.as_str())
        });
        e.verdict = Some(
            if last.is_some() {
                "at_least_valid"
            } else if e.state == "done" && session_blocked {
                "session_unavailable"
            } else {
                "insufficient_evidence"
            }
            .into(),
        );
    }
    if e.state == "running" || e.conclusion.is_none() {
        let session_blocked = probes.iter().any(|p| {
            [
                "authentication_failed",
                "session_timeout",
                "protocol_or_session_error",
                "invalid_cursor_evidence",
            ]
            .contains(&p.outcome.as_str())
        });
        e.conclusion=Some(match last {Some(age)=>format!("在本实验的定时复用频率下，游标至少可用 {:.2} 小时（自首次观测计）。{}重复使用可能影响有效期；缺测点不作成功推断。",age/3600.,if session_blocked{"后续时点因会话或认证不可用停止，未能继续测量。"}else{""}),None=>"没有取得可判定的成功样本，无法推断游标有效期。".into()});
    }
    let dir = root.join("experiments").join(e.id.to_string());
    e.report_path = Some(dir.join("report.md").display().to_string());
    store.wb_save_experiment(e)?;
    *e = store.wb_experiment(e.id)?;
    std::fs::create_dir_all(&dir)?;
    let result = json!({"version":1,"experiment":e,"cursor":cursor,"probes":probes,"measurement":"同一游标定时复用；不等价于闲置有效期"});
    archive::atomic_write(
        &dir.join("results.json"),
        &serde_json::to_vec_pretty(&result)?,
    )?;
    let mut md=format!("# 游标有效期验证 #{}\n\n{}\n\n固定会话：`{}`\n\n|角色|计划时间（epoch）|实际时间（epoch）|游标年龄（小时）|结果|证据|\n|---|---:|---:|---:|---|---|\n",e.id,e.conclusion.as_deref().unwrap_or_default(),e.session_id);
    for p in probes {
        md.push_str(&format!(
            "|{}|{:.3}|{:.3}|{:.3}|{}|{}|\n",
            p.role,
            p.scheduled_at,
            p.actual_at,
            p.cursor_age_seconds / 3600.,
            p.outcome,
            p.evidence_path
                .as_deref()
                .map(|v| format!("[受保护响应](<{v}>)"))
                .unwrap_or_default()
        ));
    }
    md.push_str("\n原始游标与响应位于当前 Windows 用户的 DPAPI 文件；普通报告不包含会话密钥。24 小时是观察窗口，不是生产游标过期阈值。\n");
    archive::atomic_write(&dir.join("report.md"), md.as_bytes())?;
    store.wb_save_experiment(e)
}

async fn experiment_tick(store: &Store, root: &Path, mut e: Experiment) -> Result<()> {
    e.conclusion = None;
    let c = store.wb_cursor(e.cursor_id)?;
    mark_missed(store, &mut e, &c, now())?;
    if e.next_index > 0
        && store.wb_probes(e.id)?.iter().any(|p| {
            p.role == "scheduled"
                && (p.scheduled_at - e.started_at).abs() < 0.1
                && p.outcome == "missed"
        })
    {
        e.state = "done".into();
        e.verdict = Some("insufficient_evidence".into());
        e.conclusion =
            Some("即时基线时点缺测，未建立起始有效性；请在运行环境就绪后创建新实验。".into());
        return write_report(store, root, &mut e);
    }
    let Some(due) = experiment_due(&e) else {
        e.state = "done".into();
        return write_report(store, root, &mut e);
    };
    if now() > e.deadline_at + 1. {
        e.state = "done".into();
        return write_report(store, root, &mut e);
    }
    if now() < due {
        return Ok(());
    }
    let session = store.wb_session(&e.session_id)?;
    let source = store.wb_source(&c.biz)?.context("实验公众号不存在")?;
    let reply = request_once(store, root, &session, Some(&c), e.id, "cursor_experiment").await?;
    if reply.outcome == "deferred" {
        return Ok(());
    }
    record_probe(store, &e, &c, "scheduled", due, &reply, &source)?;
    e.next_index += 1;
    if reply.page_for_cursor(&source, &c).is_ok() {
        store.wb_cursor_result(c.id, "valid", reply.sent_at)?;
        e.verdict = Some("at_least_valid".into());
    } else {
        let kind = reply.classification();
        if kind == "business_error"
            && confirm_unusable(
                store,
                root,
                &session,
                &c,
                &source,
                e.id,
                Some(&e),
                e.deadline_at,
            )
            .await?
        {
            store.wb_cursor_result(c.id, "unusable", now())?;
            e.state = "done".into();
            e.verdict = Some("confirmed_unusable".into());
            let last = store
                .wb_probes(e.id)?
                .into_iter()
                .filter(|p| {
                    p.outcome == "valid" && ["scheduled", "recheck"].contains(&p.role.as_str())
                })
                .map(|p| p.cursor_age_seconds)
                .max_by(f64::total_cmp);
            e.conclusion=Some(format!("同一会话下，新游标对照成功、原游标重复失败，确认不可复用。最后成功年龄：{}；确认失败年龄：{:.2} 小时。该区间为观察结果，不能证明服务端精确 TTL。",last.map(|v|format!("{:.2} 小时",v/3600.)).unwrap_or_else(||"无成功样本".into()),(now()-c.observed_at)/3600.));
        } else if [
            "authentication_failed",
            "session_timeout",
            "protocol_or_session_error",
            "invalid_cursor_evidence",
        ]
        .contains(&kind.as_str())
        {
            e.state = "done".into();
            let last = store
                .wb_probes(e.id)?
                .into_iter()
                .filter(|p| {
                    p.outcome == "valid" && ["scheduled", "recheck"].contains(&p.role.as_str())
                })
                .map(|p| p.cursor_age_seconds)
                .max_by(f64::total_cmp);
            let reason = match kind.as_str() {
                "session_timeout" => "会话已过期（服务端返回 H5 Session Timeout）",
                "authentication_failed" => "会话认证失败",
                _ => "会话或协议不可用",
            };
            if let Some(age) = last {
                // 会话过期只截断观察窗口，不否定之前已证实的可用年龄。
                e.verdict = Some("at_least_valid".into());
                e.conclusion = Some(format!(
                    "在本实验的定时复用频率下，游标至少可用 {:.2} 小时（自首次观测计）。后续时点因{reason}停止，未能继续测量；如需更长观察请在新会话下创建新实验。",
                    age / 3600.
                ));
            } else {
                e.verdict = Some("session_unavailable".into());
                e.conclusion = Some(format!(
                    "会话、认证或证据不可用（{kind}），无法独立判定游标有效期；更换会话请创建新实验。"
                ));
            }
        }
    }
    if e.next_index == 1 && e.state == "running" && reply.page_for_cursor(&source, &c).is_err() {
        e.state = "done".into();
        e.verdict = Some("insufficient_evidence".into());
        e.conclusion = Some(format!(
            "即时基线未成功（{}），未建立起始有效性，无法测量有效期；请核对请求结果后创建新实验。",
            reply.classification()
        ));
    }
    if let Some(next) = experiment_due(&e) {
        if e.state == "running" {
            store.wb_experiment_phase(e.id, "scheduled_wait", Some(next))?;
        }
    } else {
        e.state = "done".into();
    }
    write_report(store, root, &mut e)
}

/// 应用常驻时运行；历史恢复由用户发起，实验按持久化时点自动恢复。
pub async fn run(store: Arc<Store>, root: PathBuf) {
    let archive_store = store.clone();
    let archive_root = root.clone();
    tokio::spawn(async move { archive::run(archive_store, archive_root).await });
    loop {
        let next = store
            .wb_experiments()
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e.state == "running" && experiment_due(e).is_none_or(|d| d <= now()))
            .min_by(|a, b| {
                experiment_due(a)
                    .unwrap_or(0.)
                    .total_cmp(&experiment_due(b).unwrap_or(0.))
            });
        let result = if let Some(exp) = next {
            experiment_tick(&store, &root, exp).await
        } else if let Some(task) = store
            .wb_runnable_tasks(false, now())
            .unwrap_or_default()
            .into_iter()
            .filter(|t| {
                ["history", "home"].contains(&t.kind.as_str())
                    && ["queued", "running"].contains(&t.state.as_str())
                    && t.next_action_at <= now()
            })
            .min_by(|a, b| a.updated_at.total_cmp(&b.updated_at))
        {
            let id = task.id;
            let result = if task.kind == "home" {
                home_tick(&store, &root, task).await
            } else {
                history_tick(&store, &root, task).await
            };
            if result.is_err() {
                let _ = store.wb_task_state(
                    id,
                    "needs_action",
                    Some("任务环境或持久化异常，请检查运行环境与存储后继续"),
                );
            }
            result
        } else {
            Ok(())
        };
        if result.is_err() {
            crate::applog::warn(
                crate::applog::Stage::List,
                "工作台执行未完成，已保留持久化状态，请检查任务与存储",
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
