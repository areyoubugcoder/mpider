/** 工作台 IPC 契约：游标原值、会话密钥及保护文件路径不返回到前端。 */
import { invoke } from "@tauri-apps/api/core";
export interface Policy {
  version: number; list_min_ms: number; list_max_ms: number; list_daily_budget: number;
  body_gap_ms: number; image_gap_ms: number; body_workers: number; image_workers: number;
  retries: number; rebuild_limit: number; cooldown_until: number; cooldown_seconds: number;
}
export interface Source {
  biz: string; nickname: string; user_name: string; session_id: string;
  current_cursor_id: number | null; current_cursor_at: number;
  home_cursor_id: number | null; history_cursor_id: number | null; pending_count: number;
  ended: boolean; article_count: number; updated_at: number;
}
export interface CursorRecord {
  id: number; biz: string; session_id: string; parent_id: number | null; source_page: number | null; hash: string;
  observed_at: number; last_success_at: number | null; state: string;
}
export interface Progress {
  session_recovery?: { discovery_id: number; requested_at: number; previous_session_id: string; phase: string } | null;
  blocked_session_id?: string | null;
  received_articles?: number | null;
  completed: number; skipped: number; new_articles: number; pages: number; failed: number; partial: number;
  rebuilds: number; rebuilding: boolean; finished_ids: number[];
}
export interface Task {
  id: number; kind: "history" | "home" | "archive"; biz: string | null; state: string;
  target: { count?: number; cutoff_at?: number; pages?: number; cursor_id?: number; article_ids?: number[]; download_only?: boolean };
  progress: Progress; reason: string | null; next_action_at: number; created_at: number;
  updated_at: number; result_path: string | null;
}
export interface Experiment {
  phase?: string; next_action_at?: number | null;
  verdict?: "at_least_valid" | "confirmed_unusable" | "session_unavailable" | "insufficient_evidence" | null;
  id: number; cursor_id: number; session_id: string; schedule_hours: number[]; next_index: number;
  state: string; started_at: number; deadline_at: number; conclusion: string | null; report_path: string | null;
}
export interface Probe {
  id: number; experiment_id: number; request_id: string | null; role: string; scheduled_at: number;
  actual_at: number; cursor_age_seconds: number; outcome: string; evidence_path: string | null;
  detail: Record<string, unknown>;
}
export interface RequestEvent {
  id: string; category: string; purpose: string; owner_id: number; owner_kind: string; biz: string; started_at: number;
  finished_at: number | null; outcome: string; policy_version: number; gap_ms: number | null;
  detail: Record<string, unknown>;
}
export interface ArchiveRecord { article_id: number; state: string; directory: string; missing_images: string[]; updated_at: number; error: string | null }
export interface Discovery {
  id: number; state: string; target: { count?: number; cutoff_at?: number } | null;
  started_at: number; updated_at: number; reason: string | null; observed_requests: number;
  accounts: { biz: string; observed_at: number; session_ready: boolean; task_id: number | null }[];
}
export interface QueueProgress { task_count: number; completed: number; total: number | null }
export interface Snapshot {
  active_queues: { download: QueueProgress; history: QueueProgress };
  download_queue: { task_count: number; total: number; completed: number; skipped: number; failed: number; cancelled: number; pending: number };
  discovery: Discovery | null;
  sources: Source[]; cursors: CursorRecord[]; tasks: Task[]; experiments: Experiment[];
  requests: RequestEvent[]; policy: Policy; list_calls_24h: number; archives: ArchiveRecord[];
}
export interface ListPage<T> { items: T[]; total: number }
export interface SourceListItem extends Source { body_count: number; home_task: Task | null; history_task: Task | null }
export const sourcePage = (search: string, limit: number, offset: number) => invoke<ListPage<SourceListItem>>("workbench_source_page", {search,limit,offset});
export const taskPage = (kind: Task["kind"], limit: number, offset: number) => invoke<ListPage<Task>>("workbench_task_page", {kind,limit,offset});
export const requestPage = (category: string, since: number, limit: number, offset: number) => invoke<ListPage<RequestEvent>>("workbench_request_page", {category,since,limit,offset});
export const workbenchSnapshot = () => invoke<Snapshot>("workbench_snapshot");
export const savePolicy = (policy: Policy) => invoke<Policy>("workbench_policy", { policy });
export const captureHome = (pythonPath: string, expectedBiz: string | null = null) => invoke<void>("workbench_capture", { pythonPath, expectedBiz });
export const createHistory = (bizzes: string[], target: { count?: number; cutoff_at?: number }) => invoke<number[]>("workbench_history", { bizzes, target });
export const syncSourceHome = (biz: string) => invoke<number>("workbench_sync_home", { biz });
export const fetchNextPage = (biz: string) => invoke<number>("workbench_next_page", { biz });
export const sourceCounts = () => invoke<Record<string, number>>("workbench_source_counts");
export interface DownloadReceipt { task_id: number | null; requested: number; queued: number; already_downloaded: number; already_queued: number }
export const downloadSourceBodies = (biz: string) => invoke<DownloadReceipt>("workbench_archive_source", { biz });
export interface MarkdownExport { directory: string; total: number; with_body: number; missing_body: number; partial_images: number }
export const exportSourceArticles = (biz: string) => invoke<MarkdownExport | null>("workbench_export_source", { biz });
export const taskAction = (id: number, action: "resume" | "pause" | "cancel") => invoke<void>("workbench_task_action", { id, action });
export const createExperiment = (cursorId: number, scheduleHours: number[], sessionId: string | null = null) => invoke<number>("workbench_experiment", { cursorId, scheduleHours, sessionId });
export const cancelExperiment = (id: number) => invoke<void>("workbench_experiment_cancel", { id });
export const experimentProbes = (id: number) => invoke<Probe[]>("workbench_probes", { id });
export const experimentReport = (id: number) => invoke<string>("workbench_report", { id });
export const readArchive = (articleId: number) => invoke<string>("workbench_reader", { articleId });
export const exportRequests = () => invoke<string>("workbench_export_requests");
export const pythonPath = () => localStorage.getItem("mpider-python-path") || "python";

export const stopDiscovery = () => invoke<void>("workbench_discovery_stop");
export const startHomeSync = () => invoke<number>("workbench_home_sync_start", { pythonPath: pythonPath() });
export const recoverSession = (id: number) => invoke<void>("workbench_recover_session", { id, pythonPath: pythonPath() });

export const downloadArticles = (articleIds: number[]) => invoke<DownloadReceipt>("workbench_download_articles", { articleIds });
export const exportArticles = (articleIds: number[]) => invoke<MarkdownExport | null>("workbench_export_articles", { articleIds });
export const deleteArticles = (articleIds: number[]) => invoke<number>("workbench_delete_articles", { articleIds });
export interface SourceDeletion { deleted_articles: number; cancelled_tasks: number; cancelled_experiments: number; cleanup_pending: number }
export const deleteSource = (biz: string) => invoke<SourceDeletion>("workbench_delete_source", { biz });

export interface SystemLogFilter {
  since?: number; until?: number; module?: string; level?: string; keyword?: string; task_id?: number; experiment_id?: number;
}
export interface SystemLogRow {
  id: number; ts: number; level: string; module: string; job_id: number | null;
  message: string; context: Record<string, string | number | boolean | null>;
}
export interface SystemLogPage {
  rows: SystemLogRow[]; total: number; next_before_id: number | null; retention_days: number; max_rows: number;
}
export const systemLogs = (filter: SystemLogFilter, page: number, limit: number) => invoke<SystemLogPage>("workbench_logs", { filter, offset: page * limit, limit, beforeId: null });
export const exportSystemLogs = (filter: SystemLogFilter, format: "json" | "txt") => invoke<{ path: string; count: number } | null>("workbench_export_logs", { filter, format });
export const logUiError = (kind: "uncaught_error" | "unhandled_rejection" | "action_failed", message: string) => invoke<void>("workbench_ui_error", { kind, message: message.slice(0, 4000) });

export const exportExperiment = (id: number) => invoke<{ path: string; count: number } | null>("workbench_export_experiment", { id });

export const articleBodyStates = (articleIds: number[]) => invoke<Record<number,boolean>>("workbench_article_body_states", { articleIds });
