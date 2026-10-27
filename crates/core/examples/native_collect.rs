//! 原生采集命令行入口；Windows 专机读取本地配置，复用 Orchestrator 主循环与请求账本。
use anyhow::{Context, Result};
use mpider_core::{
    runner::{run_loop_real, RealRunConfig},
    Store,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        !args.is_empty(),
        "用法：native_collect <本地配置.json> [--resume | --status]"
    );
    anyhow::ensure!(
        args.len() <= 2
            && args
                .get(1)
                .is_none_or(|s| ["--resume", "--status"].contains(&s.as_str())),
        "未知参数"
    );
    let cfg: RealRunConfig = serde_json::from_slice(&std::fs::read(&args[0])?)?;
    let native = cfg.native.as_ref().context("配置缺少 native")?;
    anyhow::ensure!(native.enabled, "请启用 native.enabled");
    if let Some(parent) = std::path::Path::new(&cfg.db_path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let store = Store::open(&cfg.db_path)?;
    if args.get(1).is_some_and(|s| s == "--status") {
        let jobs:Vec<_>=store.native_recent(20)?.iter().map(|j|serde_json::json!({"id":j.id,"status":j.status,"pages":j.pages,"total_pages":j.total_pages,"new_articles":j.new_articles,"pause_reason":j.pause_reason})).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({"jobs":jobs,"articles":store.count_articles(None)?,"calls_24h":store.native_calls_window(mpider_core::model::now()-86400.)?.0,"calls":store.native_calls_since(mpider_core::model::now()-86400.,1000)?})
            )?
        );
        return Ok(());
    }
    anyhow::ensure!(store.history_active()?.is_none(), "请先结束旧历史任务");
    if args.get(1).is_some_and(|s| s == "--resume") {
        store.native_pause_on_restart()?;
        let job = store.native_active()?.context("没有待恢复任务")?;
        store.native_resume(job.id)?;
    } else {
        store.native_create(native)?;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let signal = stop.clone();
    let listener = tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        signal.store(true, Ordering::SeqCst);
    });
    let result = run_loop_real(cfg, false, stop, |event| println!("{}", event.render())).await;
    listener.abort();
    if result.is_err() {
        if let Some(job) = store.native_active()? {
            let _ = store.native_set_state(
                job.id,
                "paused",
                Some("主循环启动失败，请检查其它采集任务"),
            );
        }
    }
    result?;
    let recent = store.native_recent(1)?;
    if let Some(job) = recent.first() {
        println!(
            "任务 {}：{}，本次 {} 页，累计新增 {} 篇",
            job.id, job.status, job.pages, job.new_articles
        );
        anyhow::ensure!(job.status == "done", "采集暂停，请检查本地请求账本");
    }
    Ok(())
}
