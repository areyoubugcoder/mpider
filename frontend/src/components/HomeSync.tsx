/** 跨页面主页同步状态及会话恢复入口；恢复意图与进度均由后台保存。 */
import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { TextAction } from "@/components/TextAction";
import { Button } from "@/components/ui/button";
import { Radio } from "lucide-react";
import { cn } from "@/lib/utils";
import { homeSyncState, needsSessionRecovery, recoveryIsWaiting, recoveryMessage } from "@/lib/home-sync";
import { startHomeSync, stopDiscovery, recoverSession, logUiError, taskAction, type Snapshot, type Source, type Task } from "@/lib/workbench";
function useSyncAction() {
  const [busy, setBusy] = useState(false); const qc = useQueryClient();
  async function run(action: () => Promise<unknown>) { setBusy(true); try { await action(); await qc.invalidateQueries({ queryKey: ["workbench"] }); } catch (e) { toast.error(String(e)); void logUiError("action_failed", String(e)).catch(() => {}); } finally { setBusy(false); } }
  return { busy, run };
}
export function SessionRecoveryButton({ task, source, disabled = false }: { task: Task; source?: Source; disabled?: boolean }) {
  const { busy, run } = useSyncAction(); const waiting = recoveryIsWaiting(task);
  if (!needsSessionRecovery(task, source)) return null;
  return <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
    {!waiting && <TextAction disabled={disabled || busy} onClick={() => { void run(() => recoverSession(task.id)); }}>更新会话并继续</TextAction>}
    {waiting && <TextAction disabled={disabled || busy} onClick={() => { void run(() => taskAction(task.id, "pause")); }}>取消自动继续</TextAction>}
  </div>;
}
export function HomeSyncStatus({ snapshot }: { snapshot?: Snapshot }) {
  const status = snapshot ? homeSyncState(snapshot.discovery) : { text: "读取中", active: false, ready: false };
  const failed = snapshot?.discovery?.state === "failed";
  return <span role="status" title={snapshot?.discovery?.reason || "在公众号页开启或停止主页识别"} className="inline-flex shrink-0 items-center gap-1.5 whitespace-nowrap text-xs">
    <span aria-hidden="true" className={cn("size-1.5 shrink-0 rounded-full", status.ready ? "bg-success" : failed ? "bg-destructive" : status.active ? "bg-warning" : "bg-muted-foreground")} />
    <span>主页识别：<span className={failed ? "text-destructive" : ""}>{status.text}</span></span>
  </span>;
}
/** 列表页的重要前置操作：独立卡片展示真实监测状态，启动后引导用户手动打开主页。 */
export function HomeMonitoringCard({ snapshot, windows }: { snapshot?: Snapshot; windows: boolean }) {
  const { busy, run } = useSyncAction();
  const status = snapshot ? homeSyncState(snapshot.discovery) : { text: "读取中", active: false, ready: false };
  const stopping = snapshot?.discovery?.state === "stopping";
  const failed = snapshot?.discovery?.state === "failed";
  return <section aria-label="主页识别" aria-busy={busy} className={cn("rounded-xl border bg-card p-4", !status.active && "border-warning/40")}>
    <div className="flex flex-wrap items-center justify-between gap-4">
      <div className="flex min-w-0 items-start gap-3">
        <Radio aria-hidden="true" className={cn("mt-0.5 size-5 shrink-0", status.ready ? "text-success" : "text-warning")} />
        <div className="space-y-1.5">
          <div className="flex flex-wrap items-center gap-2"><h2 className="text-base font-semibold">主页识别</h2><span role="status" className={cn("rounded-full bg-muted px-2 py-0.5 text-xs", status.ready ? "text-success" : failed ? "text-destructive" : "text-muted-foreground")}>{status.ready ? "等待打开主页" : status.text}</span></div>
          <p className="text-sm">{!windows ? "请在 Windows 专机开启识别，再在微信中打开公众号主页。" : status.ready ? "请在微信中手动打开公众号主页，识别后会登记公众号与会话。" : status.active ? "正在准备主页识别，请稍候。" : "先开启主页识别，再在微信中手动打开公众号主页。"}</p>
          <p className="text-xs text-muted-foreground">只登记公众号与采集会话，文章同步由下方列表单独发起。持续发现新文章请使用左侧「推送监测」。</p>
        </div>
      </div>
      <Button type="button" className="shrink-0" variant={status.active ? "outline" : "default"} loading={busy} disabled={!snapshot || !windows || stopping}
        onClick={() => { void run(status.active ? stopDiscovery : startHomeSync); }}>
        {busy ? (status.active ? "正在停止…" : "正在开启…") : stopping ? "正在停止…" : status.active ? "停止主页识别" : failed ? "重新开启识别" : "开启主页识别"}
      </Button>
    </div>
    {snapshot?.discovery?.reason && <p role={failed ? "alert" : "status"} className={cn("mt-3 break-words border-t border-border-muted pt-3 text-xs", failed ? "text-destructive" : "text-warning")}>{snapshot.discovery.reason}</p>}
  </section>;
}
export function SessionRecoveryPanel({ snapshot }: { snapshot?: Snapshot }) {
  const tasks = snapshot?.tasks.filter(t => t.state === "needs_action" && needsSessionRecovery(t, snapshot.sources.find(s => s.biz === t.biz))) || [];
  if (!tasks.length) return null;
  return <div className="mb-4 space-y-3 rounded-lg border border-warning/40 bg-card p-3" aria-label="会话更新进度">{tasks.map(task => { const source = snapshot?.sources.find(s => s.biz === task.biz); return <div key={task.id} className="flex flex-wrap items-center justify-between gap-3"><div className="min-w-0"><p className="text-sm font-medium">{source?.nickname || "公众号"} · {recoveryIsWaiting(task) ? (task.progress.session_recovery?.phase === "waiting_home" ? "请打开公众号主页" : "正在准备会话") : "会话需要更新"}</p><p role="status" className="mt-1 text-xs text-muted-foreground">{recoveryMessage(task, snapshot?.discovery)}</p></div><SessionRecoveryButton task={task} source={source} /></div>; })}</div>;
}
