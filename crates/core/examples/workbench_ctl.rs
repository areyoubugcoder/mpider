//! 专机诊断与自动化入口；任务入队复用 Store，运行器与桌面共享数据目录锁。
use anyhow::{ensure, Context, Result};
use mpider_core::{
    model::now,
    native::NativeConfig,
    workbench::{self, HistoryTarget, Policy},
    RealRunConfig, Store,
};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{atomic::AtomicBool, Arc},
};

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ensure!(args.len()>=2,"用法：workbench_ctl <数据库> <logs|export-logs|snapshot|discovery-stop|policy|history|archive-source|export-source|download|export-articles|task-action|experiment|report|capture|cancel-native|run> [参数]");
    let db = PathBuf::from(&args[0]);
    ensure!(db.is_absolute(), "数据库须指定绝对路径");
    if let Some(parent) = db.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let store = Arc::new(Store::open(&db)?);
    let root = db
        .parent()
        .context("数据库须指定完整路径")?
        .join("workbench");
    match args[1].as_str() {
        "logs" | "export-logs" => {
            let filter: mpider_core::store::system_logs::LogFilter = match args.get(2) {
                Some(path) => serde_json::from_slice(&std::fs::read(path)?)?,
                None => Default::default(),
            };
            if args[1] == "logs" {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&store.system_logs(&filter, None, 100)?)?
                );
            } else {
                let format = args.get(3).context("请指定 json 或 txt")?;
                let path = PathBuf::from(args.get(4).context("请指定导出文件路径")?);
                ensure!(path.is_absolute(), "导出文件须指定绝对路径");
                let count = store.save_system_log_export(&filter, format, &path)?;
                println!("{}", json!({"count":count,"path":path}));
            }
        }
        "discovery-stop" => {
            if let Some(run) = store.wb_discovery()? {
                if matches!(run.state.as_str(), "starting" | "running") {
                    store.wb_discovery_state(run.id, "stopping", Some("更新软件，停止当前识别"))?;
                }
            }
            println!("已请求正常停止识别");
        }
        "snapshot" => {
            let path = PathBuf::from(args.get(2).context("请指定快照文件")?);
            workbench::archive::atomic_write(
                &path,
                &serde_json::to_vec_pretty(&store.wb_snapshot()?)?,
            )?;
            println!("{}", path.display());
        }
        "policy" => {
            let p: Policy =
                serde_json::from_slice(&std::fs::read(args.get(2).context("请指定策略 JSON")?)?)?;
            println!("策略版本 {}", store.wb_set_policy(p)?.version);
        }
        "history" => {
            let value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(args.get(2).context("请指定任务 JSON")?)?)?;
            let target: HistoryTarget = serde_json::from_value(value["target"].clone())?;
            let bizzes: Vec<String> = serde_json::from_value(value["bizzes"].clone())?;
            println!(
                "{}",
                json!({"tasks":store.wb_history_create(bizzes,target)?})
            );
        }
        "archive" => {
            let ids: Vec<i64> = args
                .get(2)
                .context("请指定文章 ID，逗号分隔")?
                .split(',')
                .map(str::parse)
                .collect::<std::result::Result<_, _>>()?;
            println!("{}", json!({"task":store.wb_archive_create(ids)?}));
        }
        "archive-source" => {
            let biz = args.get(2).context("请指定公众号 ID")?;
            println!("{}", json!({"download":store.wb_download_source(biz)?}));
        }
        "export-source" => {
            let biz = args.get(2).context("请指定公众号 ID")?;
            let destination = PathBuf::from(args.get(3).context("请指定导出目录")?);
            println!(
                "{}",
                serde_json::to_string(&workbench::markdown_export::export_source(
                    &store,
                    &root,
                    biz,
                    &destination
                )?)?
            );
        }
        "download" => {
            let ids: Vec<i64> = args
                .get(2)
                .context("请指定文章 ID")?
                .split(',')
                .map(str::parse)
                .collect::<std::result::Result<_, _>>()?;
            println!(
                "{}",
                serde_json::to_string(&store.wb_download_enqueue(ids)?)?
            );
        }
        "export-articles" => {
            let ids: Vec<i64> = args
                .get(2)
                .context("请指定文章 ID")?
                .split(',')
                .map(str::parse)
                .collect::<std::result::Result<_, _>>()?;
            let destination = PathBuf::from(args.get(3).context("请指定导出目录")?);
            println!(
                "{}",
                serde_json::to_string(&workbench::markdown_export::export_articles(
                    &store,
                    &root,
                    &ids,
                    &destination
                )?)?
            );
        }
        "experiment" => {
            let cursor = args.get(2).context("请指定游标 ID")?.parse()?;
            let schedule = match args.get(3) {
                Some(s) if s == "now" => Vec::new(),
                Some(s) => s
                    .split(',')
                    .map(str::parse)
                    .collect::<std::result::Result<Vec<f64>, _>>()?,
                None => (1..=24).map(f64::from).collect(),
            };
            println!(
                "{}",
                json!({"experiment":store.wb_experiment_create_with_session(cursor,schedule,args.get(4).map(String::as_str),now())?})
            );
        }
        "task-action" => {
            let id = args.get(2).context("请指定任务 ID")?.parse()?;
            let state = match args.get(3).map(String::as_str) {
                Some("pause") => "paused",
                Some("resume") => "queued",
                Some("cancel") => "cancelled",
                _ => anyhow::bail!("请选择 pause、resume 或 cancel"),
            };
            store.wb_task_state(id, state, Some("诊断工具显式操作"))?;
            println!("任务状态已保存");
        }
        "report" => {
            let mut experiment =
                store.wb_experiment(args.get(2).context("请指定实验 ID")?.parse()?)?;
            workbench::engine::write_report(&store, &root, &mut experiment)?;
            println!("{}", experiment.report_path.context("报告未生成")?);
        }
        "cancel-native" => {
            store.native_set_state(
                args.get(2).context("请指定任务 ID")?.parse()?,
                "cancelled",
                Some("诊断工具显式取消"),
            )?;
            println!("已取消指定首页识别任务");
        }
        "capture" => {
            let _lock = workbench::engine::instance_lock(&db)?;
            let mut native: NativeConfig = serde_json::from_slice(&std::fs::read(
                args.get(2).context("请指定原生配置 JSON")?,
            )?)?;
            native.enabled = true;
            native.transport = "session".into();
            native.max_pages = 1;
            let job = store.native_create(&native)?;
            let cfg = RealRunConfig {
                db_path: db.display().to_string(),
                native: Some(native),
                ..Default::default()
            };
            mpider_core::run_loop_real(cfg, false, Arc::new(AtomicBool::new(false)), |_| {})
                .await?;
            let snapshot = store.wb_snapshot()?;
            let result = store.native_job(job.id)?.context("任务不存在")?;
            println!(
                "{}",
                json!({"sources":snapshot.sources.len(),"cursors":snapshot.cursors.len(),"calls_24h":snapshot.list_calls_24h,"status":result.status,"pages":result.pages,"reason":result.pause_reason})
            );
            ensure!(
                result.status == "done" && result.pages == 1,
                "首页识别未成功完成，详情见任务记录"
            );
        }
        "run" => {
            let _lock = workbench::engine::instance_lock(&db)?;
            store.wb_restart(now())?;
            let seconds = args
                .get(2)
                .map(|s| s.parse())
                .transpose()?
                .unwrap_or(86460u64);
            tokio::select! {_=workbench::engine::run(store.clone(),root)=>{},_=tokio::signal::ctrl_c()=>{},_=tokio::time::sleep(std::time::Duration::from_secs(seconds))=>{}}
        }
        _ => anyhow::bail!("未知命令"),
    }
    Ok(())
}
