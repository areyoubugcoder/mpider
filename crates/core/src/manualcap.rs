//! 手动抓凭证代理（调试用，2026-09-08，Python 无对应）。
//!
//! 正常流程里 MITM 抓凭证代理只在任务接力队列非空时由编排器临时拉起、任务收尾即停，GUI 没有
//! 单独的开关。排查「某类链接在微信内置浏览器里打开时是否经代理、是否带凭证、响应是什么」
//! （例如公众号主页 `profile_ext?action=home` 能否一次同时截到凭证与首屏列表）需要一个**不跑任务、
//! 只开代理**的入口——这就是本模块：
//!
//! - [`start`]：起 MITM 代理（复用已安装的根证书）+ 按配置设系统代理，**开着请求追踪**
//!   （[`crate::capture::CaptureConfig::trace_requests`]：每条解密到的微信请求的路径与 `profile_ext`
//!   响应概况写进环节日志「抓凭证」），到时自动停；抓到的凭证照常入库（`capture_request`），
//!   之后任务可直接复用。
//! - [`stop`]：立即停（复位系统代理 → 停代理）。
//! - [`status`]：GUI 轮询。
//!
//! 与整链**互斥**：持有 [`crate::runstate::try_acquire_run`] 的运行守卫，开着期间「运行一次」/
//! 轮询领到任务会报「已有采集在运行」；反过来采集进行中也开不了。不接接力队列（`relay_enabled=false`）、
//! 不碰种子入口服务、不点微信——用户自己在微信里点链接。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Result};
use serde::Serialize;

use crate::applog::{self, Stage};
use crate::capture::{self, CaptureConfig, CaptureProxy, PassthroughDecider};
use crate::relay::RelayQueue;
use crate::runner::{load_or_create_ca, RealRunConfig, SysProxyApplier, RUN_BUSY_MSG};
use crate::runstate::{self, RunGuard};
use crate::store::Store;
#[cfg(not(windows))]
use crate::sysproxy::CommandApplier;
use crate::sysproxy::SystemProxyGuard;

/// 默认开启时长（分钟）。
pub const DEFAULT_MINUTES: u64 = 10;
/// 时长上限（分钟）：开着期间整链互斥，忘了关也不至于一直占着。
pub const MAX_MINUTES: u64 = 60;

/// 状态（GUI 轮询；不含密码 / 凭证）。
#[derive(Clone, Debug, Default, Serialize)]
pub struct ManualCaptureStatus {
    /// 是否在跑。
    pub running: bool,
    /// 代理监听地址（`127.0.0.1:<port>`）。
    pub proxy_addr: Option<String>,
    /// 系统代理是否已指向本代理（`set_sysproxy=false` 时为 false，需手动设）。
    pub sysproxy_set: bool,
    /// 开启时刻 / 计划自动停止时刻（epoch 秒）。
    pub started_at: Option<f64>,
    pub stop_at: Option<f64>,
    /// 最近一次开启 / 停止出错的原因。
    pub last_error: Option<String>,
}

/// 运行中的实例：代理句柄 + 系统代理守卫（Drop 复位）+ 整链运行守卫 + 自动停止任务。
struct Running {
    proxy: Option<CaptureProxy>,
    sysproxy: Option<SystemProxyGuard<SysProxyApplier>>,
    _run_guard: RunGuard,
    started_at: f64,
    stop_at: f64,
    auto_stop: Option<tokio::task::JoinHandle<()>>,
}

static STATE: Mutex<Option<Running>> = Mutex::new(None);
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

fn set_error(e: Option<String>) {
    *LAST_ERROR.lock().unwrap() = e;
}

/// 当前状态。
pub fn status() -> ManualCaptureStatus {
    let st = STATE.lock().unwrap();
    let last_error = LAST_ERROR.lock().unwrap().clone();
    match st.as_ref() {
        Some(r) => ManualCaptureStatus {
            running: true,
            proxy_addr: r.proxy.as_ref().map(|p| p.addr.to_string()),
            sysproxy_set: r.sysproxy.is_some(),
            started_at: Some(r.started_at),
            stop_at: Some(r.stop_at),
            last_error,
        },
        None => ManualCaptureStatus {
            last_error,
            ..Default::default()
        },
    }
}

/// 是否在跑。
pub fn is_running() -> bool {
    STATE.lock().unwrap().is_some()
}

/// 开启：`minutes` 到时自动停（0 / None = 默认 [`DEFAULT_MINUTES`]，封顶 [`MAX_MINUTES`]）。
/// 已在跑则原样返回状态（幂等，不重置计时）。
pub async fn start(cfg: &RealRunConfig, minutes: Option<u64>) -> Result<ManualCaptureStatus> {
    if is_running() {
        return Ok(status());
    }
    let Some(run_guard) = runstate::try_acquire_run() else {
        set_error(Some(RUN_BUSY_MSG.to_string()));
        bail!(RUN_BUSY_MSG);
    };
    let minutes = minutes
        .filter(|m| *m > 0)
        .unwrap_or(DEFAULT_MINUTES)
        .min(MAX_MINUTES);

    let started = async {
        let ca = load_or_create_ca(cfg)?;
        let db_path = if cfg.db_path.is_empty() {
            "data/mpider.db".to_string()
        } else {
            cfg.db_path.clone()
        };
        let store = Arc::new(Store::open(&db_path)?);
        store.set_cred_ttl(cfg.cred_ttl_seconds);
        let proxy = capture::start_on(
            CaptureConfig {
                ca,
                store,
                // 不接接力：用户手动点链接，页面不注入脚本、不推进队列。
                relay_enabled: false,
                relay_dwell_ms: cfg.relay_dwell_ms,
                relay_dwell_max_ms: cfg.relay_dwell_max_ms,
                seed_dwell_ms: cfg.seed_dwell_ms,
                decider: PassthroughDecider::new(),
                relay: RelayQueue::new(),
                bootstrap_seed: None,
                trace_requests: true,
                // 手动抓凭证不接接力，也不回落待命页。
                resident_home: None,
            },
            &format!("127.0.0.1:{}", cfg.capture_port),
        )
        .await?;
        Ok::<CaptureProxy, anyhow::Error>(proxy)
    }
    .await;
    let proxy = match started {
        Ok(p) => p,
        Err(e) => {
            let msg = format!("手动抓凭证代理启动失败：{e:#}");
            applog::error(Stage::Proxy, msg.clone());
            set_error(Some(msg.clone()));
            drop(run_guard);
            bail!(msg);
        }
    };
    let addr = proxy.addr;
    runstate::set_capture_addr(Some(addr.to_string()));
    applog::info(
        Stage::Proxy,
        format!(
            "手动抓凭证代理已启动（调试），监听 {addr}，{minutes} 分钟后自动停止；期间解密到的微信请求路径会记进「抓凭证」环节日志"
        ),
    );

    // 系统代理：与 runner 一致（macOS networksetup / Windows WinINET），Drop 复位。
    let host = "127.0.0.1";
    let port = addr.port();
    let service = cfg.sysproxy_service.clone();
    #[cfg(windows)]
    let _ = &service;
    let sysproxy = if cfg.set_sysproxy {
        #[cfg(windows)]
        let res = SystemProxyGuard::set(
            crate::sysproxy::WinInetApplier,
            crate::sysproxy::win_commands(true, host, port),
            crate::sysproxy::win_commands(false, host, port),
        );
        #[cfg(not(windows))]
        let res = SystemProxyGuard::set_mac(CommandApplier, host, port, &service);
        match res {
            Ok(g) => {
                applog::info(
                    Stage::Proxy,
                    format!("系统代理已指向 {host}:{port}（停止 / 到时自动复位）"),
                );
                Some(g)
            }
            Err(e) => {
                applog::error(
                    Stage::Proxy,
                    format!("设置系统代理失败：{e}；微信流量不会经过本机代理，请手动设置后再试"),
                );
                set_error(Some(format!("设置系统代理失败：{e}")));
                None
            }
        }
    } else {
        applog::warn(
            Stage::Proxy,
            format!("未自动设系统代理(set_sysproxy=false)，请手动把系统代理指向 {host}:{port}"),
        );
        None
    };

    let now = crate::model::now();
    let stop_at = now + (minutes * 60) as f64;
    let auto_stop = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(minutes * 60)).await;
        applog::info(
            Stage::Proxy,
            format!("手动抓凭证代理已开满 {minutes} 分钟，自动停止"),
        );
        stop_inner(false).await;
    });
    *STATE.lock().unwrap() = Some(Running {
        proxy: Some(proxy),
        sysproxy,
        _run_guard: run_guard,
        started_at: now,
        stop_at,
        auto_stop: Some(auto_stop),
    });
    set_error(None);
    Ok(status())
}

/// 停止（幂等）：先复位系统代理，再停 MITM，释放整链运行守卫。
pub async fn stop() -> ManualCaptureStatus {
    stop_inner(true).await;
    status()
}

async fn stop_inner(cancel_timer: bool) {
    let running = STATE.lock().unwrap().take();
    let Some(mut r) = running else {
        return;
    };
    if cancel_timer {
        if let Some(h) = r.auto_stop.take() {
            h.abort();
        }
    }
    if let Some(g) = r.sysproxy.take() {
        drop(g);
        applog::info(Stage::Proxy, "系统代理已复位".to_string());
    }
    if let Some(p) = r.proxy.take() {
        p.shutdown().await;
    }
    runstate::set_capture_addr(None);
    applog::info(Stage::Proxy, "手动抓凭证代理已停止".to_string());
    // `r._run_guard` 随 r Drop 释放整链互斥。
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_idle_by_default() {
        let s = status();
        assert!(!s.running);
        assert!(s.proxy_addr.is_none());
    }
}
