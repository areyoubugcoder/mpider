//! 环节日志总线（applog）—— 整链每个环节（任务 / 代理 / RPA / 凭证 / 列表 / 续期 / 回报 /
//! 正文 / 应用）的步骤日志统一从这里走：**一份事件，三个去向**——
//!
//! 1. 订阅者（GUI 事件推送 / CLI 打印）：[`LogBus::subscribe`]；
//! 2. SQLite `app_log` 表（**关键节点入库**，只保留近 3 天，便于「一键上报」拿去分析）：
//!    [`LogBus::set_store`]；
//! 3. `tracing`（开发期 stderr / example 里的 `tracing_subscriber`）。
//!
//! 为什么是进程级全局：抓凭证代理（`capture`）、列表采集（`collector`）、正文补采（`detail`）
//! 这些模块手里没有编排器的回调句柄，而「每个环节都要记」又要求它们都能写日志；单进程模型下
//! 一条总线即可（与 [`crate::runstate`] 同理）。单测里总线没有订阅者也没有 store，写日志是空操作。
//!
//! **脱敏**：所有消息入总线前过一遍 [`redact`]，把 `key= / pass_ticket= / uin= / wxtoken= /
//! appmsg_token=` 的值打码——微信接口的请求参数一律不进日志，日志只描述「在做什么、结果如何」
//! （如「获取首页文章列表…」「首页获取成功：10 篇」），不出现请求 URL。
//!
//! **瞬态行**（[`progress`]）：倒计时、轮询空转这类高频提示只推给 GUI、**不入库**，避免把
//! 3 天窗口刷满；真正的关键节点（收到任务 / 打开浏览器 / 凭证到手 / 每页结果 / 续期 / 回报 /
//! 各类告警与错误）都入库。

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, RwLock};

use serde::{Deserialize, Serialize};

use crate::model::now;
use crate::store::Store;

/// 日志级别（入库存小写字符串）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// 调试：只推给订阅者与 tracing，不入库。
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }

    /// 从入库字符串还原（未知按 Info）。
    pub fn parse(s: &str) -> Level {
        match s {
            "debug" => Level::Debug,
            "warn" => Level::Warn,
            "error" => Level::Error,
            _ => Level::Info,
        }
    }
}

/// 环节（步骤所属阶段）。入库存英文 code，GUI 展示中文标签（[`Stage::label`]）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    /// 应用生命周期：启动 / 轮询启停 / 证书安装 / 日志导出。
    App,
    /// 任务：主循环启停 / 批量添加公众号 / 预算暂停等任务级事件。
    Task,
    /// 抓凭证代理与系统代理：启动 / 停止 / 复位；出口代理：动态代理获取 / 切换 / 丢弃、手动选用。
    Proxy,
    /// RPA：点击种子链接 / 打开与关闭微信内置浏览器。
    Rpa,
    /// 凭证与接力：入口页注入 / 逐条打开任务链接 / 凭证获取成功 / 验证页 / 看门狗。
    Capture,
    /// 文章列表：首页 / 第 N 页获取与结果。
    List,
    /// 凭证集中续期。
    Refresh,
    /// 结果上报到用户配置的 HTTP 服务。
    Report,
    /// 正文补采。
    Detail,
    /// 定时巡检：本地全库公众号按轮自更新最新文章列表（批次开始 / 结束 / 一轮完成 / 退避）。
    Sweep,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::App => "app",
            Stage::Task => "task",
            Stage::Proxy => "proxy",
            Stage::Rpa => "rpa",
            Stage::Capture => "capture",
            Stage::List => "list",
            Stage::Refresh => "refresh",
            Stage::Report => "report",
            Stage::Detail => "detail",
            Stage::Sweep => "sweep",
        }
    }

    /// 中文标签（GUI / 导出文件用）。
    pub fn label(self) -> &'static str {
        match self {
            Stage::App => "应用",
            Stage::Task => "任务",
            Stage::Proxy => "抓包代理",
            Stage::Rpa => "RPA",
            Stage::Capture => "凭证接力",
            Stage::List => "文章列表",
            Stage::Refresh => "凭证续期",
            Stage::Report => "数据上报",
            Stage::Detail => "正文补采",
            Stage::Sweep => "定时巡检",
        }
    }

    /// 从入库 code 还原（未知按 App）。
    pub fn parse(s: &str) -> Stage {
        match s {
            "task" | "upstream" => Stage::Task,
            "proxy" => Stage::Proxy,
            "rpa" => Stage::Rpa,
            "capture" => Stage::Capture,
            "list" => Stage::List,
            "refresh" => Stage::Refresh,
            "report" => Stage::Report,
            "detail" => Stage::Detail,
            "sweep" => Stage::Sweep,
            _ => Stage::App,
        }
    }

    /// 全部环节（GUI 筛选下拉用）。
    pub fn all() -> [Stage; 10] {
        [
            Stage::App,
            Stage::Task,
            Stage::Proxy,
            Stage::Rpa,
            Stage::Capture,
            Stage::List,
            Stage::Refresh,
            Stage::Report,
            Stage::Detail,
            Stage::Sweep,
        ]
    }
}

/// 一条日志事件（GUI 事件载荷 / 订阅者回调入参；入库行见 [`crate::model::AppLogRow`]）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogEvent {
    /// 进程内单调递增序号（GUI 去重 / 排序用；与入库 id 无关）。
    pub seq: u64,
    /// epoch 秒。
    pub ts: f64,
    pub level: Level,
    pub stage: Stage,
    /// 所属本地任务 job id（编排器处理任务期间自动带上）。
    pub job_id: Option<i64>,
    /// 已脱敏的消息文本。
    pub message: String,
    /// 瞬态行：只推给订阅者、不入库（倒计时 / 空转轮询等高频提示）。
    pub transient: bool,
}

impl LogEvent {
    /// 单行文本（CLI 打印 / 导出文件）：`2026-09-04 10:02:32 [INFO][文章列表] 消息`。
    pub fn render(&self) -> String {
        format!(
            "{} [{}][{}]{} {}",
            format_local(self.ts),
            self.level.as_str().to_ascii_uppercase(),
            self.stage.label(),
            self.job_id
                .map(|j| format!("[job#{j}]"))
                .unwrap_or_default(),
            self.message
        )
    }
}

/// 把 epoch 秒格式化为本地时间 `YYYY-MM-DD HH:MM:SS`。
pub fn format_local(ts: f64) -> String {
    use chrono::{Local, TimeZone};
    let secs = ts.floor() as i64;
    match Local.timestamp_opt(secs, 0).single() {
        Some(t) => t.format("%Y-%m-%d %H:%M:%S").to_string(),
        None => format!("{ts:.0}"),
    }
}

/// 订阅者回调。
pub type LogSink = Arc<dyn Fn(&LogEvent) + Send + Sync>;

/// 订阅句柄：Drop 时自动退订（一次 run 结束即不再往已消失的 GUI 通道推）。
pub struct Subscription {
    id: u64,
    bus: &'static LogBus,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.bus.unsubscribe(self.id);
    }
}

/// 每写多少条入库日志顺手清一次过期行。
const PURGE_EVERY: u64 = 200;

/// 日志总线（进程级单例见 [`bus`]）。
pub struct LogBus {
    sinks: RwLock<Vec<(u64, LogSink)>>,
    store: RwLock<Option<Arc<Store>>>,
    /// 当前任务 job id（0 = 无）。
    job: AtomicI64,
    next_sink: AtomicU64,
    next_seq: AtomicU64,
    persisted: AtomicU64,
}

static BUS: LazyLock<LogBus> = LazyLock::new(LogBus::new);

/// 进程级日志总线。
pub fn bus() -> &'static LogBus {
    &BUS
}

impl Default for LogBus {
    fn default() -> Self {
        Self::new()
    }
}

impl LogBus {
    pub fn new() -> Self {
        Self {
            sinks: RwLock::new(Vec::new()),
            store: RwLock::new(None),
            job: AtomicI64::new(0),
            next_sink: AtomicU64::new(1),
            next_seq: AtomicU64::new(1),
            persisted: AtomicU64::new(0),
        }
    }

    /// 绑定入库目标（`None` = 不入库）。runner 装配 / GUI 启动时调用；绑定即清一次过期行。
    pub fn set_store(&self, store: Option<Arc<Store>>) {
        if let Some(s) = &store {
            let _ = s.purge_logs();
        }
        *self.store.write().expect("applog store lock") = store;
    }

    /// 当前入库目标。
    pub fn store(&self) -> Option<Arc<Store>> {
        self.store.read().expect("applog store lock").clone()
    }

    /// 设置 / 清除当前任务上下文（编排器处理任务期间，所有环节的日志自动带 job id）。
    pub fn set_job(&self, job_id: Option<i64>) {
        self.job.store(job_id.unwrap_or(0), Ordering::Relaxed);
        if job_id.is_some() {
            if let Some(s) = self.store() {
                let _ = s.purge_logs();
            }
        }
    }

    pub fn job(&self) -> Option<i64> {
        match self.job.load(Ordering::Relaxed) {
            0 => None,
            j => Some(j),
        }
    }

    /// 订阅（GUI / CLI）。返回的句柄 Drop 即退订。
    #[must_use = "订阅句柄 Drop 即退订，请持有它直到不再需要日志"]
    pub fn subscribe(&'static self, sink: LogSink) -> Subscription {
        let id = self.next_sink.fetch_add(1, Ordering::Relaxed);
        self.sinks
            .write()
            .expect("applog sinks lock")
            .push((id, sink));
        Subscription { id, bus: self }
    }

    fn unsubscribe(&self, id: u64) {
        self.sinks
            .write()
            .expect("applog sinks lock")
            .retain(|(i, _)| *i != id);
    }

    /// 当前订阅者数量（测试 / 诊断）。
    pub fn sink_count(&self) -> usize {
        self.sinks.read().expect("applog sinks lock").len()
    }

    /// 记一条日志：脱敏 → tracing → 入库（非瞬态且非 Debug）→ 推给订阅者。返回事件副本。
    pub fn record(
        &self,
        level: Level,
        stage: Stage,
        transient: bool,
        message: impl Into<String>,
    ) -> LogEvent {
        let message = redact(&message.into());
        let ev = LogEvent {
            seq: self.next_seq.fetch_add(1, Ordering::Relaxed),
            ts: now(),
            level,
            stage,
            job_id: self.job(),
            message,
            transient,
        };
        match level {
            Level::Debug => tracing::debug!(stage = stage.as_str(), "{}", ev.message),
            Level::Info => tracing::info!(stage = stage.as_str(), "{}", ev.message),
            Level::Warn => tracing::warn!(stage = stage.as_str(), "{}", ev.message),
            Level::Error => tracing::error!(stage = stage.as_str(), "{}", ev.message),
        }
        if !transient && level != Level::Debug {
            if let Some(s) = self.store() {
                let _ = s.append_log(
                    ev.ts,
                    level.as_str(),
                    stage.as_str(),
                    ev.job_id,
                    &ev.message,
                );
                let n = self.persisted.fetch_add(1, Ordering::Relaxed) + 1;
                if n.is_multiple_of(PURGE_EVERY) {
                    let _ = s.purge_logs();
                }
            }
        }
        let sinks: Vec<LogSink> = self
            .sinks
            .read()
            .expect("applog sinks lock")
            .iter()
            .map(|(_, s)| s.clone())
            .collect();
        for s in sinks {
            s(&ev);
        }
        ev
    }
}

/// 关键节点（入库）。
pub fn info(stage: Stage, message: impl Into<String>) {
    bus().record(Level::Info, stage, false, message);
}

/// 告警（入库）。
pub fn warn(stage: Stage, message: impl Into<String>) {
    bus().record(Level::Warn, stage, false, message);
}

/// 错误（入库）。
pub fn error(stage: Stage, message: impl Into<String>) {
    bus().record(Level::Error, stage, false, message);
}

/// 调试（不入库，只推订阅者 / tracing）。
pub fn debug(stage: Stage, message: impl Into<String>) {
    bus().record(Level::Debug, stage, false, message);
}

/// 瞬态提示（推给 GUI、不入库）：倒计时 / 轮询空转等高频行。
pub fn progress(stage: Stage, message: impl Into<String>) {
    bus().record(Level::Info, stage, true, message);
}

/// 需要打码的查询参数名（微信会话材料）。
const SENSITIVE_KEYS: &[&str] = &["key", "pass_ticket", "uin", "wxtoken", "appmsg_token"];

/// 脱敏：把文本里出现的 `key=… / pass_ticket=… / uin=… / wxtoken=… / appmsg_token=…`
/// 的值替换成 `***`（值到下一个 `&`、空白、引号、括号或逗号为止）。只匹配参数名前是
/// 行首 / `?` / `&` / 空白 / `(` / 引号 的位置，避免误伤 `monkey=` 这类词尾。
pub fn redact(text: &str) -> String {
    if !text.contains('=') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    'outer: while i < bytes.len() {
        for k in SENSITIVE_KEYS {
            let kb = k.as_bytes();
            if bytes[i..].starts_with(kb)
                && bytes.get(i + kb.len()) == Some(&b'=')
                && (i == 0
                    || matches!(
                        bytes[i - 1],
                        b'?' | b'&' | b' ' | b'\t' | b'(' | b'"' | b'\''
                    ))
            {
                out.push_str(k);
                out.push_str("=***");
                let mut j = i + kb.len() + 1;
                while j < bytes.len()
                    && !matches!(
                        bytes[j],
                        b'&' | b' ' | b'\t' | b'\n' | b')' | b'"' | b'\'' | b','
                    )
                {
                    j += 1;
                }
                i = j;
                continue 'outer;
            }
        }
        // 按字符边界推进（UTF-8 多字节）。
        let ch = text[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// key 的可展示摘要：前 6 位 + 长度（足够对照「换没换 key」，不泄露会话材料）。
pub fn key_preview(key: &str) -> String {
    let head: String = key.chars().take(6).collect();
    format!("{head}…（{} 位）", key.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn test_redact_masks_session_params() {
        let s = "请求失败 url=https://mp.weixin.qq.com/mp/profile_ext?action=getmsg&__biz=AAA==&uin=MTIz&key=abcdef123456&pass_ticket=pt%2Fx&wxtoken=&x5=0";
        let r = redact(s);
        assert!(r.contains("__biz=AAA=="), "{r}");
        assert!(r.contains("uin=***&"), "{r}");
        assert!(r.contains("key=***&"), "{r}");
        assert!(r.contains("pass_ticket=***&"), "{r}");
        assert!(r.contains("wxtoken=***&"), "{r}");
        assert!(!r.contains("abcdef123456"));
        assert!(!r.contains("MTIz"));
        // 不误伤：monkey= 不是 key=
        assert_eq!(redact("monkey=1 x=2"), "monkey=1 x=2");
        // 无 = 的文本原样
        assert_eq!(redact("首页获取成功：10 篇"), "首页获取成功：10 篇");
        // 中文夹杂
        assert_eq!(redact("凭证 key=甲乙丙 已过期"), "凭证 key=*** 已过期");
    }

    #[test]
    fn test_key_preview() {
        assert_eq!(key_preview("abcdefghij"), "abcdef…（10 位）");
        assert_eq!(key_preview("ab"), "ab…（2 位）");
    }

    #[test]
    fn test_level_stage_roundtrip() {
        for l in [Level::Debug, Level::Info, Level::Warn, Level::Error] {
            assert_eq!(Level::parse(l.as_str()), l);
        }
        for s in Stage::all() {
            assert_eq!(Stage::parse(s.as_str()), s);
            assert!(!s.label().is_empty());
        }
        assert_eq!(Level::parse("weird"), Level::Info);
        assert_eq!(Stage::parse("weird"), Stage::App);
    }

    #[test]
    fn test_bus_persists_key_nodes_and_skips_transient() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let b = LogBus::new();
        b.set_store(Some(store.clone()));
        b.set_job(Some(7));
        b.record(Level::Info, Stage::List, false, "首页获取成功：3 篇");
        b.record(Level::Info, Stage::Capture, true, "等待凭证… 剩余 5s");
        b.record(Level::Debug, Stage::Capture, false, "relay inject");
        b.record(
            Level::Warn,
            Stage::List,
            false,
            "凭证过期 key=SECRET 待续期",
        );
        let rows = store.list_logs(None, None, None, 100).unwrap();
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0].stage, "list");
        assert_eq!(rows[0].job_id, Some(7));
        assert_eq!(rows[1].level, "warn");
        assert!(rows[1].message.contains("key=***"));
        assert!(!rows[1].message.contains("SECRET"));
        b.set_job(None);
        b.record(Level::Info, Stage::App, false, "无任务上下文");
        let rows = store.list_logs(None, None, None, 100).unwrap();
        assert_eq!(rows[2].job_id, None);
    }

    #[test]
    fn test_global_subscribe_and_unsubscribe() {
        let got = Arc::new(Mutex::new(Vec::<String>::new()));
        let g = got.clone();
        let before = bus().sink_count();
        let sub = bus().subscribe(Arc::new(move |e| {
            g.lock()
                .unwrap()
                .push(format!("{}|{}", e.stage.as_str(), e.message))
        }));
        assert_eq!(bus().sink_count(), before + 1);
        progress(Stage::Rpa, "点击种子链接…");
        assert!(got.lock().unwrap().iter().any(|s| s == "rpa|点击种子链接…"));
        drop(sub);
        assert_eq!(bus().sink_count(), before);
    }

    #[test]
    fn test_render_line() {
        let ev = LogEvent {
            seq: 1,
            ts: 0.0,
            level: Level::Warn,
            stage: Stage::List,
            job_id: Some(3),
            message: "x".into(),
            transient: false,
        };
        let line = ev.render();
        assert!(line.contains("[WARN][文章列表][job#3] x"), "{line}");
        assert!(line.starts_with("19"), "{line}");
    }
}
