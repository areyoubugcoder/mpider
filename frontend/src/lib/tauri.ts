import { invoke } from "@tauri-apps/api/core";

// ============ 与 src-tauri/src/lib.rs 的 #[command] 返回体对齐的类型 ============
// 任何一处字段/形状变动都要和后端 lib.rs 同步（协议契约）。

export interface HealthInfo {
  ok: boolean;
  app_version: string;
  tauri_version: string;
  core: string;
  db_path: string;
  account_count: number;
  platform: string;
  arch: string;
}

export interface Account {
  biz: string;
  nickname: string | null;
  round_head_img: string | null;
  first_seen: number;
  last_seen: number;
  focus: number;
  /** —— 以下为该号凭证热数据摘要（credentials 表每号一行；不含 key 等参数本体）—— */
  /** 是否抓到过凭证（key 非空）。 */
  cred_has_key: boolean;
  /** 凭证当前是否新鲜（key 非空、未实测失效、未超 TTL）。 */
  cred_fresh: boolean;
  /** 最近一次抓到 / 刷新凭证的时刻（epoch 秒）。 */
  cred_captured_at: number | null;
  /** 预估过期时刻（captured_at + ttl）。 */
  cred_expires_at: number | null;
  /** 实测失效时刻（回放拿到 ret=-3 时打点）。 */
  cred_invalidated_at: number | null;
  /** 最近一次拿它回放接口的时刻。 */
  cred_last_used_at: number | null;
  /** 当前 key 累计回放次数。 */
  cred_use_count: number;
  /** 累计换 key 次数。 */
  cred_refresh_count: number;
  /** 该号已采到的最新一篇文章的发布时间（epoch 秒；无文章时 null）。 */
  latest_published_at: number | null;
  /** —— 定时巡检（accounts 表巡检列）—— */
  /** 上轮巡检成功后记下的最新发布时间（下一轮翻页依据）。 */
  last_published_at: number | null;
  /** 最近一次巡检处理时刻。 */
  sweep_checked_at: number | null;
  /** 最近一次巡检结果：ok / failed / no_sample。 */
  sweep_status: string | null;
  /** 最近一次巡检失败 / 跳过原因。 */
  sweep_error: string | null;
  /** 是否参与巡检（1 / 0）。 */
  sweep_enabled: number;
  /** 本轮内已尝试次数。 */
  sweep_attempts: number;
  /** 批量添加时粘贴的文章短链（种子链接）：库里还没有该号文章时接力取样用。 */
  seed_url: string | null;
  /** —— 历史文章抓取 —— 该号最新一条历史任务（没有则 null）。 */
  history: HistoryJobView | null;
}

/** mpider_core::runner::RealRunConfig 的子集（其余字段走 serde 默认）。 */
export interface RealRunConfig {
  /** 数据上报开关（默认关）：开了每个公众号列表采完后向 report_url POST 一次 JSON。 */
  report_enabled: boolean;
  /** 上报地址（http / https）。 */
  report_url: string;
  /** 上报鉴权 Bearer token（可空）。 */
  report_token: string;
  /** 上报请求超时（秒）。 */
  report_timeout_secs: number;
  capture_port: number;
  /** 本机种子入口服务写进种子链接里的 host（默认 127.0.0.1；配成本机局域网 IP 时服务绑全部网卡）。 */
  seed_host: string;
  /** 本机种子入口服务端口（固定；文件助手里那条链接不会跟着变，改了要重发并重新框选）。 */
  seed_port: number;
  /** 每条链接停留下限（毫秒）。 */
  relay_dwell_ms: number;
  /** 停留上限（毫秒）：每跳在 [下限, 上限] 内随机，避免固定节拍；<= 下限即固定停留。 */
  relay_dwell_max_ms: number;
  /** 种子链接打开后等多久跳到本批第一条任务链接（毫秒，固定值；下限 0，默认 100）。之后各篇按上面的区间随机。 */
  seed_dwell_ms: number;
  capture_wait_seconds: number;
  /** 看门狗：点种子后多少秒无文章请求判"点空"重点。 */
  relay_launch_timeout_seconds: number;
  /** 看门狗：接力中多少秒无文章请求判停滞（后端会抬到 停留上限+8s 以上）。 */
  relay_stall_seconds: number;
  /** 看门狗：无进展重拉的最大次数。 */
  relay_max_relaunch: number;
  /** 有「最后更新时间」时单号单次最多翻页数（安全阀）。 */
  list_max_pages: number;
  /** 翻页之间随机等待的下限 / 上限（毫秒）。 */
  page_sleep_min_ms: number;
  page_sleep_max_ms: number;
  /** 凭证续期：一次集中续期最多尝试几轮（≥3）。 */
  cred_refresh_attempts: number;
  /** 凭证续期：每轮等新 key 的上限（秒）。 */
  cred_refresh_wait_seconds: number;
  set_sysproxy: boolean;
  sysproxy_service: string;
  /** RPA 拉起内置浏览器后是否再发 Ctrl+F5 硬刷（默认关；种子页 / 文章页都已 no-store，留作回退） */
  rpa_hard_refresh: boolean;
  /** 定时巡检：一轮跑完后停留多久再开始下一轮（秒）。 */
  sweep_idle_seconds: number;
  /** 定时巡检：每批几个号（1~100）。 */
  sweep_batch_size: number;
  /** 全局列表请求闸门：任意两次 getmsg（跨号 / 跨页 / 跨任务）之间的随机间隔区间（毫秒）。 */
  list_gap_min_ms: number;
  list_gap_max_ms: number;
  /** 每号列表请求预算：当前微信号近 24 小时 getmsg 上限（所有路径合计；0 = 不限），达到后巡检与历史抓取都暂停。 */
  list_daily_budget: number;
  /** 定时巡检：两个批次之间至少隔多久（秒）。 */
  sweep_batch_gap_seconds: number;
  /** 定时巡检：一批无一成功且有号待重试时，整个巡检暂停多久再继续（秒）。 */
  sweep_fail_pause_seconds: number;
  /** 历史文章抓取：每页最多请求条数（1~50；微信服务端通常按 10 封顶）。 */
  history_page_count: number;
  /** 历史文章抓取：页与页之间的间隔（秒；不小于列表闸门上限）。 */
  history_gap_seconds: number;
  /** 历史文章抓取：为巡检保留的预算次数——历史任务不吃掉当前微信号 24 小时预算的最后 N 次。 */
  history_budget_reserve: number;
  /** 飞书通知开关（默认关）：开了要配机器人 Webhook；只在关键节点出问题时推预警到群。 */
  feishu_enabled: boolean;
  /** 飞书自定义机器人 Webhook（https://open.feishu.cn/open-apis/bot/v2/hook/<uuid>）。 */
  feishu_webhook: string;
  /** 飞书机器人「签名校验」密钥（空 = 机器人未开签名）。 */
  feishu_secret: string;
  /** 前端本地配置：补详情的抓取间隔（毫秒/请求）。不属于后端 RealRunConfig，
   *  随 cfg 一起传给 run_agent_once 时会被 serde 忽略，无害。 */
  detail_throttle_ms: number;
  /** 前端本地配置：补详情的并发 worker 数（1..=12）。节流是每 worker 各自算的，
   *  整体速率 ≈ workers/间隔；设为 1 才是真正的「每 detail_throttle_ms 一篇」串行。 */
  detail_workers: number;
}

export interface CertInfo {
  installed: boolean;
  platform: string;
  can_auto: boolean;
  ca_path: string;
  ca_exists: boolean;
  manual_command: string;
}

export interface CertInstallOutcome {
  ok: boolean;
  installed: boolean;
  log: string[];
  info: CertInfo;
}

export interface SystemStatus {
  platform: string;
  cert_installed: boolean;
  proxy_enabled: boolean;
  proxy_endpoint: string | null;
  proxy_service: string;
  capture_running: boolean;
  capture_addr: string | null;
  wechat_running: boolean;
  /** 本平台能否自动点开种子链接（Windows）。false = 人工模式（mac）：种子 / 框选 / 自检相关 UI 整体隐藏。 */
  rpa_auto: boolean;
}

/** 「公众号文章」列表行（后端 ArticleItem）。 */
export interface ArticleItem {
  id: number;
  biz: string;
  account: string | null;
  title: string | null;
  author: string | null;
  published_at: number | null;
  /** 1=已采详情 / 0=待采 / -1=永久不可用（微信提示页，原因见 detail_error）。 */
  detail_done: number;
  /** 最近一次补详情失败/不可用的原因。 */
  detail_error: string | null;
  content_url: string | null;
}

/** 单篇文章详情（Markdown 阅读）。 */
export interface ArticleDetail {
  id: number;
  title: string | null;
  author: string | null;
  published_at: number | null;
  content_url: string | null;
  content_md: string | null;
}

/** 文章列表一页（翻页）。 */
export interface ArticlePage {
  items: ArticleItem[];
  total: number;
}

export interface ArticleCounts {
  total: number;
  detail_done: number;
  /** 还会被自动批量补采处理的待补数（排除不可用与失败满 3 次的）。 */
  pending: number;
}

/** 环节日志级别（对齐 mpider_core::applog::Level；debug 不入库）。 */
export type LogLevel = "debug" | "info" | "warn" | "error";

/** 环节 code（对齐 mpider_core::applog::Stage）。 */
export type LogStage =
  | "app"
  | "task"
  | "proxy"
  | "rpa"
  | "capture"
  | "list"
  | "refresh"
  | "report"
  | "detail"
  | "sweep";

/** 环节中文标签（与 mpider_core::applog::Stage::label 一致）。 */
export const STAGE_LABELS: Record<LogStage, string> = {
  app: "应用",
  task: "任务",
  proxy: "抓包代理",
  rpa: "RPA",
  capture: "凭证接力",
  list: "文章列表",
  refresh: "凭证续期",
  report: "数据上报",
  detail: "正文补采",
  sweep: "定时巡检",
};

/** 环节顺序（筛选下拉用）。 */
export const STAGE_ORDER: LogStage[] = [
  "app",
  "task",
  "sweep",
  "proxy",
  "rpa",
  "capture",
  "list",
  "refresh",
  "report",
  "detail",
];

/** 巡检阶段（对齐 mpider_core::runstate::SweepPhase）。 */
export type SweepPhase = "off" | "running" | "waiting" | "cooldown";

/** 巡检进度 + 整机限流退避（后端 `sweep_status` 返回体）。 */
export interface SweepInfo {
  phase: SweepPhase;
  pass_started_at: number | null;
  pass_total: number;
  pass_done: number;
  pass_new_articles: number;
  next_pass_at: number | null;
  cooldown_until: number | null;
  cooldown_reason: string | null;
  last_error: string | null;
  passes_completed: number;
  /** 环境故障（RPA 点不开种子 / 代理没流量）导致的暂停截止时刻。 */
  paused_until: number | null;
  paused_reason: string | null;
  /** 当前微信号近 24 小时已发的列表（getmsg）请求数（所有路径合计）与每号预算（0 = 不限）。 */
  list_calls_24h: number;
  list_daily_budget: number;
  /** 已点「停止巡检」、正在等当前批次收尾。 */
  stopping: boolean;
  total_enabled: number;
  cooldown_level: number;
}

export const sweepStatus = () => invoke<SweepInfo>("sweep_status");
export const setAccountSweepEnabled = (biz: string, enabled: boolean) =>
  invoke<void>("set_account_sweep_enabled", { biz, enabled });
export const clearCooldown = () => invoke<void>("clear_cooldown");
/** 立即开始新一轮巡检：跳过停留 / 暂停，全部号重新排队。 */
export const sweepRestartPass = () => invoke<void>("sweep_restart_pass");

// ============ 限流分析（对齐 mpider_core::ratelimit::RateLimitStats）============

/** 按结果分的 getmsg 请求计数。 */
export interface OutcomeCounts {
  calls: number;
  ok: number;
  /** ret=-3 凭证过期。 */
  expired: number;
  /** ret=-6/-12 微信号被限制。 */
  blocked: number;
  /** HTTP 429 / 5xx / 接口回网页。 */
  rate_limited: number;
  error: number;
  /** ok 请求解析出的篇数 / 新入库篇数。 */
  articles: number;
  new_articles: number;
}

/** 请求来源：sweep 定时巡检 / retry 单号手动重试 / history 历史抓取（manual 是老版本手动任务的留档）。 */
export type ListSource = "manual" | "sweep" | "retry" | "history";

export const SOURCE_LABELS: Record<string, string> = {
  manual: "手动任务（旧）",
  sweep: "定时巡检",
  retry: "手动重试",
  history: "历史抓取",
};

export interface SourceCounts extends OutcomeCounts {
  source: ListSource | string;
}

/** 按影响因素（出口代理 / 微信号）分组的计数（后端 `ratelimit::FactorCounts`）。 */
export interface FactorCounts extends OutcomeCounts {
  /** 代理名称 / "直连"；或 uin 短哈希 / "未知"。 */
  key: string;
  first_ts: number;
  last_ts: number;
  avg_latency_ms: number;
}

/** 一个时间桶（起点 epoch 秒 + 桶内计数）。 */
export interface RateBucket extends OutcomeCounts {
  ts: number;
}

export interface GapBin {
  label: string;
  upto_ms: number;
  count: number;
}

/** 相邻两次 getmsg 的间隔统计（整机、不分来源）。 */
export interface GapStats {
  samples: number;
  min_ms: number;
  p50_ms: number;
  avg_ms: number;
  max_ms: number;
  histogram: GapBin[];
  /** 最近的间隔样本（毫秒，升序，最多 2000 个）。 */
  recent_ms: number[];
}

export interface LatencyStats {
  samples: number;
  avg_ms: number;
  p50_ms: number;
  max_ms: number;
}

/** 一轮巡检的汇总（各批次 run_log 合成）。 */
export interface PassSummary {
  pass_no: number;
  started_at: number;
  finished_at: number;
  in_progress: boolean;
  batches: number;
  accounts: number;
  ok: number;
  failed: number;
  retry: number;
  deferred: number;
  new_articles: number;
  list_calls: number;
  rate_limited_batches: number;
  blocked_batches: number;
  verify_batches: number;
  env_failures: number;
}

/** 一个执行单元（巡检批次；老库里还有手动任务）的结果摘要（run_log 行）。 */
export interface RunLogRow {
  id: number;
  job_id: number | null;
  kind: "manual" | "sweep" | string;
  pass_no: number | null;
  started_at: number;
  finished_at: number;
  accounts: number;
  ok: number;
  failed: number;
  retry: number;
  deferred: number;
  new_articles: number;
  urls: number;
  list_calls: number;
  rate_limited: boolean;
  blocked: boolean;
  verify_hit: boolean;
  env_failure: boolean;
  truncated: boolean;
  note: string | null;
}

/** 一次整机退避的触发 / 解除记录（cooldown_log 行）。 */
export interface CooldownLogRow {
  id: number;
  ts: number;
  /** ip（429 / 5xx / 验证页）/ account（ret=-6/-12）/ clear（手动解除）。 */
  kind: "ip" | "account" | "clear" | string;
  level: number;
  secs: number;
  reason: string;
}

/** 当前退避状态（mpider_core::runstate::CooldownSnapshot）。 */
export interface CooldownSnapshot {
  until: number | null;
  level: number;
  reason: string;
  hits: number;
}

/** 「限流分析」页的整份统计（后端 `ratelimit_stats` 返回体）。 */
export interface RateLimitStats {
  window_secs: number;
  generated_at: number;
  bucket_secs: number;
  totals: OutcomeCounts;
  by_source: SourceCounts[];
  /** 按微信号（uin 短哈希）分组（最近使用排前）：限制是否跟账号走的对照。 */
  by_account: FactorCounts[];
  buckets: RateBucket[];
  peak_bucket_calls: number;
  gaps: GapStats;
  latency: LatencyStats;
  passes: PassSummary[];
  runs: RunLogRow[];
  /** 老版本手动任务的执行单元数（留档）。 */
  runs_manual: number;
  runs_sweep: number;
  cooldowns: CooldownLogRow[];
  cooldown_hits: number;
  block_hits: number;
  cooldown_total_secs: number;
  cooldown_now: CooldownSnapshot;
  /** 今日（本地日历日）列表请求数，仅展示。 */
  today_calls: number;
  /** 当前微信号近 24 小时列表请求数（预算的计数口径）与每号预算（0 = 不限）。 */
  uin_calls_24h: number;
  daily_budget: number;
}

/** 限流分析统计：最近 windowSecs 秒（默认 24 小时）。 */
export const ratelimitStats = (windowSecs: number) =>
  invoke<RateLimitStats>("ratelimit_stats", { windowSecs });

/** 限流分析原始数据导出结果（后端 `RateLimitExport`）。 */
export interface RateLimitExport {
  path: string;
  list_calls: number;
  run_logs: number;
  cooldown_logs: number;
}
/** 导出窗口内三张留档表（list_call_log 含影响因素列 / run_log / cooldown_log）为一个 JSON 文件，供离线分析。 */
export const ratelimitExport = (windowSecs: number) =>
  invoke<RateLimitExport>("ratelimit_export", { windowSecs });

/** 飞书通知运行状态（对齐 mpider_core::notify::NotifyStatus）。 */
export interface NotifyStatus {
  enabled: boolean;
  /** Webhook 填了且形状合法。 */
  configured: boolean;
  queued: number;
  sent: number;
  failed: number;
  last_error: string | null;
  last_sent_at: number | null;
}

/** 应用飞书通知配置（幂等）：应用启动与保存配置时调用；只报异常，启动本身不推。 */
export const notifyApply = (cfg: RealRunConfig) =>
  invoke<void>("notify_apply", { cfg });
/** 用表单里的配置发一条测试消息（可未保存）。 */
export const notifyTest = (cfg: RealRunConfig) =>
  invoke<void>("notify_test", { cfg });
/** 通知运行状态。 */
export const notifyStatus = () => invoke<NotifyStatus>("notify_status");


/** 单号「重新巡检」结果（后端 mpider_core::RetryOutcome）。 */
export interface RetryOutcome {
  /** true = 已采到该号最新列表（直采或接力换 key 后采）；false = 本次没采到（原因见 message）。 */
  collected: boolean;
  /** true = 凭证不可用、走了立即接力（起代理 / 开微信换 key）；false = 凭证有效直采。 */
  relayed: boolean;
  message: string;
  total: number;
  new_articles: number;
  last_published_at: number | null;
}
/** 单号重新巡检（立即执行）：凭证有效直接采最新列表；不可用则立刻单链接接力换 key 再采。另一条整链在跑时后端拒绝。 */
export const sweepRetryAccount = (biz: string, cfg: RealRunConfig) =>
  invoke<RetryOutcome>("sweep_retry_account", { biz, cfg });

// ============ 批量添加公众号（「公众号列表」页）============

/** 一行链接的处理结果：added 新建 / updated 已存在并更新 / rejected 拒绝 / skipped 未处理（前面命中验证页）。 */
export type AddLinkStatus = "added" | "updated" | "rejected" | "skipped";
export interface AddLinkResult {
  line: string;
  status: AddLinkStatus;
  biz: string | null;
  nickname: string | null;
  reason: string | null;
}
/** `add_links` 返回：各状态计数 + 逐行结果。 */
export interface AddLinksSummary {
  added: number;
  updated: number;
  rejected: number;
  skipped: number;
  items: AddLinkResult[];
}
/** `addlinks://progress` 事件载荷。 */
export interface AddLinksProgress {
  done: number;
  total: number;
  current: string;
}
/** 批量添加公众号：每项一条文章**短链**；后端逐条匿名直连文章页解析 biz / 名称建号（不开微信）。 */
export const addLinks = (links: string[]) => invoke<AddLinksSummary>("add_links", { links });

/** 运行状态。 */
export interface RunStatus {
  /** 有执行单元（巡检批次 / 历史页 / 单号重试）正在处理——「重新巡检」据此置灰。 */
  running: boolean;
  /** 主循环（巡检 / 历史模式）是否在跑（单元之间空闲时也为 true）。 */
  loop_running: boolean;
  /** 当前任务阶段名（没有任务在跑为空串）。 */
  phase: string;
}
export const runStatus = () => invoke<RunStatus>("run_status");

// ============ 历史文章抓取（按号长任务，一次只跑一个号）============

/** 历史抓取目标：三项都可空，空 = 不限；全空 = 翻到底。时间戳为秒级。 */
export interface HistoryTarget {
  /** 范围内抓到多少篇即停（含库里已有的）。 */
  count: number | null;
  /** 起始时间：翻到早于它的文章即停。 */
  since_ts: number | null;
  /** 截止时间：晚于它的文章跳过不计，只往前翻。 */
  until_ts: number | null;
}
export type HistoryJobStatus = "running" | "paused" | "done" | "cancelled" | "failed";
export type HistoryPausedReason = "budget" | "blocked" | "credential" | "cooldown" | "user" | "restart";
/** 一条历史任务（后端 history_jobs 表 + 运行态）。 */
export interface HistoryJobView {
  id: number;
  biz: string;
  nickname: string | null;
  status: HistoryJobStatus;
  paused_reason: HistoryPausedReason | null;
  target: HistoryTarget;
  /** 启动时快照的每页条数 / 页间隔（秒）。 */
  page_count: number;
  gap_secs: number;
  /** 已请求页数 / 已抓到篇数（含范围外）/ 范围内篇数 / 新入库篇数。 */
  pages: number;
  fetched: number;
  matched: number;
  new_articles: number;
  next_offset: number;
  /** 下一页计划时刻（秒；running 时有值）。 */
  next_page_at: number | null;
  /** 预算 / 退避暂停时的预计恢复时刻（秒）。 */
  resume_at: number | null;
  started_at: number;
  finished_at: number | null;
  last_error: string | null;
  /** 是否已翻到底。 */
  reached_end: boolean;
}
/** 历史抓取全局状态：当前活动任务（running / paused）+ 当前微信号预算。 */
export interface HistoryStatus {
  job: HistoryJobView | null;
  budget_used: number;
  budget: number;
  budget_reserve: number;
}
/** 启动前的估算。 */
export interface HistoryEstimate {
  /** 预计请求页数；0 = 不限（翻到底）。 */
  pages: number;
  /** 预计耗时（秒）。 */
  seconds: number;
  /** 当前可用预算（扣掉保留后）。 */
  budget_available: number;
  exceeds_budget: boolean;
  note: string;
}
export const historyStart = (biz: string, target: HistoryTarget, cfg: RealRunConfig) =>
  invoke<HistoryJobView>("history_start", { biz, target, cfg });
/** 暂停 / 取消会当场上报一次，所以要带运行配置（上报地址在里面）；status 要用它算预算保留。 */
export const historyPause = (biz: string, cfg: RealRunConfig) =>
  invoke<HistoryJobView>("history_pause", { biz, cfg });
/** 继续：主循环没在跑（如应用重启后）会用这套配置拉起历史模式。 */
export const historyResume = (biz: string, cfg: RealRunConfig) =>
  invoke<HistoryJobView>("history_resume", { biz, cfg });
export const historyCancel = (biz: string, cfg: RealRunConfig) =>
  invoke<HistoryJobView>("history_cancel", { biz, cfg });
export const historyStatus = (cfg: RealRunConfig) => invoke<HistoryStatus>("history_status", { cfg });
export const historyEstimate = (target: HistoryTarget, cfg: RealRunConfig) =>
  invoke<HistoryEstimate>("history_estimate", { target, cfg });

// ============ 全局提醒（预算达上限 / 微信号受限 / 历史任务暂停 / 人工模式请打开文章）============

/** `manual_open`：人工模式（mac / 关闭 RPA）下等凭证时推，提示去微信打开一篇文章；凭证到位或超时后端自动收掉。 */
export type AlertKind = "budget" | "blocked" | "history_paused" | "manual_open";
export interface Alert {
  id: number;
  kind: AlertKind;
  message: string;
  /** 产生时刻（秒）。 */
  at: number;
}
/**
 * 启动自检：上次没正常退出留下的系统代理（指向本机已失效端口）→ 后端直接关掉并返回提示文案；
 * 没有残留返回 null。抓凭证进行中跳过（返回 null）。
 */
export const sysproxyResetStale = (service: string) =>
  invoke<string | null>("sysproxy_reset_stale", { service });

/** 最近一条未确认的提醒（没有为 null）。 */
export const alertLatest = () => invoke<Alert | null>("alert_latest");
export const alertAck = (id: number) => invoke<void>("alert_ack", { id });

/** 一条环节日志事件（`agent://log` 事件载荷，对齐 mpider_core::applog::LogEvent）。 */
export interface LogEvent {
  /** 进程内递增序号（与入库 id 无关）。 */
  seq: number;
  /** epoch 秒。 */
  ts: number;
  level: LogLevel;
  stage: LogStage;
  job_id: number | null;
  /** 已脱敏的消息。 */
  message: string;
  /** 瞬态行（倒计时 / 空转轮询）：只推 GUI、不入库。 */
  transient: boolean;
}

/** 入库的一行环节日志（对齐 mpider_core::model::AppLogRow）。 */
export interface AppLogRow {
  id: number;
  ts: number;
  level: LogLevel;
  stage: LogStage;
  job_id: number | null;
  message: string;
}

export interface LogStats {
  count: number;
  retention_days: number;
}

export interface LogExport {
  path: string;
  count: number;
}

/** 查询入库日志（近 3 天）：sinceSecs 只看最近 N 秒；stage 环节；minLevel warn/error；limit 默认 2000。 */
export const listLogs = (opts: {
  sinceSecs?: number;
  stage?: LogStage | "";
  minLevel?: "" | "warn" | "error";
  limit?: number;
}) =>
  invoke<AppLogRow[]>("list_logs", {
    sinceSecs: opts.sinceSecs ?? null,
    stage: opts.stage || null,
    minLevel: opts.minLevel || null,
    limit: opts.limit ?? null,
  });
export const logStats = () => invoke<LogStats>("log_stats");
/** 清空库里的环节日志（不可恢复）；返回删除行数。 */
export const clearLogsDb = () => invoke<number>("clear_logs");
/** 导出近 3 天日志为文本文件（app_data_dir/logs/），返回路径。 */
export const exportLogs = () => invoke<LogExport>("export_logs");
/** 运行配置导出结果：`text` 是 JSON 文本；`path` 只在写了文件时有值。 */
export interface ConfigExport {
  path: string | null;
  text: string;
}
/**
 * 导出运行配置：后端包一层信封 `{kind:"mpider-config", version, exported_at, app_version, platform, config}`；
 * `toFile` 为真写到 app_data_dir/exports/ 并返回路径，否则只返回文本（复制到剪贴板用）。
 * 文本含上报 token / 飞书 Webhook 与密钥原文，调用方要提醒用户注意保管。
 */
export const exportConfig = (cfg: RealRunConfig, toFile: boolean) =>
  invoke<ConfigExport>("export_config", { cfg, toFile });
/** 在访达 / 资源管理器里定位文件。 */
export const revealInFolder = (path: string) =>
  invoke<void>("reveal_in_folder", { path });

/** 把 epoch 秒格式化为本地 `HH:MM:SS`（日志行）。 */
export function fmtLogTime(ts: number): string {
  const d = new Date(ts * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

/** 把 epoch 秒格式化为本地 `YYYY-MM-DD HH:MM:SS`。 */
export function fmtLogDateTime(ts: number): string {
  const d = new Date(ts * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${fmtLogTime(ts)}`;
}

/** 一条日志事件的单行文本（复制 / 控制面板简版日志）。 */
export function formatLogLine(e: LogEvent): string {
  const lvl = e.level === "warn" ? "⚠ " : e.level === "error" ? "✗ " : "";
  return `${fmtLogTime(e.ts)} [${STAGE_LABELS[e.stage] ?? e.stage}] ${lvl}${e.message}`;
}

/** 补详情的结构化进度（detail://progress 事件载荷）。 */
export interface DetailProgress {
  completed: number;
  total: number;
  done: number;
  failed: number;
  line: string;
}

export interface DetailSummary {
  done: number;
  candidates: number;
  failed: number;
  skipped: number;
  rate_limited: boolean;
  feedback: string;
}

/** 屏幕矩形 [left, top, right, bottom]（物理像素）。 */
export type ScreenRect = [number, number, number, number];

/** 种子点位标定状态：none（未标定）/ ok（有效）/ stale（窗口尺寸或分辨率变了，已失效）。 */
export type SeedMarkState = "none" | "ok" | "stale";

/** RPA 自检结果（对齐 mpider_core::rpa::SelfCheck）。 */
export interface SelfCheck {
  /** 就绪 = 文件助手窗口在 + 种子点位已标定且仍有效。 */
  ready: boolean;
  /** 是否找到「文件传输助手」独立窗口。 */
  fh_found: boolean;
  /** 文件助手窗口矩形（物理像素）。 */
  fh_rect: ScreenRect | null;
  /** 主屏物理分辨率 [宽, 高]。 */
  screen: [number, number] | null;
  /** 文件助手窗口所在显示器的显示缩放百分比（如 100 / 150）；null=取不到。仅诊断，坐标全走物理像素。 */
  scale_pct: number | null;
  /** 种子点位标定状态。 */
  mark: SeedMarkState;
  /** 标定点位按当前窗口换算后的绝对坐标 [x, y]；null = 未标定或已失效。 */
  cached_point: [number, number] | null;
  /** 标定时框选的矩形按当前窗口换算后的绝对坐标；null 同上。 */
  cached_rect: ScreenRect | null;
  /** 人类可读诊断。 */
  message: string;
}

// ============ 命令封装：一处集中，类型对齐后端 ============
export const coreHealth = () => invoke<HealthInfo>("core_health");
/** 公众号列表一页（翻页用：行 + 总数）。 */
export interface AccountPage {
  items: Account[];
  total: number;
}

/** `limit` 省略 = 全量（下拉选择用）；给了则按 limit/offset 翻页。`total` 恒为总数。 */
export const listAccounts = (limit?: number, offset?: number) =>
  invoke<AccountPage>("list_accounts", { limit, offset });

/** credentials 表一行（mpider_core::Credential）：该号最新一份凭证热数据，含参数本体。 */
export interface Credential {
  biz: string;
  uin: string | null;
  key: string | null;
  pass_ticket: string | null;
  wxtoken: string | null;
  x5: string | null;
  appmsg_token: string | null;
  cookie: string | null;
  /** 其余参数（JSON 字符串，原样保存）。 */
  extra: string | null;
  captured_at: number;
  expires_at: number | null;
  invalidated_at: number | null;
  last_used_at: number | null;
  use_count: number;
  refresh_count: number;
}

/** 读取某号凭证完整行；没抓到过返回 null。 */
export const getCredential = (biz: string) =>
  invoke<Credential | null>("get_credential", { biz });
export const certStatus = () => invoke<CertInfo>("cert_status");
export const installCert = () => invoke<CertInstallOutcome>("install_cert");
/** 从系统信任库移除本工具证书（证书文件保留）。返回形状同 install_cert。 */
export const uninstallCert = () => invoke<CertInstallOutcome>("uninstall_cert");
export const systemStatus = (service: string) =>
  invoke<SystemStatus>("system_status", { service });
/** 开始巡检：后台长驻主循环按批巡检全库公众号（有活动的历史抓取任务时 reject）。 */
export const sweepStart = (cfg: RealRunConfig) => invoke<void>("sweep_start", { cfg });
/** 停止巡检：发停止信号（当前批次跑完再退出，期间 `loopStatus().stopping` 为 true）。 */
export const sweepStop = () => invoke<void>("sweep_stop");
/** 主循环 / 巡检开关状态（启动 / 刷新时恢复按钮态；3 秒轮询）。 */
export interface LoopStatus {
  /** 后台主循环（巡检模式 / 历史模式）在跑。 */
  loop_running: boolean;
  /** 巡检开关打开（含停止中）。 */
  sweep_on: boolean;
  /** 已请求停止、等当前批次结束。 */
  stopping: boolean;
}
export const loopStatus = () => invoke<LoopStatus>("loop_status");
/** 固定入口种子链接（「首次设置」教程展示用；单一数据源在 mpider-core）。 */
/** 固定种子链接：本机种子入口服务 `http://<seed_host>:<seed_port>/`（拼法在 mpider-core）。 */
export const seedBootstrapUrl = (seedHost: string, seedPort: number) =>
  invoke<string>("seed_bootstrap_url", { seedHost, seedPort });

/** 常驻种子入口服务的状态（后端 `seedserver::SeedServerStatus`）。 */
export interface SeedServerStatus {
  /** 是否在监听。 */
  running: boolean;
  /** 实际监听地址（如 127.0.0.1:8787）。 */
  addr: string | null;
  /** 种子链接。 */
  url: string;
  /** 最近一次启动失败的原因（端口被占等）；在跑时为空。 */
  last_error: string | null;
  started_at: number | null;
  /** 入口页累计访问次数 / 其中注入了跳转脚本的次数。 */
  hits: number;
  injected: number;
  last_hit_at: number | null;
  /** 最近一次访问是否注入了脚本 / 是否来自微信内置浏览器。 */
  last_hit_injected: boolean | null;
  last_hit_wechat: boolean | null;
  /** 当前接上的队列里待打开的条数（空闲时 0）。 */
  pending: number;
  /** 待命页自轮询是否启用（人工模式：mac / 关闭 RPA）。 */
  resident_mode: boolean;
  /** 微信内置浏览器待命页是否在线（最近 45 秒内有长轮询）。 */
  resident_alive: boolean;
  /** 最近一次待命页长轮询时刻（epoch 秒）。 */
  resident_last_poll_at: number | null;
}
/** 按运行配置确保常驻种子入口服务在跑（幂等；host / 端口变了换绑）。起不来时 reject，原因在错误文案里。 */
export const seedServerApply = (cfg: RealRunConfig) =>
  invoke<SeedServerStatus>("seed_server_apply", { cfg });
export const seedServerStatus = () =>
  invoke<SeedServerStatus>("seed_server_status");
// ============ 任务列表 ============

/** 任务结果分类（jobs.outcome）。 */
export type JobOutcome = "ok" | "timeout" | "server_error" | "error";

/** 任务的一个阶段及其耗时（毫秒），按时间顺序；同名阶段可重复（看门狗重点种子）。 */
export interface JobPhase {
  name: string;
  ms: number;
}

/** 「任务列表」页的一行（只带计数，正文用 getJob 取）。 */
export interface JobListItem {
  id: number;
  /** manual / sweep */
  kind: string;
  /** 流转状态 received|capturing|collecting|reported|done|error */
  status: string;
  /** 结果分类；跑完前 null */
  outcome: JobOutcome | null;
  /** 错误原因 */
  error: string | null;
  received_at: number;
  started_at: number | null;
  finished_at: number | null;
  reported_at: number | null;
  /** 任务链接数 / 回报链接数 / 交回链接数 */
  links: number;
  urls: number;
  remaining: number;
  truncated: boolean;
  /** 任务里的号数 / 已上报成功的号数——每个号列表采完即当场上报（开了数据上报时），列表没获取到的号不报 */
  accounts: number;
  reported_accounts: number;
  report_http_status: number | null;
  /** 各阶段耗时（耗时列 hover）；跑完前为空。 */
  phases: JobPhase[];
}

export interface JobPage {
  items: JobListItem[];
  total: number;
}

/** 一条任务的完整记录（任务数据 / 上报数据弹窗）。 */
export interface JobRow {
  id: number;
  links: string[];
  last_updated_at: number | null;
  link_since: Record<string, number>;
  status: string;
  /** 本地结果 {urls, truncated, remaining_links, accounts}；accounts 每号 {biz, links, urls, finished, reported, report_error}：
   *  采完（finished）的号当场向上报地址 POST 一次（无新文章也报），没采完的号不报 */
  result: unknown | null;
  feedback: string | null;
  received_at: number;
  reported_at: number | null;
  kind: string;
  /** 任务原始报文（一般为 null，老库遗留列） */
  raw: unknown | null;
  started_at: number | null;
  finished_at: number | null;
  outcome: JobOutcome | null;
  report_http_status: number | null;
  phases: JobPhase[];
}

export const listJobs = (kind?: string, limit?: number, offset?: number) =>
  invoke<JobPage>("list_jobs", { kind: kind ?? null, limit, offset });
export const getJob = (id: number) => invoke<JobRow | null>("get_job", { id });
/** 物理删除一条任务记录（不可恢复；进行中的任务后端拒绝）。返回是否真的删掉了。 */
export const deleteJob = (id: number) => invoke<boolean>("delete_job", { id });
/** 一键清空全部已结束的任务记录（不可恢复；进行中的保留）。返回删掉的条数。 */
export const clearJobs = () => invoke<number>("clear_jobs");

export const listArticles = (biz?: string, limit?: number, offset?: number) =>
  invoke<ArticlePage>("list_articles", { biz: biz ?? null, limit, offset });
export const getArticleDetail = (id: number) =>
  invoke<ArticleDetail>("get_article_detail", { id });
export const articleCounts = () => invoke<ArticleCounts>("article_counts");
/** 软删除公众号并级联软删除其全部文章；返回被删除的文章数。 */
export const deleteAccount = (biz: string) =>
  invoke<number>("delete_account", { biz });
/** 物理删除一篇文章（DELETE 行，不可恢复；评论一并删除）；返回是否真的删掉了。 */
export const deleteArticle = (id: number) =>
  invoke<boolean>("delete_article", { id });
/** 用系统默认浏览器打开外部链接（WebView 内 target=_blank 无效）。 */
export const openExternal = (url: string) =>
  invoke<void>("open_external", { url });
/** RPA 拦截探针结果（对齐 mpider_core::probe::InterceptProbe）。 */
export interface InterceptProbe {
  /** 系统代理是否成功设上。 */
  proxy_set_ok: boolean;
  /** 是否成功拦截到 HTTPS（MITM 明文层看到请求）。 */
  intercept_ok: boolean;
  /** 人类可读诊断。 */
  message: string;
}

/** RPA 自检：微信自动化环境是否就绪（启动即自动跑，横线 step 呈现）。 */
export const rpaSelfCheck = () => invoke<SelfCheck>("rpa_self_check");
/**
 * 拦截探针：真设一次系统代理 → 经 MITM 发 HTTPS 验证能否拦截 → 立即复位。
 * 用已安装的 CA，在真跑前暴露「代理设不上 / 拦不下来」。会短暂改动系统代理（RAII 复位）。
 */
export const rpaProxyProbe = (service: string) =>
  invoke<InterceptProbe>("rpa_proxy_probe", { service });

/** 手动抓凭证代理状态（对齐 mpider_core::manualcap::ManualCaptureStatus）。 */
export interface ManualCaptureStatus {
  /** 是否在跑。 */
  running: boolean;
  /** 代理监听地址 127.0.0.1:<port>。 */
  proxy_addr: string | null;
  /** 系统代理是否已指向本代理（set_sysproxy=false 时为 false，需手动设）。 */
  sysproxy_set: boolean;
  /** 开启时刻 / 计划自动停止时刻（epoch 秒）。 */
  started_at: number | null;
  stop_at: number | null;
  /** 最近一次开启 / 停止出错的原因。 */
  last_error: string | null;
}
/** 手动抓凭证代理状态（设置页面板轮询）。 */
export const captureStatus = () =>
  invoke<ManualCaptureStatus>("capture_status");
/**
 * 手动开启抓凭证代理（调试）：不跑任务只起 MITM + 系统代理，期间每条解密到的微信请求路径与
 * profile_ext 响应概况写进「抓凭证」环节日志，抓到的凭证照常入库；minutes 分钟后自动停（默认 10、封顶 60）。
 * 与整链互斥：采集进行中会报错；开着期间「运行一次」/ 轮询领到任务也会报「已有采集在运行」。
 */
export const captureManualStart = (cfg: RealRunConfig, minutes?: number) =>
  invoke<ManualCaptureStatus>("capture_manual_start", { cfg, minutes });
/** 停止手动抓凭证代理（幂等）：复位系统代理 → 停 MITM。 */
export const captureManualStop = () =>
  invoke<ManualCaptureStatus>("capture_manual_stop");
/** 「开始框选」返回：文件助手矩形 + 主屏物理尺寸（框选层据此画提示框）。 */
export interface PickBegin {
  fh_rect: ScreenRect;
  /** 框选层覆盖显示器的物理原点 [x, y]（多屏时非 0,0）；框选层用它把 clientX 换算成绝对物理坐标。 */
  origin: [number, number];
  screen: [number, number];
}
/**
 * RPA 开始框选种子：置顶「文件传输助手」，再在主屏盖一层全屏透明置顶的框选层（`index.html?view=pick`）。
 * 用户在框选层里框住种子链接消息 → 框选层自行调 rpaPickCommit 提交；结果经 `rpa://seed-marked` 事件回到主窗口。
 */
export const rpaPickBegin = (wxId?: number) =>
  invoke<PickBegin>("rpa_pick_begin", { wxId: wxId ?? null });
/** 框选提交结果（对齐 mpider_core::rpa::SeedMark）。 */
export interface SeedMark {
  /** 标定点位（框选中心，绝对物理像素）。 */
  point: [number, number];
  /** 框选矩形（绝对物理像素，左上→右下）。 */
  rect: ScreenRect;
  /** 标定时的文件助手窗口矩形。 */
  fh_rect: ScreenRect;
  /** 缓存文件路径（rpa_click_cache.json）。 */
  cache_path: string;
  /** 人类可读结论。 */
  message: string;
}
/**
 * RPA 提交框选（框选层调用）：`rect` 为屏幕物理像素矩形（任意两角）。成功则后端关掉框选层并广播
 * `rpa://seed-marked`；失败（如框选中心不在文件助手窗口内）reject 且框选层保留，可改框或 Esc 取消。
 */
export const rpaPickCommit = (rect: ScreenRect) =>
  invoke<SeedMark>("rpa_pick_commit", { rect });
/** RPA 取消框选：关掉框选层，主窗口收到 `rpa://seed-pick-cancelled`。 */
export const rpaPickCancel = () => invoke<void>("rpa_pick_cancel");
/** RPA 测试点击结果（对齐 mpider_core::rpa::TestClick）。 */
export interface TestClick {
  /** 点击后是否检测到新的内置浏览器窗口（= 自动点击链路整体成功）。 */
  ok: boolean;
  /** 是否找到「文件传输助手」窗口。 */
  fh_found: boolean;
  /** 文件助手窗口矩形（物理像素）。 */
  fh_rect: ScreenRect | null;
  /** 主屏物理分辨率 [宽, 高]。 */
  screen: [number, number] | null;
  /** 文件助手窗口所在显示器的显示缩放百分比（诊断分辨率/缩放问题）；null=取不到。 */
  scale_pct: number | null;
  /** 本次点击的标定点位 [x, y]（物理像素）；null = 未标定 / 已失效，没点。 */
  point: [number, number] | null;
  /** 是否真的执行了点击。 */
  clicked: boolean;
  /** 点击后新出现的内置浏览器窗口标题。 */
  opened: string[];
  /** 人类可读结论。 */
  message: string;
}
/**
 * RPA 测试点击：不发种子，只「置顶文件助手 → 按标定点位点一次 → 看是否拉起内置浏览器」。
 * 会真的动鼠标并拉起微信内置浏览器（窗口留着不关，便于核对打开的是不是种子页）。
 */
export const rpaTestClick = (wxId?: number) =>
  invoke<TestClick>("rpa_test_click", { wxId: wxId ?? null });

// ---------------- 微信号管理 ----------------
/** 「微信号管理」页的一行（对齐 src-tauri `WxAccountView`）。 */
export interface WxAccountView {
  id: number;
  alias: string;
  note: string | null;
  /** uin 的短哈希（不含原文），页面上作「标识」。 */
  uin_hash: string | null;
  is_active: boolean;
  created_at: number;
  last_captured_at: number | null;
  /** 种子标定状态。 */
  calibrated: "none" | "ok" | "stale";
  /** 该号近 24 小时列表请求数与每号预算（0 = 不限）。 */
  budget_used_24h: number;
  budget: number;
  blocked_until: number | null;
  blocked_reason: string | null;
  /** normal = 正常；budget = 预算用完；blocked = 封号退避中。 */
  status: "normal" | "budget" | "blocked";
}
export const wxList = () => invoke<WxAccountView[]>("wx_list");
export const wxActive = () => invoke<WxAccountView | null>("wx_active");
export const wxUpdate = (id: number, alias: string, note?: string) =>
  invoke<void>("wx_update", { id, alias, note: note ?? null });
export const wxDelete = (id: number) => invoke<void>("wx_delete", { id });
export const wxActivate = (id: number) => invoke<void>("wx_activate", { id });
export const wxUnblock = (id: number) => invoke<void>("wx_unblock", { id });
export const runDetail = (
  biz?: string,
  articleId?: number,
  limit?: number,
  throttleMs?: number,
  workers?: number,
) =>
  invoke<DetailSummary>("run_detail", {
    biz: biz ?? null,
    articleId: articleId ?? null,
    limit,
    throttleMs,
    workers,
  });

// ============ 运行配置持久化（localStorage，key 沿用 mp.cfg）============
export const CFG_KEY = "mp.cfg";

export const DEFAULT_CFG: RealRunConfig = {
  // 数据上报默认关；地址 / token 只存本机 localStorage。
  report_enabled: false,
  report_url: "",
  report_token: "",
  report_timeout_secs: 15,
  capture_port: 0,
  // 种子入口服务：默认只绑本机 127.0.0.1:8787（配成局域网 IP 时文件助手里也能点开）。
  seed_host: "127.0.0.1",
  seed_port: 8787,
  relay_dwell_ms: 2500,
  relay_dwell_max_ms: 4000,
  seed_dwell_ms: 100,
  capture_wait_seconds: 120,
  relay_launch_timeout_seconds: 10,
  relay_stall_seconds: 15,
  relay_max_relaunch: 3,
  list_max_pages: 50,
  page_sleep_min_ms: 3000,
  page_sleep_max_ms: 8000,
  cred_refresh_attempts: 3,
  cred_refresh_wait_seconds: 60,
  // 默认开启：抓取本就依赖系统代理，且 SystemProxyGuard 结束会自动复位。
  set_sysproxy: true,
  sysproxy_service: "Wi-Fi",
  rpa_hard_refresh: false,
  sweep_idle_seconds: 3600,
  sweep_batch_size: 20,
  // 微信号级限制（ret=-6）的节流硬闸：任意两次 getmsg 隔 8–20s、批次间隔 60s、整批失败停 15 分钟。
  // 每号预算 180：观测中账号在近 24 小时累计约 206–224 次 getmsg 时触发限制，取最低值再留一批的余量；
  // 达到后巡检与历史抓取都暂停，见 FAQ 页。
  list_gap_min_ms: 8000,
  list_gap_max_ms: 20000,
  list_daily_budget: 180,
  sweep_batch_gap_seconds: 60,
  sweep_fail_pause_seconds: 900,
  // 历史文章抓取：每页 10 条、页间隔 60 秒、给巡检留 30 次预算。
  history_page_count: 10,
  history_gap_seconds: 60,
  history_budget_reserve: 30,
  // 飞书通知默认关；Webhook / 密钥只存本机 localStorage，不进仓库。
  feishu_enabled: false,
  feishu_webhook: "",
  feishu_secret: "",
  detail_throttle_ms: 2000,
  // 默认串行（1 并发）：让「间隔」名副其实，宁慢勿触发微信限流；求快可调大。
  detail_workers: 1,
};

/** 解析导入的运行配置文本的结果：`cfg` 只含识别出来的字段；其余三组是给用户看的清单。 */
export interface ConfigImport {
  cfg: Partial<RealRunConfig>;
  /** 成功读入的字段名。 */
  applied: string[];
  /** 文本里有、但不是运行配置字段的键（忽略）。 */
  unknown: string[];
  /** 字段名对但类型不对（如数字字段给了非数字文本）的键（忽略）。 */
  badType: string[];
}

/**
 * 解析「导出配置」产出的 JSON 文本（纯函数，不碰后端）：既接受带信封的
 * `{"kind":"mpider-config","config":{…}}`，也接受直接的配置对象。按 `DEFAULT_CFG` 的字段与类型
 * 逐项校验：类型一致直接取；数字字段给了纯数字字符串则转成数字；其它不符的记进 `badType`；
 * 未知键记进 `unknown`。文本不是 JSON 或不是对象时抛错（附带原因）。
 */
export function parseConfigImport(text: string): ConfigImport {
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch (e) {
    throw new Error(
      `不是合法的 JSON：${e instanceof Error ? e.message : String(e)}`,
    );
  }
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    throw new Error("内容不是配置对象");
  }
  let src = raw as Record<string, unknown>;
  if (src.kind === "mpider-config") {
    if (
      !src.config ||
      typeof src.config !== "object" ||
      Array.isArray(src.config)
    ) {
      throw new Error("信封里缺少 config 对象");
    }
    src = src.config as Record<string, unknown>;
  }
  const out: ConfigImport = { cfg: {}, applied: [], unknown: [], badType: [] };
  const defaults = DEFAULT_CFG as unknown as Record<string, unknown>;
  const cfg = out.cfg as Record<string, unknown>;
  for (const [k, v] of Object.entries(src)) {
    if (!(k in defaults)) {
      out.unknown.push(k);
      continue;
    }
    const want = typeof defaults[k];
    if (typeof v === want) {
      cfg[k] = v;
      out.applied.push(k);
    } else if (
      want === "number" &&
      typeof v === "string" &&
      v.trim() !== "" &&
      Number.isFinite(Number(v))
    ) {
      cfg[k] = Number(v);
      out.applied.push(k);
    } else {
      out.badType.push(k);
    }
  }
  if (out.applied.length === 0) {
    throw new Error("没有识别出任何运行配置字段");
  }
  return out;
}

export function loadCfg(): RealRunConfig {
  try {
    const raw = localStorage.getItem(CFG_KEY);
    if (raw) return { ...DEFAULT_CFG, ...JSON.parse(raw) };
  } catch {
    /* 忽略：读不到就用默认 */
  }
  return { ...DEFAULT_CFG };
}

export function persistCfg(cfg: RealRunConfig): void {
  try {
    localStorage.setItem(CFG_KEY, JSON.stringify(cfg));
  } catch {
    /* 忽略 */
  }
}
