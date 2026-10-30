//! Windows 采集工作台：独立任务、历史检查点、游标实验和离线归档的公开契约。
pub mod archive;
pub mod discovery;
pub mod engine;
pub mod markdown_export;
pub mod push_monitor;
#[cfg(test)]
mod tests;

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Policy {
    pub version: i64,
    pub list_min_ms: u64,
    pub list_max_ms: u64,
    pub list_daily_budget: i64,
    pub body_gap_ms: u64,
    pub image_gap_ms: u64,
    pub body_workers: usize,
    pub image_workers: usize,
    pub retries: u32,
    pub rebuild_limit: u32,
    pub cooldown_until: f64,
    pub cooldown_seconds: u64,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            version: 1,
            list_min_ms: 90000,
            list_max_ms: 120000,
            list_daily_budget: 60,
            body_gap_ms: 1000,
            image_gap_ms: 1000,
            body_workers: 4,
            image_workers: 4,
            retries: 2,
            rebuild_limit: 1,
            cooldown_until: 0.,
            cooldown_seconds: 300,
        }
    }
}
impl Policy {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.list_min_ms <= self.list_max_ms && self.list_max_ms <= 86400000,
            "列表间隔范围无效"
        );
        ensure!(
            self.body_gap_ms <= 86400000 && self.image_gap_ms <= 86400000,
            "请求间隔不能超过一天"
        );
        ensure!(self.list_daily_budget > 0, "列表预算须为正数");
        ensure!(
            (1..=12).contains(&self.body_workers) && (1..=12).contains(&self.image_workers),
            "并发须为 1 至 12"
        );
        ensure!(
            self.retries <= 10 && self.rebuild_limit <= 10,
            "重试与重建次数须为 0 至 10"
        );
        ensure!(
            self.cooldown_seconds > 0 && self.cooldown_seconds <= 86400,
            "异常冷却须为 1 至 86400 秒"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HistoryTarget {
    pub count: Option<u32>,
    /// 实验室显式获取下一页；与日期、篇数目标互斥。
    pub pages: Option<u32>,
    /// 单页任务实际使用的当前游标，由入队事务绑定。
    pub cursor_id: Option<i64>,
    /// 截止日本机零点，包含当天；由前端用本机时区转换。
    pub cutoff_at: Option<i64>,
}
impl HistoryTarget {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            [
                self.count.is_some(),
                self.cutoff_at.is_some(),
                self.pages.is_some()
            ]
            .into_iter()
            .filter(|v| *v)
            .count()
                == 1,
            "请选择一个历史目标"
        );
        ensure!(
            self.count.is_none_or(|v| v > 0)
                && self.cutoff_at.is_none_or(|v| v > 0)
                && self.pages.is_none_or(|v| v == 1),
            "历史目标无效"
        );
        ensure!(
            self.cursor_id.is_none() || self.pages == Some(1),
            "仅单页任务可绑定游标"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct WorkbenchPage<T> {
    pub items: Vec<T>,
    pub total: i64,
}
/// 删除公众号后的实际处理结果；历史请求与任务审计保留。
#[derive(Clone, Debug, Default, Serialize)]
pub struct SourceDeleteReceipt {
    pub deleted_articles: usize,
    pub cancelled_tasks: usize,
    pub cancelled_experiments: usize,
    /// 文件占用等原因尚未清理的文章缓存目录数，后台会重试。
    pub cleanup_pending: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct SourceListItem {
    #[serde(flatten)]
    pub source: Source,
    /// 未删除文章中已有非空 Markdown 正文的数量，用于下载与导出入口互斥。
    pub body_count: i64,
    pub home_task: Option<Task>,
    pub history_task: Option<Task>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Source {
    pub biz: String,
    pub nickname: String,
    pub user_name: String,
    pub session_id: String,
    /// 公众号页、实验室、手动下一页共同引用的当前游标。
    #[serde(default)]
    pub current_cursor_id: Option<i64>,
    #[serde(default)]
    pub current_cursor_at: f64,
    pub home_cursor_id: Option<i64>,
    pub history_cursor_id: Option<i64>,
    pub pending_count: i64,
    pub ended: bool,
    pub article_count: i64,
    pub updated_at: f64,
    #[serde(default)]
    pub boundary_keys: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub biz: String,
    #[serde(skip_serializing)]
    pub path: String,
    pub config: crate::native::NativeConfig,
    pub created_at: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CursorRecord {
    pub id: i64,
    pub biz: String,
    pub session_id: String,
    pub parent_id: Option<i64>,
    /// 该采集链中返回此游标的页面；旧检查点缺少证据时保持未知。
    #[serde(default)]
    pub source_page: Option<u32>,
    pub hash: String,
    /// 原值在当前 Windows 用户 DPAPI 文件中；IPC 不返回该路径。
    #[serde(skip_serializing)]
    pub evidence_path: String,
    pub observed_at: f64,
    pub last_success_at: Option<f64>,
    pub state: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskProgress {
    /// 已被服务端拒绝的会话版本；恢复前必须实际保存不同版本的会话。
    pub blocked_session_id: Option<String>,
    pub session_recovery: Option<SessionRecovery>,
    pub completed: u32,
    /// 成功列表响应实际返回的文章数；旧任务缺少证据时保持未知。
    pub received_articles: Option<u32>,
    pub skipped: u32,
    pub new_articles: u32,
    pub pages: u32,
    pub failed: u32,
    pub partial: u32,
    pub rebuilds: u32,
    pub rebuilding: bool,
    pub rebuild_cursor: Option<i64>,
    pub rebuild_anchor: Vec<String>,
    pub rebuilt_seen: Vec<String>,
    pub date_boundary_pages: u32,
    pub finished_ids: Vec<i64>,
}

pub const SESSION_RECOVERY_HINT: &str = "点击“更新会话并继续”，软件启动主页识别，监测就绪后请在微信打开或刷新该公众号主页；新会话保存后自动继续，原游标与历史进度保留";

/// 用户明确下发的恢复意图；绑定同步运行、公众号和原会话，停止或暂停即撤销。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionRecovery {
    pub discovery_id: i64,
    pub requested_at: f64,
    pub previous_session_id: String,
    pub phase: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: i64,
    pub kind: String,
    pub biz: Option<String>,
    pub state: String,
    pub target: serde_json::Value,
    pub progress: TaskProgress,
    pub reason: Option<String>,
    pub next_action_at: f64,
    pub created_at: f64,
    pub updated_at: f64,
    pub result_path: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Experiment {
    /// 当前执行阶段与闸门给出的最早执行时间，跨重启保留。
    #[serde(default)]
    pub phase: String,
    #[serde(default)]
    pub next_action_at: Option<f64>,
    /// 结论分类，区别游标不可复用与会话、协议故障。
    #[serde(default)]
    pub verdict: Option<String>,
    pub id: i64,
    pub cursor_id: i64,
    pub session_id: String,
    pub schedule_hours: Vec<f64>,
    pub next_index: usize,
    pub state: String,
    pub started_at: f64,
    pub deadline_at: f64,
    pub conclusion: Option<String>,
    pub report_path: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Probe {
    pub id: i64,
    pub experiment_id: i64,
    pub request_id: Option<String>,
    pub role: String,
    pub scheduled_at: f64,
    pub actual_at: f64,
    pub cursor_age_seconds: f64,
    pub outcome: String,
    pub evidence_path: Option<String>,
    pub detail: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestEvent {
    pub id: String,
    pub category: String,
    pub purpose: String,
    pub owner_id: i64,
    #[serde(default)]
    pub owner_kind: String,
    pub biz: String,
    pub started_at: f64,
    pub finished_at: Option<f64>,
    pub outcome: String,
    pub policy_version: i64,
    pub gap_ms: Option<i64>,
    pub detail: serde_json::Value,
}

/// 每轮识别的目标与观察状态持久化；重启后只恢复状态，不自动附加微信。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Discovery {
    pub id: i64,
    pub state: String,
    pub target: Option<HistoryTarget>,
    #[serde(default)]
    pub scope_started_at: f64,
    pub started_at: f64,
    pub updated_at: f64,
    pub reason: Option<String>,
    pub observed_requests: u64,
    pub accounts: Vec<DiscoveryAccount>,
    #[serde(default)]
    pub phases: Vec<DiscoveryPhase>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiscoveryPhase {
    pub phase: String,
    pub ts: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiscoveryAccount {
    pub biz: String,
    pub observed_at: f64,
    pub session_ready: bool,
    pub task_id: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub active_queues: ActiveQueues,
    pub discovery: Option<Discovery>,
    pub sources: Vec<Source>,
    pub cursors: Vec<CursorRecord>,
    pub tasks: Vec<Task>,
    pub experiments: Vec<Experiment>,
    pub requests: Vec<RequestEvent>,
    pub policy: Policy,
    pub list_calls_24h: i64,
    pub archives: Vec<ArchiveRecord>,
    pub download_queue: DownloadQueue,
}

/// 仅统计当前未结束任务；日期目标的总篇数未知。
#[derive(Clone, Debug, Default, Serialize)]
pub struct QueueProgress {
    pub task_count: usize,
    pub completed: usize,
    pub total: Option<usize>,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct ActiveQueues {
    pub download: QueueProgress,
    pub history: QueueProgress,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct DownloadReceipt {
    pub task_id: Option<i64>,
    pub requested: usize,
    pub queued: usize,
    pub already_downloaded: usize,
    pub already_queued: usize,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct DownloadQueue {
    pub task_count: usize,
    pub total: usize,
    pub completed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub pending: usize,
}
impl DownloadQueue {
    pub fn from_tasks(tasks: &[Task]) -> Self {
        let mut queue = Self::default();
        for task in tasks
            .iter()
            .filter(|t| t.kind == "archive" && t.target["download_only"] == true)
        {
            let total = task.target["article_ids"].as_array().map_or(0, Vec::len);
            let completed = (task.progress.completed as usize).min(total);
            queue.task_count += 1;
            queue.total += total;
            queue.completed += completed;
            queue.skipped += task.progress.skipped as usize;
            queue.failed += task.progress.failed as usize;
            if task.state == "cancelled" {
                queue.cancelled += total - completed;
            } else {
                queue.pending += total - completed;
            }
        }
        queue
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArchiveRecord {
    pub article_id: i64,
    pub state: String,
    pub directory: String,
    pub missing_images: Vec<String>,
    pub updated_at: f64,
    #[serde(default)]
    pub error: Option<String>,
}

pub struct Permit {
    pub request_id: Option<String>,
    pub next_at: f64,
    pub reason: Option<String>,
}

/// 新的所有列表入口共享同一把锁，包含首次微信取样和实验对照请求。
pub fn list_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}
