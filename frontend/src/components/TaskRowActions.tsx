/** 任务行统一入口：暂停、继续和会话恢复互斥，取消作为次级菜单操作保留。 */
import type { ReactNode } from "react";
import { TextAction } from "@/components/TextAction";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { canCancelTask, taskPrimaryAction } from "@/lib/action-visibility";
import { recoverSession, taskAction, type Source, type Task } from "@/lib/workbench";

type Run = (action: () => Promise<unknown>, message?: string) => Promise<void>;
export function TaskPrimaryAction({ task, source, busy, run, labels = {}, onRetry }: {
  task: Task; source?: Source; busy: boolean; run: Run;
  labels?: Partial<Record<"pause" | "resume" | "recover" | "stop_recovery", string>>;
  onRetry?: () => Promise<unknown>;
}) {
  const action = taskPrimaryAction(task, source);
  if (!action || (action === "retry_download" && !onRetry)) return null;
  const label = action === "retry_download" ? "重试未完成下载" : labels[action] || {
    pause: "暂停", resume: task.state === "failed" ? "重试" : "继续", recover: "更新会话并继续", stop_recovery: "取消自动继续",
  }[action];
  return <TextAction disabled={busy} onClick={() => { void run(() => action === "recover" ? recoverSession(task.id) : action === "retry_download" ? onRetry!() : taskAction(task.id, action === "resume" ? "resume" : "pause")); }}>{label}</TextAction>;
}

export function TaskMoreActions({ tasks, busy, run, children }: { tasks: { task?: Task | null; label: string }[]; busy: boolean; run: Run; children?: ReactNode }) {
  const items = tasks.filter(item => canCancelTask(item.task));
  if (!items.length && !children) return null;
  return <DropdownMenu><DropdownMenuTrigger asChild><TextAction disabled={busy} aria-label={children ? "更多公众号操作" : "更多任务操作"}>更多</TextAction></DropdownMenuTrigger>
    <DropdownMenuContent align="end">{items.map(({ task, label }) => <DropdownMenuItem key={task!.id} disabled={busy} onSelect={() => { void run(() => taskAction(task!.id, "cancel")); }}>{label}</DropdownMenuItem>)}{!!items.length && children && <DropdownMenuSeparator />}{children}</DropdownMenuContent>
  </DropdownMenu>;
}
