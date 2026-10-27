//! 手动抓凭证代理探针（manualcap_probe）：**脱离 GUI** 在终端里起「手动抓凭证代理」
//! （与设置页「手动开启抓凭证代理（调试）」同一条代码路径 `manualcap::start`），把环节日志实时打到 stdout，
//! 用来在没人点界面的情况下验证「微信内置浏览器是否走系统代理、是否信任根证书、能否抓到凭证」。
//!
//! 用法：
//! ```text
//! cargo run -p mpider-core --example manualcap_probe -- \
//!     --db "<app_data_dir>/mpider.db" --ca-dir "<app_data_dir>" --service Wi-Fi --minutes 10
//! ```
//! 开着期间去微信里打开一篇公众号文章；stdout 出现「解密到微信请求：/s（微信内置浏览器，主文档，带 key…）」
//! 与「凭证获取成功」即通过。Ctrl-C 或到时自动停，两者都会复位系统代理。
//! `--db` / `--ca-dir` 指向 GUI 的数据目录时复用已安装的根证书、凭证直接入 GUI 的库。

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use mpider_core::applog;
use mpider_core::runner::RealRunConfig;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut cfg = RealRunConfig {
        set_sysproxy: true,
        ..RealRunConfig::default()
    };
    if let Some(s) = arg(&args, "--service") {
        cfg.sysproxy_service = s;
    }
    if let Some(db) = arg(&args, "--db") {
        cfg.db_path = db;
    }
    if let Some(dir) = arg(&args, "--ca-dir") {
        let dir = std::path::Path::new(&dir);
        cfg.ca_cert_path = dir.join("ca.crt").display().to_string();
        cfg.ca_key_path = dir.join("ca.key").display().to_string();
    }
    let minutes: u64 = arg(&args, "--minutes")
        .map(|m| m.parse())
        .transpose()?
        .unwrap_or(mpider_core::manualcap::DEFAULT_MINUTES);
    if minutes == 0 {
        bail!("--minutes 必须大于 0");
    }

    // 环节日志实时打到 stdout（已脱敏），瞬态行也打，便于看到倒计时。
    let _sub = applog::bus().subscribe(Arc::new(|e: &applog::LogEvent| {
        println!(
            "{} [{}][{}] {}",
            applog::format_local(e.ts),
            e.stage.label(),
            e.level.as_str(),
            e.message
        );
    }));

    let st = mpider_core::manualcap::start(&cfg, Some(minutes)).await?;
    println!(
        "== 探针已开启：监听 {}，系统代理{}（网络服务「{}」），{} 分钟后自动停；Ctrl-C 立即停 ==",
        st.proxy_addr.as_deref().unwrap_or("?"),
        if st.sysproxy_set { "已设" } else { "未设" },
        cfg.sysproxy_service,
        minutes
    );
    println!("== 现在去微信里打开一篇公众号文章 ==");

    let wait_stop = async {
        while mpider_core::manualcap::is_running() {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => println!("== 收到 Ctrl-C，停止探针 =="),
        _ = wait_stop => println!("== 到时自动停止 =="),
    }
    let st = mpider_core::manualcap::stop().await;
    println!(
        "== 探针已停止，系统代理已复位{} ==",
        st.last_error
            .as_deref()
            .map(|e| format!("；最近错误：{e}"))
            .unwrap_or_default()
    );
    Ok(())
}
