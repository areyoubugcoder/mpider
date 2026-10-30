/** 推送监测与主页识别独立开关；URL 候选与内存内容不经过前端。 */
import { invoke } from "@tauri-apps/api/core";
export interface PushConfig { scan_seconds: number; recent_hours: number; hourly_budget: number; fetch_seconds: number; bizzes: string[] }
export interface PushRun {
  id: number; active: boolean; phase: string; config: PushConfig; started_at: number;
  last_scan_at: number; next_scan_at: number; next_fetch_at: number; processes: number;
  read_bytes: number; rounds: number; reason: string | null;
}
export interface PushEvent {
  id: number; first_seen_at: number; finished_at: number | null; outcome: string;
  nickname: string; title: string; biz: string; article_id: number | null;
  published_at: number | null; reason: string | null;
}
export interface PushStatus { supported: boolean; run: PushRun; pending: number; imported: number; requests_hour: number; events: PushEvent[] }
export const pushStatus = () => invoke<PushStatus>("workbench_push_status");
export const startPushMonitor = (config: PushConfig) => invoke<number>("workbench_push_start", { config });
export const stopPushMonitor = () => invoke<void>("workbench_push_stop");
export const defaultPushConfig: PushConfig = { scan_seconds: 10, recent_hours: 24, hourly_budget: 3600, fetch_seconds: 1, bizzes: [] };
export const pushOutcome = (value: string) => ({
  pending: "等待处理", fetching: "正在解析", imported: "新增入库", existing: "文章已存在", ignored: "已过滤",
  verification_required: "网页要求验证", rate_limited: "请求受限", http_error: "网页请求失败",
  network_error: "网络异常", invalid_metadata: "文章信息不完整", unavailable: "文章不可用",
  link_updated: "已补齐签名，等待重试", redirect_pending: "等待跳转解析", redirect_rejected: "跳转目标不是可校验的文章", redirect_limit: "跳转次数已达上限",
}[value] || value);
export function pushPhase(run?: PushRun) {
  if (!run) return "读取中";
  if (!run.active) return run.phase === "failed" ? "运行异常" : "已停止";
  if (run.phase === "starting") return "正在启动";
  if (run.phase === "waiting_wechat") return "等待微信启动";
  if (run.next_fetch_at > Date.now()/1000) return "监测中 · 网页解析等待中";
  return "监测中";
}
