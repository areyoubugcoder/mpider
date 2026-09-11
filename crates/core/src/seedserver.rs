//! 本机种子入口服务 —— App 自己起一个固定端口的 HTTP 服务，作为微信「文件传输助手」里那条
//! **固定种子链接**的落点。
//!
//! 流程：用户首次把 `http://<seed_host>:<seed_port>/` 发进文件助手 → 每批任务 RPA 点它拉起微信内置
//! 浏览器 → 内置浏览器请求到本服务 → 本服务**直接**把接力脚本（`location.href` 跳到本批首条任务链接，
//! 见 [`crate::relay`]）写进返回的 HTML，不再依赖 MITM 解密种子页再注入。
//!
//! 为什么要自己起服务而不是继续用外网域名：
//! - 起跳快、不依赖外网可达 / DNS / 该域名的证书；走 http 不走 https，根 CA 没装好种子页也能开。
//! - 种子页的 HTML 由本进程输出，可直接读进程内 [`RelayQueue`]，少一层 MITM 注入。
//! - 内置浏览器（Chromium 内核）对 `127.0.0.1` / `localhost` **默认绕过系统代理**，种子页请求根本不经过
//!   MITM 代理，所以入口页的三件事（取队列首条、记心跳、注入脚本）必须在这里做；`capture.rs` 里按 host
//!   识别入口页的分支仍保留——种子 host 配成局域网 IP（实测 `http://192.0.2.10:8787/` 可点开）时
//!   请求会经代理，两条路都能接住（服务已注入的页含 MARKER，代理侧不会二次注入）。
//!
//! 注入闸门与 MITM 一致：接力开启且队列非空、UA 含 `MicroMessenger`（同机 Chrome 打开只给说明页、不消耗
//! 队列）、只对主文档（`Sec-Fetch-Dest: document`，没带该头视为主文档）。响应 `Cache-Control: no-store`，
//! 避免内置浏览器命中缓存跳到上一批的链接。**2026-09-08 起 RPA 点开后不再 Ctrl+F5 硬刷**：硬刷是外网域名
//! 时代（种子页经 MITM 注入、命中缓存代理就看不到）的遗留，本机直出 + no-store 后没有意义，且它落在
//! 首条文章页上白白多一次重载；文章页自身的缓存由 MITM 改写 `/s` 响应时统一加 `no-store` 兜住（`capture.rs`）。
//! 保留 `rpa_hard_refresh` 开关（默认关）供回退验证。
//!
//! **待命页自轮询**（2026-09-11 起，人工模式专用）：mac / 关闭 RPA 时没人替我们点种子，于是让内置浏览器
//! **留在种子页上待命**：待命页用 `fetch` 长轮询 [`WAIT_PATH`]（服务端最多挂 [`WAIT_HOLD_SECS`] 秒，队列一有任务就
//! 返回首条），拿到链接即 `location.href` 跳走；每批接力跑完，MITM 注入的尾部脚本再把浏览器送回种子页
//! （`capture.rs` 的 `resident_home`），于是**第一次**要人打开一篇文章，之后每批都自动。用 `fetch` 链而不用
//! `setTimeout` 轮询，是因为 Chromium 对后台页的定时器会节流到分钟级，而 promise 续接不受影响。
//! 编排层按 [`resident_alive`]（最近 [`RESIDENT_ALIVE_SECS`] 秒内有过微信 UA 的长轮询）决定这批是交给待命页还是
//! 推「请打开一篇文章」提醒。只在 [`set_resident_mode`] 打开时启用；Windows 自动点击模式下种子页空闲态仍是纯说明页，
//! 免得上一批没关掉的窗口与新点开的种子页抢同一批队列。
//!
//! **常驻**（2026-09-08 起）：服务不再随抓凭证代理起停，而是进程级单例（[`apply`] 幂等确保在跑，自带线程与
//! tokio 运行时），应用启动 / 保存配置 / 每次运行开始都会调；没有任务在跑时也能打开，只给一行说明页，随时
//! 可验证文件助手里那条链接与框选没坏。运行开始 [`attach`] 接上本次的接力队列，收尾 [`detach`] 换回空队列，
//! 避免空闲时点开跳到上一批残留的链接。改了 host / 端口即换绑（旧的先停），绑定失败记 `last_error`、下次再试。

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use http_body_util::Full;
use hudsucker::hyper::body::Bytes;
use hudsucker::hyper::server::conn::http1;
use hudsucker::hyper::service::service_fn;
use hudsucker::hyper::{header, Request, Response};
use hudsucker::hyper_util::rt::TokioIo;
use serde::Serialize;
use tracing::{error, info};

use crate::applog::{self, Stage};
use crate::model::{now, Epoch};
use crate::relay::{self, RelayQueue};

/// 种子服务默认端口（实测该端口链接可点开）。
pub const DEFAULT_SEED_PORT: u16 = 8787;

/// 种子服务默认 host（写进种子链接里的地址）。
pub const DEFAULT_SEED_HOST: &str = "127.0.0.1";

/// 待命页长轮询的路径（种子服务自己处理，内置浏览器对回环地址绕过代理，不经 MITM）。
pub const WAIT_PATH: &str = "/wait";
/// 一次长轮询服务端最多挂多久（秒）；到时返回空，页面立刻再发下一次。
pub const WAIT_HOLD_SECS: u64 = 20;
/// 最近多少秒内有过微信 UA 的长轮询就算待命页在线（长轮询周期 + 余量）。
pub const RESIDENT_ALIVE_SECS: f64 = 45.0;

/// 待命页自轮询是否启用（人工模式：mac / 关闭 RPA）。`runner::build_orchestrator` / GUI 应用配置时设。
static RESIDENT_MODE: AtomicBool = AtomicBool::new(false);
/// 最近一次微信 UA 长轮询的时刻（待命页心跳）。
static RESIDENT_LAST_POLL: Mutex<Option<Epoch>> = Mutex::new(None);

/// 设置待命页自轮询开关（见模块文档）。
pub fn set_resident_mode(on: bool) {
    RESIDENT_MODE.store(on, Ordering::Relaxed);
}

/// 待命页自轮询是否启用。
pub fn resident_mode() -> bool {
    RESIDENT_MODE.load(Ordering::Relaxed)
}

/// 记一次待命页心跳（长轮询到达；测试也用它模拟「待命页在线」）。
pub fn note_resident_poll() {
    *RESIDENT_LAST_POLL.lock().unwrap_or_else(|e| e.into_inner()) = Some(now());
}

/// 清掉待命页心跳（测试用）。
pub fn reset_resident_poll() {
    *RESIDENT_LAST_POLL.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// 最近一次待命页心跳时刻。
pub fn resident_last_poll_at() -> Option<Epoch> {
    *RESIDENT_LAST_POLL.lock().unwrap_or_else(|e| e.into_inner())
}

/// 待命页是否在线：开关打开且最近 [`RESIDENT_ALIVE_SECS`] 秒内有过长轮询。编排 / 续期据此决定
/// 本批交给待命页自动接力，还是推「请打开一篇文章」提醒。
pub fn resident_alive() -> bool {
    resident_mode()
        && resident_last_poll_at()
            .map(|t| now() - t <= RESIDENT_ALIVE_SECS)
            .unwrap_or(false)
}

/// 种子链接（用户复制进文件传输助手的那条）：`http://<host>:<port>/`。
///
/// host 为空按 [`DEFAULT_SEED_HOST`]，port 为 0 按 [`DEFAULT_SEED_PORT`]。
pub fn seed_url(host: &str, port: u16) -> String {
    let (h, p) = normalize(host, port);
    format!("http://{h}:{p}/")
}

/// 服务实际绑定地址：种子 host 是回环地址就只绑 `127.0.0.1`（不对局域网暴露）；配成局域网 IP /
/// 主机名时绑 `0.0.0.0`，否则内置浏览器按那个 IP 连不上。
pub fn bind_addr(host: &str, port: u16) -> String {
    let (h, p) = normalize(host, port);
    if is_loopback(&h) {
        format!("127.0.0.1:{p}")
    } else {
        format!("0.0.0.0:{p}")
    }
}

fn normalize(host: &str, port: u16) -> (String, u16) {
    let h = host.trim();
    let h = if h.is_empty() { DEFAULT_SEED_HOST } else { h };
    let p = if port == 0 { DEFAULT_SEED_PORT } else { port };
    (h.to_string(), p)
}

fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

/// 起种子服务所需的配置。
#[derive(Clone)]
pub struct SeedServerConfig {
    /// 进程内接力队列（与 orchestrator / MITM 共享同一实例）。
    pub relay: RelayQueue,
    /// 接力是否开启（关了只给说明页）。
    pub relay_enabled: bool,
    /// 文章页停留区间（毫秒），与 MITM 注入一致（种子页本身不用它们，留作字段对齐）。
    pub relay_dwell_ms: i64,
    pub relay_dwell_max_ms: i64,
    /// 种子页打开后等多久跳到本批第一条任务链接（毫秒，固定值，下限 0；默认 100）。
    pub seed_dwell_ms: i64,
}

/// 渲染结果（抽出便于单测）。
#[derive(Debug, PartialEq, Eq)]
pub struct Rendered {
    pub html: String,
    /// 是否注入了接力脚本（同时已取队列首条为在途、记了心跳）。
    pub injected: bool,
}

/// 说明页骨架。`<meta name="referrer" content="no-referrer">`：跳到微信文章时不把本机地址作为来源发出去。
fn page(body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
<meta name=\"referrer\" content=\"no-referrer\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<title>MPider 接力入口</title>\
<style>body{{font:14px/1.6 system-ui,sans-serif;color:#333;padding:24px}}</style>\
</head><body>{body}</body></html>"
    )
}

/// 待命页：显示状态并用 `fetch` 链长轮询 [`WAIT_PATH`]，拿到链接即跳转。失败（服务重启中）退避 3 秒再试。
/// 文案给人看的只有一句，不解释原理；别关这个窗口是唯一要记住的事。
fn standby_page() -> String {
    let body = format!(
        "<p><b>MPider 待命中</b>：有采集任务会自动在这里打开，请别关闭这个窗口。</p>\
<p id=\"st\" style=\"color:#888\">正在连接…</p>\
<script>(function(){{var st=document.getElementById('st');\
function tick(){{fetch('{WAIT_PATH}',{{cache:'no-store'}}).then(function(r){{return r.json();}}).then(function(j){{\
if(j&&j.next){{st.textContent='收到任务，正在打开…';location.href=j.next;return;}}\
st.textContent='待命中 · 上次检查 '+new Date().toLocaleTimeString();tick();}}).catch(function(){{\
st.textContent='连接中断，3 秒后重试…';setTimeout(tick,3000);}});}}tick();}})();</script>"
    );
    page(&body)
}

/// 三个停留参数（毫秒）：种子页 → 首条固定等待，文章页随机区间。GUI 保存配置 / 运行开始时更新到常驻服务。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeedDwell {
    pub seed_dwell_ms: i64,
    pub relay_dwell_ms: i64,
    pub relay_dwell_max_ms: i64,
}

impl Default for SeedDwell {
    fn default() -> Self {
        Self {
            seed_dwell_ms: 100,
            relay_dwell_ms: 2500,
            relay_dwell_max_ms: 4000,
        }
    }
}

impl SeedServerConfig {
    /// 空闲配置：空队列 + 默认停留。常驻服务没有任务在跑时用它，只输出说明页。
    pub fn idle() -> Self {
        let d = SeedDwell::default();
        Self {
            relay: RelayQueue::new(),
            relay_enabled: true,
            relay_dwell_ms: d.relay_dwell_ms,
            relay_dwell_max_ms: d.relay_dwell_max_ms,
            seed_dwell_ms: d.seed_dwell_ms,
        }
    }

    fn set_dwell(&mut self, d: SeedDwell) {
        self.seed_dwell_ms = d.seed_dwell_ms;
        self.relay_dwell_ms = d.relay_dwell_ms;
        self.relay_dwell_max_ms = d.relay_dwell_max_ms;
    }

    /// 按请求头渲染种子页：过闸门则注入接力脚本（并取队列首条为在途、记心跳），否则只给说明页。
    ///
    /// 纯逻辑，便于脱网络单测；HTTP 收发见 [`start`]。
    pub fn render(&self, user_agent: Option<&str>, sec_fetch_dest: Option<&str>) -> Rendered {
        let wechat_ua = user_agent
            .map(|ua| ua.contains("MicroMessenger"))
            .unwrap_or(false);
        let is_document = sec_fetch_dest
            .map(|d| d.eq_ignore_ascii_case("document"))
            .unwrap_or(true);

        if !wechat_ua {
            return Rendered {
                html: page(
                    "<p>这是 MPider 的接力入口页。请把本页链接发到微信「文件传输助手」，\
由微信内置浏览器打开；在普通浏览器里打开不会做任何事。</p>",
                ),
                injected: false,
            };
        }
        if !self.relay_enabled || !is_document {
            return Rendered {
                html: page("<p>接力入口已就绪（当前没有待接力的任务）。</p>"),
                injected: false,
            };
        }
        // 与 MITM 入口页分支同序：先记心跳（证明"点种子拉起了浏览器"，看门狗点空计时复位），再取首条。
        self.relay.touch();
        let Some(next_url) = self.relay.bootstrap_next() else {
            // 人工模式：没任务时给待命页，页面长轮询等下一批；自动点击模式仍是纯说明页（见模块文档）。
            if resident_mode() {
                return Rendered {
                    html: standby_page(),
                    injected: false,
                };
            }
            return Rendered {
                html: page("<p>接力入口已就绪（当前没有待接力的任务）。</p>"),
                injected: false,
            };
        };
        // 种子页 → 首条用单独的固定等待（`seed_dwell_ms`，无 300 下限），之后各篇才按 `[relay_dwell_ms, relay_dwell_max_ms]` 随机。
        let script = relay::build_seed_script(Some(&next_url), self.seed_dwell_ms);
        let pending = self.relay.pending_count();
        info!(next = %next_url, pending, "种子入口页（本机服务）→ 注入接力跳到本批首条任务链接");
        applog::info(
            Stage::Capture,
            format!("种子入口页已在微信内置浏览器打开，注入接力脚本跳转到首条任务链接（队列 {pending} 条）"),
        );
        let html = relay::inject_into_html(&page("<p>正在打开任务链接…</p>"), &script);
        Rendered {
            html,
            injected: true,
        }
    }
}

/// 运行中的种子服务句柄。`shutdown` 触发关闭并等待任务结束。
pub struct SeedServer {
    /// 实际监听地址。
    pub addr: SocketAddr,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    join: tokio::task::JoinHandle<()>,
}

impl SeedServer {
    /// 关闭服务并等待接受循环退出（已建立的连接各自响应完即断）。
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.join.await;
    }
}

/// 在 `bind`（一般由 [`bind_addr`] 算出）起种子服务（配置固定；测试 / 老流程用）。常驻服务走 [`apply`]。
pub async fn start(cfg: SeedServerConfig, bind: &str) -> Result<SeedServer> {
    start_shared(Arc::new(RwLock::new(cfg)), bind).await
}

/// 起种子服务，配置放在共享槽里、每条请求现读——常驻服务靠它在不重启的情况下换接力队列 / 停留参数。
/// 端口被占等绑定失败直接报错：种子链接指向一个连不上的端口，整批接力都起不来。
async fn start_shared(shared: Arc<RwLock<SeedServerConfig>>, bind: &str) -> Result<SeedServer> {
    let listener = tokio::net::TcpListener::bind(bind).await.with_context(|| {
        format!("种子入口服务绑定 {bind} 失败（端口被占用？可在系统设置里改种子端口）")
    })?;
    let addr = listener.local_addr()?;
    let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();

    let join = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut rx => break,
                accepted = listener.accept() => {
                    let (stream, _peer) = match accepted {
                        Ok(v) => v,
                        Err(e) => {
                            error!(error = %e, "种子入口服务 accept 失败");
                            continue;
                        }
                    };
                    let shared = shared.clone();
                    tokio::spawn(async move {
                        let svc = service_fn(move |req: Request<hudsucker::hyper::body::Incoming>| {
                            let shared = shared.clone();
                            async move { Ok::<_, std::convert::Infallible>(respond(&shared, &req).await) }
                        });
                        // 种子页一次性打开，不保活：响应完即断，停服务时没有挂着的空闲连接。
                        if let Err(e) = http1::Builder::new()
                            .keep_alive(false)
                            .serve_connection(TokioIo::new(stream), svc)
                            .await
                        {
                            // 内置浏览器常在跳走时直接断连，属正常，只记 debug 级别。
                            tracing::debug!(error = %e, "种子入口服务连接结束");
                        }
                    });
                }
            }
        }
    });

    Ok(SeedServer {
        addr,
        shutdown_tx: Some(tx),
        join,
    })
}

/// 把一条请求变成响应：[`WAIT_PATH`] 是待命页的长轮询；`favicon.ico` 直接 204；其余任何 path 都当入口页
/// （对末尾斜杠 / 子路径健壮）。配置从共享槽现读（常驻服务换队列 / 停留不用重启）；每次入口页访问记进 [`HITS`]
/// 供 GUI 展示（长轮询不计）。
async fn respond<B>(
    shared: &Arc<RwLock<SeedServerConfig>>,
    req: &Request<B>,
) -> Response<Full<Bytes>> {
    if req.uri().path() == "/favicon.ico" {
        return Response::builder()
            .status(204)
            .header(header::CACHE_CONTROL, "no-store")
            .body(Full::new(Bytes::new()))
            .expect("static response");
    }
    let ua = req
        .headers()
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok());
    if req.uri().path() == WAIT_PATH {
        let next = wait_next(shared, ua).await;
        let body = match next {
            Some(u) => serde_json::json!({ "next": u }),
            None => serde_json::json!({ "next": null }),
        };
        return Response::builder()
            .status(200)
            .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-store")
            .body(Full::new(Bytes::from(body.to_string())))
            .expect("json response");
    }
    let dest = req
        .headers()
        .get("sec-fetch-dest")
        .and_then(|v| v.to_str().ok());
    let cfg = shared.read().unwrap_or_else(|e| e.into_inner()).clone();
    let rendered = cfg.render(ua, dest);
    {
        let mut h = HITS.lock().unwrap_or_else(|e| e.into_inner());
        h.total += 1;
        if rendered.injected {
            h.injected += 1;
        }
        h.last_at = Some(now());
        h.last_injected = Some(rendered.injected);
        h.last_wechat = Some(ua.map(|u| u.contains("MicroMessenger")).unwrap_or(false));
    }
    Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store, no-cache, must-revalidate")
        .header(header::PRAGMA, "no-cache")
        .header(header::EXPIRES, "0")
        .body(Full::new(Bytes::from(rendered.html)))
        .expect("html response")
}

/// 待命页长轮询：微信 UA 才算心跳（同机普通浏览器开着待命页不能冒充内置浏览器在线）；开关关着或非微信 UA
/// 立即返回空。否则最多挂 [`WAIT_HOLD_SECS`] 秒，期间每 300ms 看一次队列，有首条就取为在途、记接力心跳并交出去。
async fn wait_next(shared: &Arc<RwLock<SeedServerConfig>>, ua: Option<&str>) -> Option<String> {
    let wechat_ua = ua.map(|u| u.contains("MicroMessenger")).unwrap_or(false);
    if !wechat_ua || !resident_mode() {
        return None;
    }
    note_resident_poll();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(WAIT_HOLD_SECS);
    loop {
        let cfg = shared.read().unwrap_or_else(|e| e.into_inner()).clone();
        if cfg.relay_enabled && cfg.relay.pending_count() > 0 {
            cfg.relay.touch();
            if let Some(next) = cfg.relay.bootstrap_next() {
                let pending = cfg.relay.pending_count();
                info!(next = %next, pending, "待命页长轮询取到本批首条，交给内置浏览器跳转");
                applog::info(
                    Stage::Capture,
                    format!(
                        "微信内置浏览器待命页收到本批任务，自动跳转首条链接（队列 {pending} 条）"
                    ),
                );
                note_resident_poll();
                return Some(next);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            note_resident_poll();
            return None;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

// ============ 常驻服务（进程级单例） ============

/// 入口页访问统计（GUI 展示；常驻与非常驻都记）。
struct Hits {
    total: u64,
    injected: u64,
    last_at: Option<Epoch>,
    last_injected: Option<bool>,
    last_wechat: Option<bool>,
}

static HITS: Mutex<Hits> = Mutex::new(Hits {
    total: 0,
    injected: 0,
    last_at: None,
    last_injected: None,
    last_wechat: None,
});

/// 常驻服务的状态：共享配置槽 + 监听线程句柄。
struct Resident {
    /// 服务每条请求现读的配置（接力队列 / 停留参数）。
    shared: Arc<RwLock<SeedServerConfig>>,
    /// 当前监听的绑定地址（`None` = 没在跑）。
    bind: Option<String>,
    addr: Option<SocketAddr>,
    /// 种子链接（给 GUI 展示 / 日志）。
    url: String,
    /// 丢掉即通知线程退出。
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    last_error: Option<String>,
    started_at: Option<Epoch>,
}

static RESIDENT: LazyLock<Mutex<Resident>> = LazyLock::new(|| {
    Mutex::new(Resident {
        shared: Arc::new(RwLock::new(SeedServerConfig::idle())),
        bind: None,
        addr: None,
        url: String::new(),
        stop: None,
        thread: None,
        last_error: None,
        started_at: None,
    })
});

/// 单测串行锁：常驻服务是进程级单例，涉及它的测试要互斥。
#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

fn lock_resident() -> std::sync::MutexGuard<'static, Resident> {
    RESIDENT.lock().unwrap_or_else(|e| e.into_inner())
}

/// 常驻服务状态快照（GUI「种子链接」卡）。
#[derive(Clone, Debug, Serialize)]
pub struct SeedServerStatus {
    /// 是否在监听。
    pub running: bool,
    /// 实际监听地址（如 `127.0.0.1:8787`）。
    pub addr: Option<String>,
    /// 种子链接。
    pub url: String,
    /// 最近一次启动失败的原因（端口被占等）；在跑时为空。
    pub last_error: Option<String>,
    pub started_at: Option<Epoch>,
    /// 入口页累计访问次数 / 其中注入了接力脚本的次数。
    pub hits: u64,
    pub injected: u64,
    pub last_hit_at: Option<Epoch>,
    /// 最近一次访问是否注入了脚本 / 是否来自微信内置浏览器。
    pub last_hit_injected: Option<bool>,
    pub last_hit_wechat: Option<bool>,
    /// 当前接上的接力队列里待打开的条数（空闲时 0）。
    pub pending: usize,
    /// 待命页自轮询是否启用（人工模式）/ 待命页是否在线 / 最近一次长轮询时刻。
    pub resident_mode: bool,
    pub resident_alive: bool,
    pub resident_last_poll_at: Option<Epoch>,
}

/// **确保常驻服务按 host / 端口在跑**（幂等）：没在跑就起；已在同一地址上跑就只更新停留参数；地址变了先停旧的
/// 再起新的。绑定失败（端口被占）记 `last_error`、服务停在「未运行」并返回 Err，下次再调即重试。
/// 应用启动、保存配置、`runner::build_orchestrator` 都会调。
pub fn apply(host: &str, port: u16, dwell: SeedDwell) -> Result<SocketAddr> {
    apply_bind(&bind_addr(host, port), seed_url(host, port), dwell)
}

fn apply_bind(bind: &str, url: String, dwell: SeedDwell) -> Result<SocketAddr> {
    let mut r = lock_resident();
    r.shared
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .set_dwell(dwell);
    if r.thread.is_some() && r.bind.as_deref() == Some(bind) {
        r.url = url;
        return r
            .addr
            .ok_or_else(|| anyhow!("种子入口服务状态异常：在跑但没有监听地址"));
    }
    stop_locked(&mut r);

    let shared = r.shared.clone();
    let (ready_tx, ready_rx) =
        std::sync::mpsc::channel::<std::result::Result<SocketAddr, String>>();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let bind_owned = bind.to_string();
    let thread = std::thread::Builder::new()
        .name("seedserver".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("创建运行时失败：{e}")));
                    return;
                }
            };
            rt.block_on(async move {
                let srv = match start_shared(shared, &bind_owned).await {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = ready_tx.send(Err(format!("{e:#}")));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(srv.addr));
                // 等停止信号：发送端被丢弃（stop / 换绑）即返回。放到阻塞线程里等，不占运行时。
                let _ = tokio::task::spawn_blocking(move || stop_rx.recv()).await;
                srv.shutdown().await;
            });
        })
        .context("起种子入口服务线程失败")?;

    match ready_rx.recv() {
        Ok(Ok(addr)) => {
            r.thread = Some(thread);
            r.stop = Some(stop_tx);
            r.bind = Some(bind.to_string());
            r.addr = Some(addr);
            r.url = url.clone();
            r.last_error = None;
            r.started_at = Some(now());
            applog::info(
                Stage::Proxy,
                format!("种子入口服务已启动（常驻），监听 {addr}（种子链接 {url}）"),
            );
            Ok(addr)
        }
        Ok(Err(e)) => {
            drop(stop_tx);
            let _ = thread.join();
            r.last_error = Some(e.clone());
            r.url = url;
            applog::error(Stage::Proxy, format!("种子入口服务启动失败：{e}"));
            Err(anyhow!(e))
        }
        Err(_) => {
            drop(stop_tx);
            let _ = thread.join();
            let e = "种子入口服务线程未报告启动结果就退出了".to_string();
            r.last_error = Some(e.clone());
            r.url = url;
            applog::error(Stage::Proxy, e.clone());
            Err(anyhow!(e))
        }
    }
}

/// 停掉常驻服务（持锁版）：丢弃停止信号发送端让线程收尾，再等线程退出。
fn stop_locked(r: &mut Resident) {
    let was_running = r.thread.is_some();
    r.stop.take();
    if let Some(t) = r.thread.take() {
        let _ = t.join();
    }
    r.bind = None;
    r.addr = None;
    r.started_at = None;
    if was_running {
        applog::info(Stage::Proxy, "种子入口服务已停止".to_string());
    }
}

/// 停掉常驻服务（测试 / 退出用；正常运行不需要）。
pub fn stop() {
    let mut r = lock_resident();
    stop_locked(&mut r);
}

/// 一次运行开始：把本次的接力队列接到常驻服务上（每批 `capture_start` 都调，幂等）。
pub fn attach(relay: RelayQueue) {
    let r = lock_resident();
    r.shared.write().unwrap_or_else(|e| e.into_inner()).relay = relay;
}

/// 运行收尾：换回空队列，空闲时点开种子链接只给说明页，不会跳到上一批残留的链接。
pub fn detach() {
    let r = lock_resident();
    r.shared.write().unwrap_or_else(|e| e.into_inner()).relay = RelayQueue::new();
}

/// 状态快照（GUI）。
pub fn status() -> SeedServerStatus {
    let r = lock_resident();
    let h = HITS.lock().unwrap_or_else(|e| e.into_inner());
    let pending = r
        .shared
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .relay
        .pending_count();
    SeedServerStatus {
        running: r.thread.is_some(),
        addr: r.addr.map(|a| a.to_string()),
        url: r.url.clone(),
        last_error: r.last_error.clone(),
        started_at: r.started_at,
        hits: h.total,
        injected: h.injected,
        last_hit_at: h.last_at,
        last_hit_injected: h.last_injected,
        last_hit_wechat: h.last_wechat,
        pending,
        resident_mode: resident_mode(),
        resident_alive: resident_alive(),
        resident_last_poll_at: resident_last_poll_at(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WX_UA: &str =
        "Mozilla/5.0 (Windows NT 10.0; WOW64) AppleWebKit/537.36 Chrome/107 MicroMessenger/7.0.20";

    fn cfg(relay: &RelayQueue, enabled: bool) -> SeedServerConfig {
        SeedServerConfig {
            relay: relay.clone(),
            relay_enabled: enabled,
            relay_dwell_ms: 500,
            relay_dwell_max_ms: 900,
            seed_dwell_ms: 400,
        }
    }

    /// 测试用：向已起的服务发一条 GET，返回整段响应文本（状态行 + 头 + 体）。
    fn http_get(addr: SocketAddr, path: &str, ua: &str) -> String {
        use std::io::{Read, Write};
        let mut c = std::net::TcpStream::connect(addr).unwrap();
        write!(
            c,
            "GET {path} HTTP/1.1\r\nHost: x\r\nUser-Agent: {ua}\r\nSec-Fetch-Dest: document\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut out = String::new();
        c.read_to_string(&mut out).unwrap();
        out
    }

    /// 待命页自轮询（人工模式）：空闲时微信 UA 得到待命页（不注入接力）；非微信 UA 的长轮询立即得空、不算在线；
    /// 微信 UA 的长轮询挂起期间接上队列即拿到首条（首条记在途、记接力心跳、待命页在线）；关开关后回到说明页。
    #[test]
    fn test_resident_standby_and_wait() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        stop();
        reset_resident_poll();
        set_resident_mode(true);
        let addr = apply_bind(
            "127.0.0.1:0",
            "http://127.0.0.1:0/".into(),
            SeedDwell::default(),
        )
        .unwrap();

        let idle = http_get(addr, "/", WX_UA);
        assert!(
            idle.contains("待命中") && idle.contains(WAIT_PATH),
            "{idle}"
        );
        assert!(!relay::already_injected(Some(&idle)));
        assert!(!resident_alive());

        let chrome = http_get(addr, WAIT_PATH, "Mozilla/5.0 Chrome/120");
        assert!(chrome.contains("\"next\":null"), "{chrome}");
        assert!(!resident_alive());

        let relay = RelayQueue::new();
        relay.set(&["https://mp.weixin.qq.com/s/a".to_string()]);
        let poller = std::thread::spawn(move || http_get(addr, WAIT_PATH, WX_UA));
        std::thread::sleep(Duration::from_millis(700));
        assert!(resident_alive(), "长轮询一到就算在线");
        attach(relay.clone());
        let got = poller.join().unwrap();
        assert!(got.contains("https://mp.weixin.qq.com/s/a"), "{got}");
        assert_eq!(
            relay.inflight().as_deref(),
            Some("https://mp.weixin.qq.com/s/a")
        );
        assert!(relay.last_request_at().is_some());
        let st = status();
        assert!(st.resident_mode && st.resident_alive && st.resident_last_poll_at.is_some());

        set_resident_mode(false);
        assert!(!resident_alive());
        detach();
        assert!(http_get(addr, "/", WX_UA).contains("当前没有待接力的任务"));
        stop();
        reset_resident_poll();
    }

    #[test]
    fn test_seed_url_and_bind() {
        assert_eq!(seed_url("", 0), "http://127.0.0.1:8787/");
        assert_eq!(seed_url("192.0.2.10", 8787), "http://192.0.2.10:8787/");
        assert_eq!(seed_url(" localhost ", 9000), "http://localhost:9000/");
        // 回环只绑本机；局域网 IP / 主机名绑全部网卡
        assert_eq!(bind_addr("127.0.0.1", 8787), "127.0.0.1:8787");
        assert_eq!(bind_addr("localhost", 8787), "127.0.0.1:8787");
        assert_eq!(bind_addr("::1", 8787), "127.0.0.1:8787");
        assert_eq!(bind_addr("192.0.2.10", 8787), "0.0.0.0:8787");
        assert_eq!(bind_addr("mybox.lan", 0), "0.0.0.0:8787");
    }

    #[test]
    fn test_render_injects_for_wechat_document() {
        let relay = RelayQueue::new();
        relay.set(&[
            "https://mp.weixin.qq.com/s/a".to_string(),
            "https://mp.weixin.qq.com/s/b".to_string(),
        ]);
        let c = cfg(&relay, true);
        assert!(relay.last_request_at().is_none());

        let r = c.render(Some(WX_UA), Some("document"));
        assert!(r.injected);
        assert!(relay::already_injected(Some(&r.html)));
        assert!(r.html.contains("https://mp.weixin.qq.com/s/a"));
        assert!(r.html.contains("no-referrer"));
        // 种子页 → 首条用固定的 seed_dwell_ms（400），不是文章页的 [500, 900] 随机区间
        assert!(r.html.contains("var d=400;"));
        assert!(!r.html.contains("var d=500+"));
        // 首条记在途、不标已打开；心跳已记
        assert_eq!(relay.pending_count(), 2);
        assert_eq!(
            relay.inflight(),
            Some("https://mp.weixin.qq.com/s/a".to_string())
        );
        assert!(relay.last_request_at().is_some());
    }

    #[test]
    fn test_render_gates() {
        let relay = RelayQueue::new();
        relay.set(&["https://mp.weixin.qq.com/s/a".to_string()]);

        // 同机 Chrome：说明页、不消耗队列、不记心跳
        let r = cfg(&relay, true).render(Some("Mozilla/5.0 Chrome/120"), Some("document"));
        assert!(!r.injected);
        assert!(!relay::already_injected(Some(&r.html)));
        assert!(relay.inflight().is_none());
        assert!(relay.last_request_at().is_none());

        // iframe 不注入
        let r = cfg(&relay, true).render(Some(WX_UA), Some("iframe"));
        assert!(!r.injected);
        assert!(relay.inflight().is_none());

        // 接力关着不注入
        let r = cfg(&relay, false).render(Some(WX_UA), Some("document"));
        assert!(!r.injected);

        // 没带 Sec-Fetch-Dest 的老内核视为主文档
        let r = cfg(&relay, true).render(Some(WX_UA), None);
        assert!(r.injected);

        // 队列空：说明页
        let empty = RelayQueue::new();
        let r = cfg(&empty, true).render(Some(WX_UA), Some("document"));
        assert!(!r.injected);
        assert!(r.html.contains("没有待接力的任务"));
    }

    #[tokio::test]
    async fn test_server_roundtrip() {
        let relay = RelayQueue::new();
        relay.set(&["https://mp.weixin.qq.com/s/a".to_string()]);
        let srv = start(cfg(&relay, true), "127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", srv.addr);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();

        // 微信 UA、主文档：注入 + no-store
        let res = client
            .get(format!("{base}/anything?x=1"))
            .header("User-Agent", WX_UA)
            .header("Sec-Fetch-Dest", "document")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        assert!(res
            .headers()
            .get("cache-control")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("no-store"));
        let html = res.text().await.unwrap();
        assert!(relay::already_injected(Some(&html)));
        assert_eq!(
            relay.inflight(),
            Some("https://mp.weixin.qq.com/s/a".to_string())
        );

        // favicon 204
        let res = client
            .get(format!("{base}/favicon.ico"))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 204);

        // 停服务后新连接被拒（用新 client，避开连接池里可能残留的旧连接）。
        srv.shutdown().await;
        let fresh = reqwest::Client::builder().no_proxy().build().unwrap();
        assert!(fresh.get(base).send().await.is_err());
    }
    /// 常驻服务：起 → 同地址幂等 → 接上队列后微信 UA 访问注入 → 摘下后只给说明页 → 停。
    #[test]
    fn test_resident_apply_attach_detach_stop() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        stop();
        let addr = apply_bind(
            "127.0.0.1:0",
            "http://127.0.0.1:0/".into(),
            SeedDwell::default(),
        )
        .expect("绑定临时端口");
        assert_ne!(addr.port(), 0);
        let st = status();
        assert!(st.running);
        assert_eq!(st.addr.as_deref(), Some(addr.to_string().as_str()));
        assert!(st.last_error.is_none());
        // 同一绑定地址再 apply：不重启，地址不变；停留参数更新
        let d = SeedDwell {
            seed_dwell_ms: 700,
            relay_dwell_ms: 500,
            relay_dwell_max_ms: 900,
        };
        let again = apply_bind("127.0.0.1:0", "http://127.0.0.1:0/".into(), d).unwrap();
        // 注意：这里传的 bind 字符串与首次相同（"127.0.0.1:0"），按字符串比对视为同地址
        assert_eq!(again, addr);

        let get = |ua: &str| -> String {
            use std::io::{Read, Write};
            let mut c = std::net::TcpStream::connect(addr).unwrap();
            write!(
                c,
                "GET / HTTP/1.1\r\nHost: x\r\nUser-Agent: {ua}\r\nSec-Fetch-Dest: document\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            let mut out = String::new();
            c.read_to_string(&mut out).unwrap();
            out
        };
        // 空闲：微信 UA 也只给说明页
        let idle = get(WX_UA);
        assert!(idle.contains("当前没有待接力的任务"), "{idle}");
        assert!(!relay::already_injected(Some(&idle)));

        // 接上队列：注入脚本、用固定的种子等待
        let relay = RelayQueue::new();
        relay.set(&["https://mp.weixin.qq.com/s/a".to_string()]);
        attach(relay.clone());
        assert_eq!(status().pending, 1);
        let hot = get(WX_UA);
        assert!(relay::already_injected(Some(&hot)), "{hot}");
        assert!(hot.contains("var d=700;"));
        assert_eq!(
            relay.inflight().as_deref(),
            Some("https://mp.weixin.qq.com/s/a")
        );
        let st = status();
        assert!(st.hits >= 2 && st.injected >= 1);
        assert_eq!(st.last_hit_injected, Some(true));
        assert_eq!(st.last_hit_wechat, Some(true));

        // 摘下：回到说明页，队列条数 0
        detach();
        assert_eq!(status().pending, 0);
        assert!(get(WX_UA).contains("当前没有待接力的任务"));

        stop();
        let st = status();
        assert!(!st.running);
        assert!(st.addr.is_none());
        assert!(std::net::TcpStream::connect(addr).is_err());
    }

    /// 端口被占：apply 报错、记 last_error、不在跑；释放后再 apply 成功。
    #[test]
    fn test_resident_bind_failure_recorded() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        stop();
        let holder = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let taken = holder.local_addr().unwrap().to_string();
        let err = apply_bind(&taken, format!("http://{taken}/"), SeedDwell::default())
            .expect_err("端口被占应报错");
        assert!(err.to_string().contains("绑定"), "{err}");
        let st = status();
        assert!(!st.running);
        assert!(st.last_error.as_deref().unwrap_or("").contains("绑定"));
        drop(holder);
        apply_bind(&taken, format!("http://{taken}/"), SeedDwell::default()).expect("释放后可绑");
        assert!(status().running);
        stop();
    }
}
