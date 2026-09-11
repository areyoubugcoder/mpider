//! 抓取前置「拦截探针」：**真设一次系统代理 → 经 MITM 发一个 HTTPS 请求验证能否拦截 → 立即复位**。
//!
//! 目的：在真正跑抓取之前，把「证书没装进系统信任库 / 系统代理设不上 / MITM 拦不下来」这类问题
//! 提前暴露出来。与真机整链同一套装配（同一张 CA、同样按 SNI 决定 MITM/直通），所以探针通过≈真跑能拦。
//!
//! 判据（对齐 `capture.rs` 的 `test_mitm_decrypts_https_through_proxy`）：客户端**只在默认根之外额外信任
//! 我方 CA**，经该代理请求一个默认会被 MITM 的主机（`mp.weixin.qq.com`）；只要 MITM 现签叶证书并在**明文层
//! 看到**该请求（`seen_urls` 命中），即证明拦截成立。系统代理用 RAII 守卫设置，函数结束（正常/异常）都复位。

use std::sync::Arc;
use std::time::Duration;

use crate::ca::CaMaterial;
use crate::capture::{self, CaptureConfig, PassthroughDecider};
use crate::relay::RelayQueue;
use crate::store::Store;
use crate::sysproxy::SystemProxyGuard;

/// 拦截探针结果（可 serde 给 GUI）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct InterceptProbe {
    /// 系统代理是否成功设上（Windows 走 WinINET，mac 走 networksetup）。
    pub proxy_set_ok: bool,
    /// 是否成功拦截到 HTTPS（MITM 在明文层看到请求）。
    pub intercept_ok: bool,
    /// 人类可读诊断。
    pub message: String,
}

/// 平台相关的系统代理执行器（与 `runner.rs` 一致）。
#[cfg(windows)]
type ProbeApplier = crate::sysproxy::WinInetApplier;
#[cfg(not(windows))]
type ProbeApplier = crate::sysproxy::CommandApplier;

#[cfg(windows)]
fn probe_applier() -> ProbeApplier {
    crate::sysproxy::WinInetApplier
}
#[cfg(not(windows))]
fn probe_applier() -> ProbeApplier {
    crate::sysproxy::CommandApplier
}

#[cfg(windows)]
fn proxy_commands(host: &str, port: u16, _service: &str) -> (Vec<Vec<String>>, Vec<Vec<String>>) {
    (
        crate::sysproxy::win_commands(true, host, port),
        crate::sysproxy::win_commands(false, host, port),
    )
}
#[cfg(not(windows))]
fn proxy_commands(host: &str, port: u16, service: &str) -> (Vec<Vec<String>>, Vec<Vec<String>>) {
    (
        crate::sysproxy::mac_commands(true, host, port, service),
        crate::sysproxy::mac_commands(false, host, port, service),
    )
}

/// 探针主流程。`set_sysproxy=false` 时只测 MITM 拦截、不动系统代理（proxy_set_ok 视作跳过=true）。
///
/// 全程 best-effort：任何一步失败都返回带原因的结果，绝不 panic；系统代理由 RAII 守卫保证复位。
pub async fn probe_intercept(
    ca: CaMaterial,
    set_sysproxy: bool,
    sysproxy_service: &str,
) -> InterceptProbe {
    capture::install_default_crypto();

    // 1) 起 MITM 于临时端口（内存库、关接力）。
    let store = match Store::open_in_memory() {
        Ok(s) => Arc::new(s),
        Err(e) => {
            return InterceptProbe {
                proxy_set_ok: false,
                intercept_ok: false,
                message: format!("探针内存库失败：{e}"),
            }
        }
    };
    let proxy = match capture::start_on(
        CaptureConfig {
            ca: ca.clone(),
            store,
            relay_enabled: false,
            relay_dwell_ms: 0,
            relay_dwell_max_ms: 0,
            seed_dwell_ms: 0,
            decider: PassthroughDecider::new(),
            relay: RelayQueue::new(),
            bootstrap_seed: None,
            trace_requests: false,
            resident_home: None,
        },
        "127.0.0.1:0",
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            return InterceptProbe {
                proxy_set_ok: false,
                intercept_ok: false,
                message: format!("启动 MITM 代理失败：{e}"),
            }
        }
    };
    let addr = proxy.addr;

    // 2) 设系统代理（RAII，作用域结束自动复位）。
    let mut proxy_set_ok = !set_sysproxy; // 不设时视作跳过
    let mut set_err = String::new();
    let _guard = if set_sysproxy {
        let (enable, disable) =
            proxy_commands(&addr.ip().to_string(), addr.port(), sysproxy_service);
        match SystemProxyGuard::<ProbeApplier>::set(probe_applier(), enable, disable) {
            Ok(g) => {
                proxy_set_ok = true;
                Some(g)
            }
            Err(e) => {
                set_err = e.to_string();
                None
            }
        }
    } else {
        None
    };

    // 3) 经 MITM 发一个默认会被拦截的 HTTPS 请求（客户端额外信任我方 CA + 显式走该代理）。
    let mut req_err = String::new();
    let client = reqwest::Certificate::from_pem(ca.cert_pem.as_bytes()).and_then(|cert| {
        reqwest::Client::builder()
            .add_root_certificate(cert)
            .proxy(reqwest::Proxy::all(format!("http://{addr}"))?)
            .timeout(Duration::from_secs(8))
            .build()
    });
    match client {
        Ok(client) => {
            // 目标故意用 mp.weixin.qq.com（默认 decider 对它 MITM）；无需真微信，代理现签叶证书解密。
            if let Err(e) = client
                .get("https://mp.weixin.qq.com/s?__biz=MzPROBE==&mid=1&idx=1&sn=probe")
                .send()
                .await
            {
                req_err = e.to_string();
            }
        }
        Err(e) => req_err = format!("构造探针客户端失败：{e}"),
    }
    // 拦截证据：处理器在明文层看到该请求（即便上游 502/超时，解密已发生也算拦截成立）。
    let intercept_ok = proxy
        .seen_urls()
        .iter()
        .any(|u| u.contains("mp.weixin.qq.com"));

    // 4) 复位（守卫 Drop）+ 关 MITM。
    drop(_guard);
    proxy.shutdown().await;

    let message = if intercept_ok {
        "代理拦截正常：已设系统代理 → 拦到 HTTPS → 已复位".to_string()
    } else if !proxy_set_ok {
        format!("设置系统代理失败：{set_err}（Windows 需 WinINET / mac 需正确的网络服务名）")
    } else {
        format!(
            "拦截失败：{}（多半是证书没装进系统信任库，或网络不通）",
            if req_err.is_empty() {
                "未在明文层看到请求".to_string()
            } else {
                req_err
            }
        )
    };
    InterceptProbe {
        proxy_set_ok,
        intercept_ok,
        message,
    }
}
