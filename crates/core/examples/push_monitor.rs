//! 显式运行本地推送监测的诊断入口；使用调用方提供的独立数据库，不自动打开微信页面。
#[cfg(windows)]
use anyhow::ensure;
use anyhow::Result;
#[cfg(windows)]
use mpider_core::{
    workbench::push_monitor::{self, Config, Scanner},
    Store,
};

#[tokio::main]
async fn main() -> Result<()> {
    #[cfg(not(windows))]
    anyhow::bail!("推送监测诊断仅支持 Windows");
    #[cfg(windows)]
    {
        let args: Vec<String> = std::env::args().collect();
        if args.get(1).map(String::as_str) == Some("scan") {
            let scan = push_monitor::WindowsScanner::default()
                .scan(&std::sync::atomic::AtomicBool::new(false))?;
            println!(
                "{}",
                serde_json::json!({"processes":scan.processes,"read_bytes":scan.read_bytes,"links":scan.links.len(),"inaccessible":scan.inaccessible})
            );
            return Ok(());
        }
        ensure!(
            args.len() == 5 && args[1] == "run",
            "用法：push_monitor scan | push_monitor run <独立数据库> <秒数> <每小时网页上限>"
        );
        let seconds: u64 = args[3].parse()?;
        let budget: i64 = args[4].parse()?;
        ensure!((1..=3600).contains(&seconds), "诊断时长须为 1 至 3600 秒");
        let store = std::sync::Arc::new(Store::open(std::path::Path::new(&args[2]))?);
        store.push_restart()?;
        let id = store.push_start(Config {
            hourly_budget: budget,
            ..Default::default()
        })?;
        let runner = tokio::spawn(push_monitor::run(store.clone(), id));
        let until = tokio::time::Instant::now() + std::time::Duration::from_secs(seconds);
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let status = store.push_status()?;
            // 不向终端输出文章 URL、内存片段、正文或会话信息。
            println!(
                "{}",
                serde_json::json!({"run_id":id,"active":status.run.active,"phase":status.run.phase,"rounds":status.run.rounds,"processes":status.run.processes,"read_bytes":status.run.read_bytes,"pending":status.pending,"imported":status.imported,"requests_hour":status.requests_hour,"reason":status.run.reason})
            );
            if tokio::time::Instant::now() >= until || !status.run.active {
                break;
            }
        }
        store.push_end(id, "stopped", None)?;
        tokio::time::timeout(std::time::Duration::from_secs(30), runner).await??;
        Ok(())
    }
}
