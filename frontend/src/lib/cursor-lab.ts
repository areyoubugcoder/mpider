/** 游标实验的展示规则：只把原游标的真实成功样本计入可用年龄，不从缺测或对照推断。 */
import type { Experiment, Probe } from "./workbench";
export const outcomeNames: Record<string, string> = {
  valid: "验证成功", business_error: "业务拒绝，需对照", session_timeout: "会话已过期", authentication_failed: "会话认证失败", protocol_or_session_error: "会话或协议异常",
  invalid_cursor_evidence: "游标证据不可用", schema_or_identity_error: "响应结构或身份异常", network_or_storage_error: "网络或存储异常", timeout: "请求超时",
  http_error: "HTTP 请求失败", rate_signal: "触发限流", worker_error: "请求环境异常", interrupted: "已中断", missed: "缺测", deferred: "等待请求许可",
};
export const roleNames: Record<string, string> = { scheduled: "计划验证", control_home: "① 同会话获取首页", control_page: "② 新游标分页对照", recheck: "③ 原游标再次验证" };
export function duration(seconds: number) {
  const value = Math.max(0, Math.floor(seconds));
  if (value < 60) return `${value} 秒`;
  if (value < 3600) return `${Math.floor(value / 60)} 分 ${value % 60} 秒`;
  return `${Math.floor(value / 3600)} 小时 ${Math.floor(value % 3600 / 60)} 分`;
}
export function parseSchedule(value: string): number[] | null {
  const parts = value.split(/[,，\s]+/).filter(Boolean).map(Number);
  if (!parts.length || parts.length > 24 || parts.some(h => !Number.isFinite(h) || h <= 0 || h > 24)) return null;
  return [...new Set(parts)].sort((a, b) => a - b);
}
export function experimentSummary(e: Experiment, probes: Probe[]) {
  const successful = probes.filter(p => p.outcome === "valid" && ["scheduled", "recheck"].includes(p.role));
  const lastSuccessAge = successful.length ? Math.max(...successful.map(p => p.cursor_age_seconds)) : null;
  const baseline = probes.find(p => p.role === "scheduled" && Math.abs(p.scheduled_at - e.started_at) < 0.1);
  const blocked = e.verdict === "session_unavailable" || (e.state !== "running" && lastSuccessAge === null && probes.some(p => ["authentication_failed", "session_timeout", "protocol_or_session_error", "invalid_cursor_evidence"].includes(p.outcome)));
  if (e.verdict === "confirmed_unusable") return { title: "已确认原游标不可复用", tone: "destructive" as const, lastSuccessAge, baseline,
    explanation: "同一会话的新游标对照成功，原游标重复失败。只能确定实测范围，不能推算精确到期时刻。" };
  if (blocked) return { title: "会话或证据阻断，无法判断游标有效期", tone: "warning" as const, lastSuccessAge, baseline,
    explanation: "本次请求未得到可用于判定的业务响应。请先到公众号页开始识别，再打开或刷新主页，等新会话保存后复测；原实验和旧游标时间保留。" };
  if (lastSuccessAge !== null) return { title: `已证实：游标年龄 ${duration(lastSuccessAge)} 时仍可用`, tone: "success" as const, lastSuccessAge, baseline,
    explanation: (e.state === "running" ? "这是已取得的成功证据。实验会继续等待后续时点，尚不能确定上限。" : "该年龄是已验证的下界。没有完成失败对照时，不能说游标在此后过期。") + (e.state !== "running" && probes.some(p => ["session_timeout", "authentication_failed", "protocol_or_session_error"].includes(p.outcome)) ? "后续时点因会话过期或不可用停止，未能继续测量。" : "") };
  return { title: e.state === "running" ? "等待建立有效基线" : "尚无可判定的成功样本", tone: "muted" as const, lastSuccessAge, baseline,
    explanation: baseline && baseline.outcome !== "valid" ? "基线没有成功，无法从这次实验推断有效期。失败原因和已发送请求保留在下方。" : "先验证原游标当前可用，再观察它在后续时点是否仍能取得有效页面。" };
}
export function plannedChecks(e: Experiment, probes: Probe[]) {
  return e.schedule_hours.map((hours, index) => {
    const scheduledAt = e.started_at + hours * 3600;
    const probe = probes.find(p => p.role === "scheduled" && Math.abs(p.scheduled_at - scheduledAt) < 0.1);
    return { index, hours, scheduledAt, probe, status: probe ? probe.outcome : e.state !== "running" ? "not_run" : index === e.next_index ? "next" : "pending" };
  });
}
export function phaseText(e: Experiment, now: number) {
  const phase = e.phase || (e.next_index === 0 ? "baseline_pending" : "scheduled_wait");
  const names: Record<string, string> = { baseline_pending: "等待即时基线", scheduled_wait: "等待下一验证时点", requesting: "正在请求原游标页面", gate_wait: "等待列表间隔、预算或冷却", control_home: "正在做同会话首页对照", control_page: "正在做新游标分页对照", recheck: "正在复测原游标", cancelled: "实验已终止", completed: "实验已结束", session_blocked: "会话阻断，实验已停止", confirmed_unusable: "已完成不可复用确认" };
  if (e.state !== "running") return names[e.phase || ""] || (e.state === "cancelled" ? "实验已终止" : "实验已结束");
  const due = e.next_action_at ?? (e.started_at + (e.schedule_hours[e.next_index] ?? 0) * 3600);
  return `${names[phase] || "实验执行中"}${due > now ? ` · ${duration(due - now)}后` : phase === "baseline_pending" || phase === "gate_wait" ? " · 等待调度" : ""}`;
}
