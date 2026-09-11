//! `mpider-core`：mpider 的 Rust 核心库（store/config/model、抓凭证纯函数、MITM 抓凭证 +
//! TLS 直通、回放解析、结果上报）。
//!
//! 行为对齐自早期 Python 原型（未随开源发布）：SQLite schema、接口语义与解析规则以它为准。
//!
//! 模块速览：
//! - [`model`]：数据模型（对齐 Python SQLite 行结构）。
//! - [`store`]：rusqlite 存储层（accounts/credentials/articles/jobs/config/relay）。
//! - [`proxy_addon`]：抓凭证纯函数（extract_credentials/parse_s_url/… + TLS 直通判定）。
//! - [`relay`]：脚本注入接力（纯函数）。
//! - [`report`]：采集结果上报——每个号列表采完就 POST 一条通用 JSON 到用户配置的 HTTP 服务（可选 Bearer）。
//! - [`notify`]：飞书通知——开关 + 机器人 Webhook / 签名密钥，只在异常（任务异常 / 限流退避 /
//!   巡检故障 / 轮询退出）时入队由单一线程按序推送，60s 内同文案去重。
//! - [`wechat`]：getmsg 列表 / 正文 HTML 解析 + `fetch_msg_list` 网络层。
//! - [`collector`]：列表采集（`collect_list`：第一页 / 按「最后更新时间」翻页 / 从 offset 续采）。
//! - [`credrefresh`]：⭐ 凭证续期独立封装（一次接力批量续掉整批过期号，≥3 次重试）。
//! - [`rng`]：轻量随机数（翻页随机等待、续期随机取样）。
//! - [`detail`]：正文补采（并发抓公开 /s 页 → article-md 解析为 Markdown）。
//! - [`ca`]：根 CA 生成 + hudsucker 授权体。
//! - [`capture`]：⭐ MITM 抓凭证 + 接力注入 + **按 SNI 直通** 的代理生命周期。
//! - [`manualcap`]：手动抓凭证代理（调试）——不跑任务只开代理 + 系统代理，请求追踪写日志，到时自动停；与整链互斥。
//! - [`seedserver`]：本机种子入口服务——固定端口 HTTP 服务直接输出带接力脚本的种子页
//!   （文件传输助手里那条固定链接的落点；内置浏览器对回环地址绕过代理，故不能靠 MITM 注入）。
//! - [`sysproxy`]：系统代理开关 + **Drop 保证复位** 的 RAII 守卫。
//! - [`rpa`]：微信内置浏览器控制器（open_seed/close_browser，mac/win/NoOp；种子点位由人工框选标定）。
//! - [`orchestrator`]：把整链串起来的编排器（依赖全可注入）。
//! - [`phases`]：任务阶段计时（编排链各转折点打点，随 `jobs.phases_json` 落库，GUI 耗时列 hover 展示）。
//! - [`runner`]：真机整链库级入口 `run_loop_real`（巡检 / 历史模式主循环）与单号 `sweep_retry_account`。
//! - [`runstate`]：运行期全局状态（抓包代理是否在跑 / 整机限流退避 / 巡检进度 / 整链互斥）。
//! - [`sweep`]：⭐ 定时巡检——本地全库公众号按轮自更新最新文章列表（编排主循环的巡检模式任务源）。
//! - [`addlink`]：「批量添加」——短链匿名直连文章页解析公众号名称 / biz 建号，记种子链接。
//! - [`applog`]：⭐ 环节日志总线（一份事件 → GUI 订阅 / SQLite `app_log` 入库留 3 天 / tracing；
//!   消息脱敏，不出现微信接口参数）。
//! - [`ratelimit`]：限流分析——把每次 `getmsg` 请求 / 每个执行单元 / 每次退避的留档聚合成
//!   GUI「限流分析」页的统计（请求频率、间隔分布、轮次 / 批次摘要、退避记录）。

pub mod addlink;
pub mod applog;
pub mod ca;
pub mod capture;
pub mod collector;
pub mod credrefresh;
pub mod detail;
pub mod history;
pub mod manualcap;
pub mod model;
pub mod notify;
pub mod orchestrator;
pub mod phases;
pub mod probe;
pub mod proxy_addon;
pub mod ratelimit;
pub mod relay;
pub mod report;
pub mod rng;
pub mod rpa;
pub mod runner;
pub mod runstate;
pub mod seedserver;
pub mod store;
pub mod sweep;
pub mod sysproxy;
pub mod wechat;

// 常用类型再导出，方便上层（src-tauri / CLI）直接引用。
pub use addlink::{AddLinkResult, AddLinkStatus, AddLinksSummary};
pub use applog::{Level as LogLevel, LogBus, LogEvent, Stage as LogStage};
pub use ca::CaMaterial;
pub use capture::{
    start, start_on, CaptureConfig, CaptureHandler, CaptureProxy, PassthroughDecider,
};
pub use collector::{
    collect_list, CollectOutcome, CredentialExpired, ListPlan, ListRequest, ListSource,
};
pub use credrefresh::{CredentialRefresher, RefreshConfig, RefreshReport, RefreshTarget};
pub use detail::{collect_detail, DetailConfig, DetailProgress, DetailSummary};
pub use history::{HistoryConfig, PAGE_COUNT_MAX as HISTORY_PAGE_COUNT_MAX};
pub use manualcap::ManualCaptureStatus;
pub use model::{
    Account, Alert, AppLogRow, ArticleFields, ArticleRow, CooldownLogRow, Credential,
    CredentialFields, CredentialLogRow, HistoryEstimate, HistoryJob, HistoryJobView, HistoryStatus,
    HistoryTarget, Job, JobKind, JobRow, ListCallEvent, ListCallRow, ParsedArticle, RelayRow,
    ReportAck, RunLogEvent, RunLogRow,
};
pub use notify::{FeishuConfig, NotifyStatus};
pub use orchestrator::{bizs_from_links, Orchestrator, OrchestratorConfig, Report};
pub use probe::{probe_intercept, InterceptProbe};
pub use proxy_addon::{is_short_article_link, normalize_article_link, require_short_article_link};
pub use ratelimit::{stats as ratelimit_stats, RateLimitStats};
pub use relay::RelayQueue;
pub use report::{HttpReporter, ReportArticle, ReportConfig, ReportPayload};
pub use rpa::{get_controller, SeedMark, SelfCheck, TestClick, WeChatController};
pub use runner::{run_loop_real, sweep_retry_account, RealRunConfig, RETRY_BUSY_MSG, RUN_BUSY_MSG};
pub use runstate::{CooldownSnapshot, SweepPhase, SweepStatus};
pub use seedserver::{seed_url, DEFAULT_SEED_HOST, DEFAULT_SEED_PORT};
pub use store::Store;
pub use sweep::{RetryOutcome, Sweep, SweepBatch, SweepConfig};
pub use sysproxy::{CommandApplier, ProxyApplier, SystemProxyGuard};
