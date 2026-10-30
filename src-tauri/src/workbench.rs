//! 工作台 IPC：任务、策略、游标实验和离线阅读；敏感材料不经过前端。
use super::*;
use mpider_core::workbench::{self as core, *};

fn root(state: &AppState) -> Result<PathBuf, String> {
    if state.db_path == ":memory:" {
        return Err("持久化存储不可用，请修复数据库后启动任务".into());
    }
    Ok(Path::new(&state.db_path)
        .parent()
        .ok_or("数据目录不可用")?
        .join("workbench"))
}
#[tauri::command]
pub fn workbench_push_status(
    state: State<'_, AppState>,
) -> Result<core::push_monitor::Status, String> {
    state.store.push_status().map_err(|e| e.to_string())
}
#[tauri::command]
pub fn workbench_push_start(
    state: State<'_, AppState>,
    config: core::push_monitor::Config,
) -> Result<i64, String> {
    root(&state)?;
    if !cfg!(windows) {
        return Err("推送监测仅支持 Windows，请在已登录微信的 Windows 电脑开启".into());
    }
    let id = state.store.push_start(config).map_err(|e| e.to_string())?;
    let store = state.store.clone();
    tauri::async_runtime::spawn(async move {
        core::push_monitor::run(store, id).await;
    });
    Ok(id)
}
#[tauri::command]
pub fn workbench_push_stop(state: State<'_, AppState>) -> Result<(), String> {
    let id = state.store.push_run().map_err(|e| e.to_string())?.id;
    state
        .store
        .push_end(id, "stopped", None)
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub fn workbench_snapshot(state: State<'_, AppState>) -> Result<Snapshot, String> {
    state.store.wb_snapshot().map_err(|e| e.to_string())
}
#[tauri::command]
pub fn workbench_article_body_states(
    state: State<'_, AppState>,
    article_ids: Vec<i64>,
) -> Result<std::collections::HashMap<i64, bool>, String> {
    state
        .store
        .wb_article_body_states(&article_ids)
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub fn workbench_source_page(
    state: State<'_, AppState>,
    search: String,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<WorkbenchPage<SourceListItem>, String> {
    state
        .store
        .wb_source_page(&search, limit.unwrap_or(10), offset.unwrap_or(0))
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub fn workbench_task_page(
    state: State<'_, AppState>,
    kind: String,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<WorkbenchPage<Task>, String> {
    state
        .store
        .wb_task_page(&kind, limit.unwrap_or(10), offset.unwrap_or(0))
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub fn workbench_request_page(
    state: State<'_, AppState>,
    category: String,
    since: f64,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<WorkbenchPage<RequestEvent>, String> {
    state
        .store
        .wb_request_page(&category, since, limit.unwrap_or(10), offset.unwrap_or(0))
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub fn workbench_policy(state: State<'_, AppState>, policy: Policy) -> Result<Policy, String> {
    state.store.wb_set_policy(policy).map_err(|e| e.to_string())
}
#[tauri::command]
pub fn workbench_history(
    state: State<'_, AppState>,
    bizzes: Vec<String>,
    target: HistoryTarget,
) -> Result<Vec<i64>, String> {
    root(&state)?;
    state
        .store
        .wb_history_create(bizzes, target)
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub fn workbench_sync_home(state: State<'_, AppState>, biz: String) -> Result<i64, String> {
    root(&state)?;
    state.store.wb_home_create(&biz).map_err(|e| e.to_string())
}
#[tauri::command]
pub fn workbench_source_counts(
    state: State<'_, AppState>,
) -> Result<std::collections::HashMap<String, i64>, String> {
    state.store.wb_source_counts().map_err(|e| e.to_string())
}
#[tauri::command]
pub fn workbench_archive_source(
    state: State<'_, AppState>,
    biz: String,
) -> Result<DownloadReceipt, String> {
    root(&state)?;
    state
        .store
        .wb_download_source(&biz)
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub fn workbench_download_articles(
    state: State<'_, AppState>,
    article_ids: Vec<i64>,
) -> Result<DownloadReceipt, String> {
    root(&state)?;
    state
        .store
        .wb_download_enqueue(article_ids)
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub async fn workbench_delete_source(
    state: State<'_, AppState>,
    biz: String,
) -> Result<SourceDeleteReceipt, String> {
    let directory = root(&state)?;
    let store = state.store.clone();
    tauri::async_runtime::spawn_blocking(move || store.wb_delete_source(&biz, &directory))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub fn workbench_delete_articles(
    state: State<'_, AppState>,
    article_ids: Vec<i64>,
) -> Result<usize, String> {
    state
        .store
        .wb_delete_articles(article_ids)
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub async fn workbench_export_source(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    biz: String,
    on_progress: tauri::ipc::Channel<core::markdown_export::ExportProgress>,
) -> Result<Option<core::markdown_export::MarkdownExport>, String> {
    pick_markdown_export(
        window,
        state.store.clone(),
        root(&state)?,
        Some(biz),
        Vec::new(),
        on_progress,
    )
    .await
}
#[tauri::command]
pub async fn workbench_export_articles(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    article_ids: Vec<i64>,
    on_progress: tauri::ipc::Channel<core::markdown_export::ExportProgress>,
) -> Result<Option<core::markdown_export::MarkdownExport>, String> {
    pick_markdown_export(
        window,
        state.store.clone(),
        root(&state)?,
        None,
        article_ids,
        on_progress,
    )
    .await
}
async fn pick_markdown_export(
    window: tauri::WebviewWindow,
    store: Arc<Store>,
    root: PathBuf,
    biz: Option<String>,
    ids: Vec<i64>,
    on_progress: tauri::ipc::Channel<core::markdown_export::ExportProgress>,
) -> Result<Option<core::markdown_export::MarkdownExport>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        core::markdown_export::validate_selection(&store, biz.as_deref(), &ids)
            .map_err(|e| e.to_string())?;
        let selected = rfd::FileDialog::new()
            .set_parent(&window)
            .set_title("选择文章导出文件夹（Markdown 和 HTML）")
            .pick_folder();
        let Some(path) = selected else {
            return Ok(None);
        };
        let report = |progress| {
            // 窗口关闭或进度接收失败不影响本地文件导出。
            let _ = on_progress.send(progress);
        };
        match biz {
            Some(biz) => core::markdown_export::export_source_with_progress(
                &store, &root, &biz, &path, &report,
            ),
            None => core::markdown_export::export_articles_with_progress(
                &store, &root, &ids, &path, &report,
            ),
        }
        .map(Some)
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|_| "导出任务执行失败".to_string())?
}
#[tauri::command]
pub fn workbench_next_page(state: State<'_, AppState>, biz: String) -> Result<i64, String> {
    root(&state)?;
    state
        .store
        .wb_history_create(
            vec![biz],
            HistoryTarget {
                pages: Some(1),
                ..Default::default()
            },
        )
        .map_err(|e| e.to_string())?
        .into_iter()
        .next()
        .ok_or("未创建下一页任务".into())
}
#[tauri::command]
pub fn workbench_task_action(
    state: State<'_, AppState>,
    id: i64,
    action: String,
) -> Result<(), String> {
    let status = match action.as_str() {
        "resume" => "queued",
        "pause" => "paused",
        "cancel" => "cancelled",
        _ => return Err("未知任务操作".into()),
    };
    state
        .store
        .wb_task_state(id, status, Some("用户操作"))
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub fn workbench_experiment(
    state: State<'_, AppState>,
    cursor_id: i64,
    schedule_hours: Vec<f64>,
    session_id: Option<String>,
) -> Result<i64, String> {
    root(&state)?;
    if !cfg!(windows) {
        return Err("请在保存会话的 Windows 专机运行实验".into());
    }
    state
        .store
        .wb_experiment_create_with_session(
            cursor_id,
            schedule_hours,
            session_id.as_deref(),
            mpider_core::model::now(),
        )
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub fn workbench_experiment_cancel(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    let mut e = state.store.wb_experiment(id).map_err(|e| e.to_string())?;
    if e.state != "running" {
        return Err("实验已结束".into());
    }
    e.state = "cancelled".into();
    e.conclusion = Some("用户终止实验；报告保留已取得的样本，不推断未测时间点。".into());
    core::engine::write_report(&state.store, &root(&state)?, &mut e).map_err(|e| e.to_string())
}
#[tauri::command]
pub fn workbench_probes(state: State<'_, AppState>, id: i64) -> Result<Vec<Probe>, String> {
    state.store.wb_probes(id).map_err(|e| e.to_string())
}
#[tauri::command]
pub fn workbench_report(state: State<'_, AppState>, id: i64) -> Result<String, String> {
    let mut experiment = state.store.wb_experiment(id).map_err(|e| e.to_string())?;
    core::engine::write_report(&state.store, &root(&state)?, &mut experiment)
        .map_err(|e| e.to_string())?;
    experiment.report_path.ok_or("报告文件尚未生成".into())
}
#[tauri::command]
pub async fn workbench_reader(
    state: State<'_, AppState>,
    article_id: i64,
) -> Result<String, String> {
    let store = state.store.clone();
    let directory = root(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        core::archive::reader_html(&store, &directory, article_id)
    })
    .await
    .map_err(|_| "无法读取已下载正文".to_string())?
    .map_err(|e| e.to_string())
}
#[tauri::command]
pub fn workbench_archive_status(
    state: State<'_, AppState>,
    article_id: i64,
) -> Result<Option<ArchiveRecord>, String> {
    state
        .store
        .wb_archive(article_id)
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub fn workbench_export_requests(state: State<'_, AppState>) -> Result<String, String> {
    let path = root(&state)?.join("exports").join(format!(
        "requests-{}.json",
        (mpider_core::model::now() * 1000.) as u64
    ));
    let events = state.store.wb_requests(0.).map_err(|e| e.to_string())?;
    core::archive::atomic_write(
        &path,
        &serde_json::to_vec_pretty(&events).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(path.display().to_string())
}
#[tauri::command]
pub fn workbench_capture(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    python_path: String,
    expected_biz: Option<String>,
) -> Result<(), String> {
    root(&state)?;
    native_preflight(&state)?;
    let policy = state.store.wb_policy().map_err(|e| e.to_string())?;
    let snapshot = state.store.wb_snapshot().map_err(|e| e.to_string())?;
    if snapshot.list_calls_24h >= policy.list_daily_budget {
        return Err("近 24 小时列表预算已用完，请在限流分析中查看或调整".into());
    }
    if policy.cooldown_until > mpider_core::model::now() {
        return Err("当前仍在全局冷却，请等待后重试".into());
    }
    let title = expected_biz
        .as_deref()
        .map(|biz| {
            state
                .store
                .wb_source(biz)
                .map(|s| s.map(|s| s.nickname).unwrap_or_default())
        })
        .transpose()
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let config = mpider_core::native::NativeConfig {
        enabled: true,
        transport: "session".into(),
        python_path,
        window_title: title,
        expected_biz,
        min_gap_seconds: policy.list_min_ms.div_ceil(1000),
        max_gap_seconds: policy.list_max_ms.div_ceil(1000),
        daily_budget: policy.list_daily_budget,
        max_pages: 1,
        ..Default::default()
    };
    native_start(app, state, config, RealRunConfig::default())
}

#[tauri::command]
pub fn workbench_discovery_start(
    state: State<'_, AppState>,
    python_path: String,
    target: Option<HistoryTarget>,
) -> Result<i64, String> {
    if target.is_some() {
        return Err("监测不设置采集范围，请在公众号操作列创建任务".into());
    }
    prepare_home_sync(&state)?;
    let directory = root(&state)?;
    let (id, created) = state
        .store
        .wb_home_sync_start(target, true)
        .map_err(|e| e.to_string())?;
    launch_home_sync(&state, directory, id, created, python_path);
    Ok(id)
}

fn prepare_home_sync(state: &AppState) -> Result<(), String> {
    if !cfg!(windows) {
        return Err("请在 Windows 专机开启主页同步".into());
    }
    if state
        .store
        .wb_discovery()
        .map_err(|e| e.to_string())?
        .is_some_and(|r| matches!(r.state.as_str(), "starting" | "running"))
    {
        return Ok(());
    }
    native_preflight(state)?;
    if let Some(job) = state.store.native_active().map_err(|e| e.to_string())? {
        if job.status == "running" {
            return Err("请先结束原有首页识别任务".into());
        }
        state
            .store
            .native_set_state(job.id, "cancelled", Some("切换至持续识别"))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn launch_home_sync(state: &AppState, directory: PathBuf, id: i64, created: bool, python: String) {
    if !created {
        return;
    }
    let store = state.store.clone();
    tauri::async_runtime::spawn(async move {
        core::discovery::run(store, directory, id, python).await;
    });
}

#[tauri::command]
pub fn workbench_home_sync_start(
    state: State<'_, AppState>,
    python_path: String,
) -> Result<i64, String> {
    prepare_home_sync(&state)?;
    let directory = root(&state)?;
    let (id, created) = state
        .store
        .wb_home_sync_start(None, false)
        .map_err(|e| e.to_string())?;
    launch_home_sync(&state, directory, id, created, python_path);
    Ok(id)
}

#[tauri::command]
pub fn workbench_recover_session(
    state: State<'_, AppState>,
    id: i64,
    python_path: String,
) -> Result<(), String> {
    prepare_home_sync(&state)?;
    let directory = root(&state)?;
    let (run, created) = state
        .store
        .wb_recover_session(id, mpider_core::model::now())
        .map_err(|e| e.to_string())?;
    if let Some(run) = run {
        launch_home_sync(&state, directory, run, created, python_path);
    }
    Ok(())
}
#[tauri::command]
pub fn workbench_discovery_stop(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(run) = state.store.wb_discovery().map_err(|e| e.to_string())? {
        if matches!(run.state.as_str(), "starting" | "running") {
            state
                .store
                .wb_discovery_state(
                    run.id,
                    "stopping",
                    Some("正在卸载观察器，已创建任务继续运行"),
                )
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// 系统日志独立分页，不从工作台快照截取；筛选和导出共用同一契约。
#[tauri::command]
pub fn workbench_logs(
    state: State<'_, AppState>,
    filter: mpider_core::store::system_logs::LogFilter,
    before_id: Option<i64>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<mpider_core::store::system_logs::LogPage, String> {
    let result = if let Some(offset) = offset {
        state
            .store
            .system_logs_offset(&filter, offset, limit.unwrap_or(10))
    } else {
        state
            .store
            .system_logs(&filter, before_id, limit.unwrap_or(10))
    };
    result.map_err(|e| e.to_string())
}
#[derive(Serialize)]
pub struct SystemLogExport {
    path: String,
    count: usize,
}
#[tauri::command]
pub async fn workbench_export_logs(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    filter: mpider_core::store::system_logs::LogFilter,
    format: String,
) -> Result<Option<SystemLogExport>, String> {
    if !["json", "txt"].contains(&format.as_str()) {
        return Err("请选择 JSON 或文本格式".into());
    }
    let store = state.store.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let name = format!(
            "mpider-system-log-{}.{}",
            (mpider_core::model::now() * 1000.) as u64,
            format
        );
        let selected = rfd::FileDialog::new()
            .set_parent(&window)
            .set_title("导出系统日志")
            .add_filter("日志文件", &[format.as_str()])
            .set_file_name(name)
            .save_file();
        let Some(path) = selected else {
            return Ok(None);
        };
        match store.save_system_log_export(&filter, &format, &path) {
            Ok(count) => Ok(Some(SystemLogExport {
                path: path.display().to_string(),
                count,
            })),
            Err(error) => {
                let _ = store.system_log(
                    "storage",
                    "error",
                    "日志导出写入失败，请检查目标文件夹权限和可用空间",
                    serde_json::json!({"format":format,"outcome":"write_failed"}),
                );
                Err(error.to_string())
            }
        }
    })
    .await
    .map_err(|_| "日志导出执行失败".to_string())?
}

/// 前端异常只记录有限分类及经过统一脱敏的文本，拒绝任意模块与协议载荷。
#[tauri::command]
pub fn workbench_ui_error(
    state: State<'_, AppState>,
    kind: String,
    message: String,
) -> Result<(), String> {
    if !["uncaught_error", "unhandled_rejection", "action_failed"].contains(&kind.as_str()) {
        return Err("未知前端异常类型".into());
    }
    state.store.system_log("app","error","界面执行异常",serde_json::json!({"reason_code":kind,"reason":message.chars().take(4000).collect::<String>()})).map(|_|()).map_err(|e|e.to_string())
}

/// 导出一个实验的全部计划与样本；取消保存不生成导出成功记录。
#[tauri::command]
pub async fn workbench_export_experiment(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    id: i64,
) -> Result<Option<SystemLogExport>, String> {
    let store = state.store.clone();
    let directory = root(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let selected = rfd::FileDialog::new()
            .set_parent(&window)
            .set_title("下载游标有效期实验数据")
            .add_filter("实验数据", &["json"])
            .set_file_name(format!("mpider-cursor-experiment-{id}.json"))
            .save_file();
        let Some(path) = selected else {
            return Ok(None);
        };
        let mut e = store.wb_experiment(id).map_err(|e| e.to_string())?;
        core::engine::write_report(&store, &directory, &mut e).map_err(|e| e.to_string())?;
        let source = directory
            .join("experiments")
            .join(id.to_string())
            .join("results.json");
        let bytes = std::fs::read(source).map_err(|e| e.to_string())?;
        core::archive::atomic_write(&path, &bytes).map_err(|e| e.to_string())?;
        let count = store.wb_probes(id).map_err(|e| e.to_string())?.len();
        store
            .system_log(
                "export",
                "info",
                "游标实验数据导出完成",
                serde_json::json!({"experiment_id":id,"count":count,"format":"json"}),
            )
            .map_err(|e| e.to_string())?;
        Ok(Some(SystemLogExport {
            path: path.display().to_string(),
            count,
        }))
    })
    .await
    .map_err(|_| "实验数据导出执行失败".to_string())?
}
