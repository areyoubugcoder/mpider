//! 微信原生列表采集：已登录客户端的页面驱动、桥消息解析及受限自动翻页。
//!
//! 原生游标是不透明字符串，不能与旧 HTTP 列表的整数 offset 混用。
//! 支持页面驱动与会话独立请求；均不安装证书、不修改系统代理。

pub mod protocol;
pub mod runtime;

use serde::{Deserialize, Serialize};

/// 原生采集配置；每次任务保存快照，后续修改设置不改变在途任务。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NativeConfig {
    pub enabled: bool,
    /// page 使用微信页面；session 自动取样后独立请求。
    pub transport: String,
    /// 可选的研究 JSON 或当前用户保护的会话文件；空值自动从微信提取。
    pub session_file: String,
    pub python_path: String,
    /// 公众号名称；会话模式可留空，从首个关联响应自动绑定，此后用于身份核对。
    pub window_title: String,
    pub expected_biz: Option<String>,
    pub min_gap_seconds: u64,
    pub max_gap_seconds: u64,
    pub daily_budget: i64,
    pub max_pages: u32,
    pub response_timeout_seconds: u64,
}

impl Default for NativeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            transport: "page".into(),
            session_file: String::new(),
            python_path: "python".into(),
            window_title: String::new(),
            expected_biz: None,
            min_gap_seconds: 90,
            max_gap_seconds: 120,
            daily_budget: 60,
            max_pages: 1,
            response_timeout_seconds: 45,
        }
    }
}

impl NativeConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            ["page", "session"].contains(&self.transport.as_str()),
            "未知采集方式"
        );
        anyhow::ensure!(
            !self.python_path.trim().is_empty(),
            "请指定原生采集运行环境"
        );
        anyhow::ensure!(
            self.transport == "session" || !self.window_title.trim().is_empty(),
            "页面模式需要已打开的公众号名称"
        );
        anyhow::ensure!((1..=500).contains(&self.max_pages), "单次页数须为 1 至 500");
        anyhow::ensure!(
            self.daily_budget >= 1,
            "原生采集必须设置正数的 24 小时请求预算"
        );
        anyhow::ensure!(
            self.max_gap_seconds >= self.min_gap_seconds && self.max_gap_seconds <= 86400,
            "间隔范围无效，最大间隔须不小于最小间隔且不超过一天"
        );
        anyhow::ensure!(
            (10..=180).contains(&self.response_timeout_seconds),
            "响应等待须为 10 至 180 秒"
        );
        Ok(())
    }
}

/// 任务状态；暂停不会自动重试，恢复需显式操作并重新核对页面。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NativeJob {
    pub id: i64,
    pub status: String,
    pub config: NativeConfig,
    pub biz: Option<String>,
    pub user_name: Option<String>,
    pub nickname: Option<String>,
    #[serde(skip_serializing)]
    pub cursor: Option<String>,
    pub pages: i64,
    pub total_pages: i64,
    pub new_articles: i64,
    pub last_request_at: Option<f64>,
    pub next_action_at: Option<f64>,
    pub pause_reason: Option<String>,
    pub started_at: f64,
    pub updated_at: f64,
    pub finished_at: Option<f64>,
}

/// 每次客户端请求一行；游标只留哈希，原始游标仅在任务检查点保存。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NativeCall {
    pub id: i64,
    pub job_id: i64,
    pub request_id: String,
    pub ts: f64,
    pub cgi: i64,
    pub action: Option<i64>,
    pub outcome: String,
    pub latency_ms: Option<i64>,
    pub gap_ms: Option<i64>,
    pub outer_ret: Option<i64>,
    pub jsapi_ret: Option<i64>,
    pub inner_ret: Option<i64>,
    pub request_cursor_hash: Option<String>,
    pub response_cursor_hash: Option<String>,
    pub cursor_advanced: Option<bool>,
    pub articles: i64,
    pub new_articles: i64,
    pub dll_sha256: String,
    pub gate_min_ms: i64,
    pub gate_max_ms: i64,
    pub reason: Option<String>,
    pub transport: String,
    pub policy_version: i64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct NativeStatus {
    pub active: Option<NativeJob>,
    pub recent: Vec<NativeJob>,
    pub calls: Vec<NativeCall>,
    pub calls_24h: i64,
    pub running: bool,
}
