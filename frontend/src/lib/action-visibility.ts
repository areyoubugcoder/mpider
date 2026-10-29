/** 操作列状态规则：每类任务仅给出一个主操作，正文全部齐备后才允许执行整号导出。 */
import { needsSessionRecovery, recoveryIsWaiting } from "./home-sync.ts";
import type { Source, Task } from "./workbench.ts";

export function canCancelTask(task?: Task | null): task is Task {
  return !!task && !["done", "cancelled"].includes(task.state);
}

export function taskPrimaryAction(task: Task, source?: Source): "pause" | "resume" | "recover" | "stop_recovery" | "retry_download" | null {
  if (["done", "cancelled"].includes(task.state)) {
    return task.state === "done" && task.kind === "archive" && (task.progress.partial > 0 || task.progress.failed > 0) ? "retry_download" : null;
  }
  if (recoveryIsWaiting(task)) return "stop_recovery";
  if (needsSessionRecovery(task, source)) return "recover";
  if (["running", "queued"].includes(task.state)) return "pause";
  if (["paused", "needs_action", "failed"].includes(task.state)) return "resume";
  return null;
}

export function sourceBodyAction(articleCount: number, bodyCount?: number): "download" | "export" | null {
  if (articleCount <= 0 || bodyCount == null || !Number.isInteger(bodyCount) || bodyCount < 0 || bodyCount > articleCount) return null;
  return bodyCount === articleCount ? "export" : "download";
}
