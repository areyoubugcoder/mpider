//! MITM 抓凭证 + TLS 直通 —— 可行性冒烟 example。
//!
//! 运行：
//! ```bash
//! # 只起代理，打印 CA 与监听地址（把系统/浏览器代理指到该地址、信任 CA 后即可手动验证）
//! cargo run -p mpider-core --example mitm_smoke
//!
//! # 起代理并对指定 HTTPS 做一次自证请求（本进程 reqwest 信任生成的 CA，经代理请求）
//! cargo run -p mpider-core --example mitm_smoke -- https://example.com
//! ```
//!
//! 它做三件事：
//! 1. 用 rcgen 生成根 CA（写到临时目录，打印路径）。
//! 2. 启动进程内 MITM 代理（hudsucker），对 `mp.weixin.qq.com` 抓凭证入库、对固定证书
//!    主机 / 无 SNI 连接 **TLS 直通**。
//! 3. 若给了 URL：本进程 reqwest 信任该 CA、经代理请求它，打印状态码与“代理在明文层
//!    看到的请求”（证明 MITM 生效）。

use std::sync::Arc;

use mpider_core::capture::{self, CaptureConfig, PassthroughDecider};
use mpider_core::relay::RelayQueue;
use mpider_core::{CaMaterial, Store};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,mpider_core=debug".into()),
        )
        .init();

    // 1) 生成并落盘 CA（真实产品里这是“证书信任向导”要安装到系统信任库的根证书）。
    let ca = CaMaterial::generate()?;
    let dir = std::env::temp_dir().join("mpider-core-mitm-smoke");
    std::fs::create_dir_all(&dir)?;
    let ca_crt = dir.join("ca.crt");
    let ca_key = dir.join("ca.key");
    std::fs::write(&ca_crt, ca.cert_pem.as_bytes())?;
    std::fs::write(&ca_key, ca.key_pem.as_bytes())?;
    println!("CA 证书: {}", ca_crt.display());
    println!("CA 私钥: {}", ca_key.display());

    // 2) 起 MITM 代理（进程内内存库；接力先关）。
    let store = Arc::new(Store::open_in_memory()?);
    let decider = PassthroughDecider::new();
    let proxy = capture::start(CaptureConfig {
        ca: ca.clone(),
        store,
        relay_enabled: false,
        relay_dwell_ms: 2500,
        relay_dwell_max_ms: 2500,
        seed_dwell_ms: 1000,
        decider,
        relay: RelayQueue::new(),
        bootstrap_seed: None,
        trace_requests: false,
        resident_home: None,
    })
    .await?;
    println!("MITM 代理监听: http://{}", proxy.addr);
    println!(
        "直通规则：无 SNI / *.long.weixin.qq.com / *.short.weixin.qq.com / dns.weixin.qq.com / mmtls.* → TLS 直通"
    );

    // 3) 可选：对指定 URL 自证一次（信任 CA + 走代理）。
    if let Some(url) = std::env::args().nth(1) {
        println!("\n经代理请求：{url}");
        let client = reqwest::Client::builder()
            .add_root_certificate(reqwest::Certificate::from_pem(ca.cert_pem.as_bytes())?)
            .proxy(reqwest::Proxy::all(format!("http://{}", proxy.addr))?)
            .build()?;
        match client.get(&url).send().await {
            Ok(resp) => {
                println!("状态码: {}", resp.status());
                let body = resp.text().await.unwrap_or_default();
                let end = body.len().min(120);
                println!("响应前 {end} 字节: {:?}", &body[..end]);
            }
            Err(e) => println!("请求失败（可能无网络）：{e}"),
        }
        println!("\n代理在明文层看到的请求（证明 MITM 生效）:");
        for u in proxy.seen_urls() {
            println!("  - {u}");
        }
    } else {
        println!("\n未提供 URL：代理将空转 3 秒后退出（可传一个 https URL 做自证）。");
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }

    proxy.shutdown().await;
    Ok(())
}
