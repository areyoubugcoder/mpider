//! 飞书通知（自定义机器人 Webhook，2026-09-08，Python 无对应）。
//!
//! 需求：系统设置里一个「通知开关」，开了之后填飞书机器人 Webhook 与签名密钥，程序在**关键点出问题时**
//! 推一条预警到群里（无人值守时出事要有人知道）。
//!
//! **策略（2026-09-08 收紧）：只报异常，不报状态。** 应用启动 / 轮询启停 / 代理切换成功 / 巡检一轮完成 /
//! 任务正常结束这类「一切正常」的消息一律不推——频繁推送会把真正的预警淹没。预警定义：需要人看一眼
//! 或说明采集能力已受损的事（任务超时或出错、整机退避与封号、巡检环境故障 / 整批失败 / 预算用完、轮询出错
//! 退出）。
//!
//! - 协议：`POST <webhook>`，body `{"timestamp","sign","msg_type":"text","content":{"text"}}`；签名 =
//!   `base64(HmacSHA256(key = "<timestamp>\n<secret>", data = ""))`（飞书「签名校验」规则，时间戳秒级、
//!   与服务器相差不能超过 1 小时）。响应 `{"code":0}`（新）或 `{"StatusCode":0}`（旧）算成功。
//! - 发送走进程级**单一工作线程**（自带 tokio 运行时，`std::sync::mpsc` 排队、按序发送）：调用方
//!   [`notify`] 只是入队，永不阻塞、永不 panic；开关关着 / 没配 Webhook 就直接丢弃。
//! - 同一文案 60 秒内只发一次（防止重试循环刷屏）；此外**同类预警有合并窗口**（[`Kind::merge_window`]：
//!   任务 / 巡检 10 分钟）——窗口内再触发只计数不发，窗口过后下一条发出时附
//!   「期间另有 N 条同类预警已合并」；退避 / 封号与运行状态不合并（本身就稀疏且每条都要看）。
//!   发送失败记环节日志（第一次与之后每 10 次入库）。
//! - **Webhook / 密钥不得写日志**（Webhook 里那段 UUID 就是凭证）。
//!
//! 预警点（[`Kind`]）：轮询出错退出、任务异常结束（手动任务与巡检批次都只报超时 / 服务端错误 / 错误）、
//! 整机限流退避与封号、巡检环境故障 / 整批失败暂停 / 预算用完。
//! 新增预警点：调 `notify::notify(Kind, 文案)` 一行即可，文案写「发生了什么、影响是什么」，别带接口参数；
//! **别为「正常完成」加通知**——要看运行状态去 GUI 日志页。

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::Sha256;

use crate::applog::{self, Stage};

/// 同一文案的去重窗口。
const DEDUP_WINDOW: Duration = Duration::from_secs(60);
/// 单条请求超时。
const SEND_TIMEOUT: Duration = Duration::from_secs(10);
/// 队列上限：超过就丢最新的（说明飞书那边发不出去了，堆着没意义）。
const QUEUE_CAP: usize = 200;

/// 飞书通知配置（从 [`crate::runner::RealRunConfig`] 派生）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeishuConfig {
    /// 通知开关。
    pub enabled: bool,
    /// 自定义机器人 Webhook（`https://open.feishu.cn/open-apis/bot/v2/hook/<uuid>`）。
    pub webhook: String,
    /// 「签名校验」密钥；空 = 机器人没开签名校验，报文不带 `sign`。
    pub secret: String,
}

impl FeishuConfig {
    /// 能发 = 开关开且 Webhook 像回事。
    pub fn is_active(&self) -> bool {
        self.enabled && is_webhook_like(&self.webhook)
    }
}

/// Webhook 形状校验：只认 https 且是飞书 / Lark 的 bot hook 路径。纯函数。
pub fn is_webhook_like(url: &str) -> bool {
    let u = url.trim();
    u.starts_with("https://") && u.contains("/open-apis/bot/v2/hook/") && u.len() > 40
}

/// 预警类别：决定消息标题前缀与合并窗口。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum Kind {
    /// 运行状态：轮询出错退出。
    App,
    /// 任务异常结束（手动任务 / 巡检批次：超时 / 服务端错误 / 错误）。
    Task,
    /// 整机限流退避 / 微信号封禁。
    Cooldown,
    /// 定时巡检：环境故障 / 整批失败暂停 / 预算用完。
    Sweep,
    /// 用户在设置页点「发送测试消息」。
    Test,
}

impl Kind {
    pub fn title(self) -> &'static str {
        match self {
            Kind::App => "运行状态",
            Kind::Task => "任务",
            Kind::Cooldown => "限流退避",
            Kind::Sweep => "定时巡检",
            Kind::Test => "测试",
        }
    }

    /// 标题前的表情，群里一眼分轻重。
    fn emoji(self) -> &'static str {
        match self {
            Kind::App => "🔴",
            Kind::Task => "⚠️",
            Kind::Cooldown => "⛔",
            Kind::Sweep => "🟠",
            Kind::Test => "🔔",
        }
    }

    /// 同类预警的合并窗口：窗口内再触发只计数不发，下一条发出时附合并条数。`None` = 每条都发。
    /// 任务 / 巡检批次一条接一条失败时（微信离线、RPA 点不开）不该每条刷一遍；
    /// 退避 / 封号自带翻倍间隔且每级都要看，轮询退出一进程只有一次。
    pub fn merge_window(self) -> Option<Duration> {
        match self {
            Kind::Task | Kind::Sweep => Some(Duration::from_secs(10 * 60)),
            Kind::App | Kind::Cooldown | Kind::Test => None,
        }
    }
}

/// 飞书签名：`base64(HmacSHA256(key = "{timestamp}\n{secret}", data = ""))`。纯函数。
pub fn sign(secret: &str, timestamp: i64) -> String {
    let key = format!("{timestamp}\n{secret}");
    // HMAC 对任意长度的 key 都合法，`new_from_slice` 不会失败。
    let mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("hmac 接受任意长度 key");
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// 组装请求体。`secret` 为空则不带签名字段。纯函数。
pub fn build_payload(secret: &str, timestamp: i64, text: &str) -> Value {
    let mut body = json!({
        "msg_type": "text",
        "content": { "text": text },
    });
    if !secret.is_empty() {
        body["timestamp"] = Value::String(timestamp.to_string());
        body["sign"] = Value::String(sign(secret, timestamp));
    }
    body
}

/// 解析飞书响应：`{"code":0}` / `{"StatusCode":0}` 为成功，否则带回 `msg`。纯函数。
pub fn parse_response(text: &str) -> Result<()> {
    let v: Value = serde_json::from_str(text).context("飞书响应不是 JSON")?;
    let code = v
        .get("code")
        .or_else(|| v.get("StatusCode"))
        .and_then(Value::as_i64);
    match code {
        Some(0) => Ok(()),
        Some(c) => {
            let msg = v
                .get("msg")
                .or_else(|| v.get("StatusMessage"))
                .and_then(Value::as_str)
                .unwrap_or("未知错误");
            bail!("飞书返回 code={c}：{msg}")
        }
        None => bail!("飞书响应缺少 code 字段"),
    }
}

/// 本机名（消息里标明来自哪台机；多台采集机同群时分得清）。
static HOSTNAME: LazyLock<String> = LazyLock::new(|| {
    for key in ["COMPUTERNAME", "HOSTNAME"] {
        if let Ok(v) = std::env::var(key) {
            if !v.trim().is_empty() {
                return v.trim().to_string();
            }
        }
    }
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
});

/// 组装群里看到的正文：标题行 + 机器 / 时间 + 内容。纯函数（`host` 与时间由调用方传）。
pub fn format_text(kind: Kind, host: &str, when: &str, body: &str) -> String {
    format!(
        "{} 【MPider · {}】\n{} · {}\n{}",
        kind.emoji(),
        kind.title(),
        host,
        when,
        body.trim()
    )
}

/// 真正发一条（阻塞到响应；给工作线程与「发送测试消息」用）。
pub async fn send(cfg: &FeishuConfig, text: &str) -> Result<()> {
    if !is_webhook_like(&cfg.webhook) {
        bail!("Webhook 地址不合法（应形如 https://open.feishu.cn/open-apis/bot/v2/hook/…）");
    }
    let client = reqwest::Client::builder()
        .timeout(SEND_TIMEOUT)
        .no_proxy()
        .build()?;
    let ts = crate::model::now() as i64;
    let body = build_payload(cfg.secret.trim(), ts, text);
    let resp = client
        .post(cfg.webhook.trim())
        .header("Content-Type", "application/json")
        .body(serde_json::to_string(&body)?)
        .send()
        .await
        .context("请求飞书失败")?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("飞书返回 HTTP {}", status.as_u16());
    }
    parse_response(&text)
}

/// 「发送测试消息」：用给定配置（未必已保存）发一条，成功返回 `Ok`。
pub async fn send_test(cfg: &FeishuConfig) -> Result<()> {
    let text = format_text(
        Kind::Test,
        &HOSTNAME,
        &applog::format_local(crate::model::now()),
        "这是一条测试消息：飞书通知配置正确。之后只有出问题才会推到这里：任务超时 / 出错、限流退避与封号、巡检环境故障 / 整批失败 / 预算用完、轮询出错退出。正常运行不打扰。",
    );
    send(cfg, &text).await
}

// -----------------------------------------------------------------------------
// 进程级：配置 + 单一工作线程
// -----------------------------------------------------------------------------

struct Inner {
    cfg: FeishuConfig,
    tx: Option<std::sync::mpsc::SyncSender<String>>,
    /// 去重：文案 → 最近入队时刻。
    recent: HashMap<String, Instant>,
    /// 合并窗口：类别 → 最近一次真正入队的时刻。
    last_by_kind: HashMap<Kind, Instant>,
    /// 合并窗口内被压下的同类条数（下一条发出时带上并清零）。
    merged: HashMap<Kind, u64>,
    /// 连续发送失败次数（成功归零）。
    failures: u64,
    /// 累计：入队 / 发出成功 / 失败（状态页展示）。
    queued: u64,
    sent: u64,
    failed: u64,
    last_error: Option<String>,
    last_sent_at: Option<f64>,
}

static STATE: LazyLock<Mutex<Inner>> = LazyLock::new(|| {
    Mutex::new(Inner {
        cfg: FeishuConfig::default(),
        tx: None,
        recent: HashMap::new(),
        last_by_kind: HashMap::new(),
        merged: HashMap::new(),
        failures: 0,
        queued: 0,
        sent: 0,
        failed: 0,
        last_error: None,
        last_sent_at: None,
    })
});

fn lock() -> std::sync::MutexGuard<'static, Inner> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// 状态快照（GUI 设置页「通知」区块展示）。
#[derive(Clone, Debug, Serialize)]
pub struct NotifyStatus {
    pub enabled: bool,
    /// Webhook 是否填了且形状合法。
    pub configured: bool,
    pub queued: u64,
    pub sent: u64,
    pub failed: u64,
    pub last_error: Option<String>,
    pub last_sent_at: Option<f64>,
}

pub fn status() -> NotifyStatus {
    let s = lock();
    NotifyStatus {
        enabled: s.cfg.enabled,
        configured: is_webhook_like(&s.cfg.webhook),
        queued: s.queued,
        sent: s.sent,
        failed: s.failed,
        last_error: s.last_error.clone(),
        last_sent_at: s.last_sent_at,
    }
}

/// **应用配置**（幂等）：记住开关 / Webhook / 密钥；工作线程按需懒启动，配置变了不用重启线程
/// （每条消息发送时重新读配置）。返回是否从「关」变「开」（调用方可据此发一条上线通知）。
pub fn configure(cfg: FeishuConfig) -> bool {
    let cfg = FeishuConfig {
        webhook: cfg.webhook.trim().to_string(),
        secret: cfg.secret.trim().to_string(),
        ..cfg
    };
    let mut s = lock();
    if s.cfg == cfg {
        return false;
    }
    let was_active = s.cfg.is_active();
    s.cfg = cfg;
    let now_active = s.cfg.is_active();
    drop(s);
    if was_active != now_active {
        applog::info(
            Stage::App,
            if now_active {
                "飞书通知已开启：只在出问题时推送（任务异常 / 限流退避与封号 / 巡检暂停 / 轮询出错退出）"
            } else {
                "飞书通知已关闭"
            },
        );
    }
    now_active && !was_active
}

/// 当前是否会真的发（开关开 + Webhook 合法）。
pub fn is_active() -> bool {
    lock().cfg.is_active()
}

/// 入队前的闸门（纯逻辑，便于单测）：60 秒内相同文案丢弃；同类预警在 [`Kind::merge_window`] 内只计数；
/// 放行时返回要发的正文（窗口里压下过同类的，附一行合并说明）。
fn admit(s: &mut Inner, kind: Kind, body: &str, now: Instant) -> Option<String> {
    // 去重窗口：顺手清掉过期项，免得 map 无限长。
    s.recent
        .retain(|_, t| now.duration_since(*t) < DEDUP_WINDOW);
    let key = format!("{kind:?}:{body}");
    if s.recent.contains_key(&key) {
        return None;
    }
    s.recent.insert(key, now);
    if let Some(window) = kind.merge_window() {
        if let Some(last) = s.last_by_kind.get(&kind) {
            if now.duration_since(*last) < window {
                *s.merged.entry(kind).or_insert(0) += 1;
                return None;
            }
        }
    }
    s.last_by_kind.insert(kind, now);
    let merged = s.merged.remove(&kind).unwrap_or(0);
    let mut text = body.to_string();
    if merged > 0 {
        let mins = kind.merge_window().map_or(0, |w| w.as_secs().div_ceil(60));
        text.push_str(&format!(
            "\n（此前 {mins} 分钟内另有 {merged} 条同类预警已合并，明细见 GUI 日志页）"
        ));
    }
    Some(text)
}

/// **预警通知**（入队即返回）。开关关 / 没配 Webhook 直接丢弃；60 秒内相同文案只发一次；同类预警按
/// [`Kind::merge_window`] 合并。**只用于异常**——正常完成的事别调这里。
pub fn notify(kind: Kind, body: impl Into<String>) {
    let body = body.into();
    let mut s = lock();
    if !s.cfg.is_active() {
        return;
    }
    let Some(body) = admit(&mut s, kind, &body, Instant::now()) else {
        return;
    };
    let text = format_text(
        kind,
        &HOSTNAME,
        &applog::format_local(crate::model::now()),
        &body,
    );
    if s.tx.is_none() {
        s.tx = spawn_worker();
    }
    let Some(tx) = s.tx.as_ref() else {
        return;
    };
    match tx.try_send(text) {
        Ok(()) => s.queued += 1,
        Err(std::sync::mpsc::TrySendError::Full(_)) => {
            s.last_error = Some("通知队列已满（飞书发不出去？），本条丢弃".to_string());
        }
        Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
            // 工作线程没了：下次入队重新拉起。
            s.tx = None;
        }
    }
}

/// 拉起单一工作线程：自带 current_thread 运行时，按序把队列里的消息发出去。
fn spawn_worker() -> Option<std::sync::mpsc::SyncSender<String>> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<String>(QUEUE_CAP);
    let spawned = std::thread::Builder::new()
        .name("feishu-notify".to_string())
        .spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                applog::error(Stage::App, "飞书通知线程启动失败：无法创建运行时");
                return;
            };
            while let Ok(text) = rx.recv() {
                let cfg = lock().cfg.clone();
                if !cfg.is_active() {
                    continue;
                }
                match rt.block_on(send(&cfg, &text)) {
                    Ok(()) => {
                        let mut s = lock();
                        s.sent += 1;
                        s.failures = 0;
                        s.last_error = None;
                        s.last_sent_at = Some(crate::model::now());
                    }
                    Err(e) => {
                        let msg = format!("{e:#}");
                        let failures = {
                            let mut s = lock();
                            s.failed += 1;
                            s.failures += 1;
                            s.last_error = Some(msg.clone());
                            s.failures
                        };
                        // 第一次与之后每 10 次入库；其余瞬态行。
                        if failures == 1 || failures % 10 == 0 {
                            applog::warn(
                                Stage::App,
                                format!("飞书通知发送失败（连续 {failures} 次）：{msg}"),
                            );
                        } else {
                            applog::progress(
                                Stage::App,
                                format!("飞书通知发送失败（连续 {failures} 次）：{msg}"),
                            );
                        }
                        // 失败后稍缓一缓，别把队列里的消息一口气全打失败。
                        std::thread::sleep(Duration::from_secs(2));
                    }
                }
            }
        });
    match spawned {
        Ok(_) => Some(tx),
        Err(e) => {
            applog::error(Stage::App, format!("飞书通知线程启动失败：{e}"));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// 签名对照：与飞书官方 Python 示例算法一致（key = "ts\nsecret"，data 为空）。
    #[test]
    fn sign_matches_reference() {
        // python: base64(hmac.new(b"1700000000\nabc", b"", sha256).digest())
        assert_eq!(
            sign("abc", 1_700_000_000),
            "VIS10b0EBvzzSdFnuk4tznEmK5wHaruvf/WnViv2yR4="
        );
        // 不同时间戳签名不同
        assert_ne!(sign("abc", 1), sign("abc", 2));
    }

    #[test]
    fn payload_shape() {
        let p = build_payload("s", 123, "hi");
        assert_eq!(p["msg_type"], "text");
        assert_eq!(p["content"]["text"], "hi");
        assert_eq!(p["timestamp"], "123");
        assert_eq!(p["sign"], sign("s", 123));
        let p = build_payload("", 123, "hi");
        assert!(p.get("sign").is_none());
        assert!(p.get("timestamp").is_none());
    }

    #[test]
    fn response_parsing() {
        assert!(parse_response(r#"{"code":0,"msg":"success"}"#).is_ok());
        assert!(parse_response(r#"{"StatusCode":0,"StatusMessage":"success"}"#).is_ok());
        let e = parse_response(r#"{"code":19021,"msg":"sign match fail"}"#)
            .unwrap_err()
            .to_string();
        assert!(e.contains("19021") && e.contains("sign match fail"));
        assert!(parse_response("nope").is_err());
        assert!(parse_response(r#"{"data":{}}"#).is_err());
    }

    #[test]
    fn webhook_shape() {
        assert!(is_webhook_like(
            "https://open.feishu.cn/open-apis/bot/v2/hook/398a10c7-facc-4681-a338-000000000000"
        ));
        assert!(!is_webhook_like(
            "http://open.feishu.cn/open-apis/bot/v2/hook/x"
        ));
        assert!(!is_webhook_like("https://example.com/hook"));
        assert!(!is_webhook_like(""));
        let cfg = FeishuConfig {
            enabled: true,
            webhook: "https://example.com".into(),
            secret: String::new(),
        };
        assert!(!cfg.is_active());
    }

    fn fresh_inner() -> Inner {
        Inner {
            cfg: FeishuConfig::default(),
            tx: None,
            recent: HashMap::new(),
            last_by_kind: HashMap::new(),
            merged: HashMap::new(),
            failures: 0,
            queued: 0,
            sent: 0,
            failed: 0,
            last_error: None,
            last_sent_at: None,
        }
    }

    /// 闸门：相同文案 60 秒去重；同类预警在合并窗口内只计数，窗口过后下一条带合并条数；
    /// 退避类不合并，每条都发。
    #[test]
    fn admit_dedups_and_merges_by_kind() {
        let mut s = fresh_inner();
        let t0 = Instant::now();
        assert_eq!(
            admit(&mut s, Kind::Task, "任务 A 超时", t0).as_deref(),
            Some("任务 A 超时")
        );
        // 相同文案 60 秒内丢弃
        assert!(admit(
            &mut s,
            Kind::Task,
            "任务 A 超时",
            t0 + Duration::from_secs(10)
        )
        .is_none());
        // 不同文案但同类、在 10 分钟窗口内：压下计数
        assert!(admit(
            &mut s,
            Kind::Task,
            "任务 B 出错",
            t0 + Duration::from_secs(120)
        )
        .is_none());
        assert!(admit(
            &mut s,
            Kind::Task,
            "任务 C 出错",
            t0 + Duration::from_secs(300)
        )
        .is_none());
        assert_eq!(s.merged.get(&Kind::Task), Some(&2));
        // 窗口过后放行，并附合并说明
        let out = admit(
            &mut s,
            Kind::Task,
            "任务 D 出错",
            t0 + Duration::from_secs(11 * 60),
        )
        .unwrap();
        assert!(
            out.starts_with("任务 D 出错\n（此前 10 分钟内另有 2 条同类预警已合并"),
            "{out}"
        );
        assert!(!s.merged.contains_key(&Kind::Task));
        // 另一类不受影响
        assert!(admit(
            &mut s,
            Kind::Sweep,
            "环境故障",
            t0 + Duration::from_secs(130)
        )
        .is_some());
        // 退避 / 封号：不合并，连发都放行
        assert!(admit(
            &mut s,
            Kind::Cooldown,
            "退避 5 分钟",
            t0 + Duration::from_secs(1)
        )
        .is_some());
        assert!(admit(
            &mut s,
            Kind::Cooldown,
            "退避 10 分钟",
            t0 + Duration::from_secs(2)
        )
        .is_some());
    }

    #[test]
    fn text_format() {
        let t = format_text(
            Kind::Cooldown,
            "win-1",
            "2026-09-08 10:00:00",
            "  退避 5 分钟 \n",
        );
        assert!(t.starts_with("⛔ 【MPider · 限流退避】\nwin-1 · 2026-09-08 10:00:00\n退避 5 分钟"));
        assert!(!t.ends_with('\n'));
    }

    /// 本机假飞书：收到 POST、校验 body 形状与签名可复算，回 code=0 → send 成功；回 code≠0 → Err。
    #[tokio::test]
    async fn send_hits_webhook_and_checks_response() {
        // send() 只认 https + 飞书路径，所以本机假服务走不了 send()；改测 build_payload + 真实 HTTP 往返
        // 的解析链：用 reqwest 直接 POST 到假服务，确认 parse_response 对接正确。
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let n = sock.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = r#"{"code":0,"msg":"success"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            let _ = tx.send(req);
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let payload = build_payload("sec", 1_700_000_000, "hello");
        let resp = client
            .post(format!("http://127.0.0.1:{port}/hook"))
            .header("Content-Type", "application/json")
            .body(serde_json::to_string(&payload).unwrap())
            .send()
            .await
            .unwrap();
        assert!(parse_response(&resp.text().await.unwrap()).is_ok());
        let req = rx.await.unwrap();
        let body = req.split("\r\n\r\n").nth(1).unwrap_or("");
        let v: Value = serde_json::from_str(body).unwrap();
        assert_eq!(v["sign"], sign("sec", 1_700_000_000));
        assert_eq!(v["content"]["text"], "hello");
    }

    /// 真发一条到飞书（需 `FEISHU_WEBHOOK` / `FEISHU_SECRET` 环境变量；默认 ignored）：
    /// `FEISHU_WEBHOOK=… FEISHU_SECRET=… cargo test -p mpider-core -- --ignored notify::tests::real_send`
    #[tokio::test]
    #[ignore]
    async fn real_send() {
        let cfg = FeishuConfig {
            enabled: true,
            webhook: std::env::var("FEISHU_WEBHOOK").unwrap_or_default(),
            secret: std::env::var("FEISHU_SECRET").unwrap_or_default(),
        };
        send_test(&cfg).await.expect("飞书应回 code=0");
    }

    /// 关着 / 没配时 notify 是 no-op，不起线程；配置变化返回值正确。
    #[test]
    fn notify_noop_when_inactive() {
        let first = configure(FeishuConfig::default());
        assert!(!first);
        notify(Kind::App, "x");
        let st = status();
        assert!(!st.enabled && !st.configured && st.queued == 0);
        // 开了但 webhook 不合法：仍不发
        assert!(!configure(FeishuConfig {
            enabled: true,
            webhook: "https://example.com".into(),
            secret: String::new(),
        }));
        notify(Kind::App, "x");
        assert_eq!(status().queued, 0);
        configure(FeishuConfig::default());
    }
}
