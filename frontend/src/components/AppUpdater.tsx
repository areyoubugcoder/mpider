/** 应用级更新状态：预下载不阻塞采集，切页后仍可查看进度，安装须用户确认。 */
import { createContext, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { Download, RefreshCw, RotateCw } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { openExternal } from "@/lib/tauri";

interface UpdateStatus {
  current_version: string;
  configured: boolean;
  supported: boolean;
  phase: "idle" | "checking" | "available" | "downloading" | "ready" | "installing" | "up_to_date" | "error";
  version: string | null;
  notes: string | null;
  downloaded_bytes: number;
  total_bytes: number | null;
  error: string | null;
  releases_url: string;
}
type Command = "check" | "download" | "clear" | "install";
const UpdateContext = createContext<{
  status?: UpdateStatus; error: string | null; busy: boolean;
  run: (command: Command) => Promise<void>; requestInstall: () => void;
} | null>(null);
const releasesUrl = "https://github.com/areyoubugcoder/mpider/releases";
const queryKey = ["app-updater"];
const mb = (size: number) => `${(size / 1024 / 1024).toFixed(1)} MB`;

function useUpdater() {
  const context = useContext(UpdateContext);
  if (!context) throw new Error("更新组件缺少应用上下文");
  return context;
}

export function UpdateProvider({ children }: { children: ReactNode }) {
  const client = useQueryClient();
  const [pending, setPending] = useState(false);
  const commandRunning = useRef(false);
  const [error, setError] = useState<string | null>(null);
  const [confirm, setConfirm] = useState(false);
  const checked = useRef(false);
  const state = useQuery({
    queryKey, queryFn: () => invoke<UpdateStatus>("update_status"), enabled: isTauri(), retry: false,
    refetchInterval: query => pending || ["checking", "downloading", "installing"].includes(query.state.data?.phase || "") ? 700 : false,
  });
  const status = state.data;
  const busy = pending || ["checking", "downloading", "installing"].includes(status?.phase || "");
  async function run(command: Command) {
    if (commandRunning.current) return;
    commandRunning.current = true; setPending(true); setError(null);
    try {
      const result = await invoke<UpdateStatus | null>(`update_${command}`);
      if (result) {
        // 旧的轮询响应不能覆盖刚完成操作的状态。
        await client.cancelQueries({ queryKey });
        client.setQueryData(queryKey, result);
      }
      if (command === "check" && result?.phase === "up_to_date") toast.success("当前已是最新版本");
      if (command === "download" && result?.phase === "ready") toast.success("安装包已准备好，可稍后启动更新");
    } catch (e) {
      setError(String(e));
      await client.invalidateQueries({ queryKey });
    } finally { commandRunning.current = false; setPending(false); }
  }
  useEffect(() => {
    if (checked.current || !status) return;
    checked.current = true;
    // 每次启动只检查一次；恢复到预下载状态时不联网、不自动安装。
    if (status.supported && status.configured && status.phase === "idle") void run("check");
  }, [status]);
  return <UpdateContext.Provider value={{ status, busy, error: error || (state.error ? String(state.error) : null), run, requestInstall: () => setConfirm(true) }}>
    {children}
    <Dialog open={confirm} onOpenChange={setConfirm}>
      <DialogContent>
        <DialogHeader><DialogTitle>安装 v{status?.version} 并重启应用？</DialogTitle><DialogDescription>应用将退出并启动更新，正在进行的采集和下载会中断。已保存的公众号、文章、任务进度和配置会保留，建议先暂停任务。</DialogDescription></DialogHeader>
        <DialogFooter><Button variant="outline" onClick={() => setConfirm(false)}>稍后更新</Button><Button disabled={busy || status?.phase !== "ready"} onClick={() => { setConfirm(false); void run("install"); }}>安装并重启</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  </UpdateContext.Provider>;
}

export function UpdatePanel() {
  const { status, error, busy, run, requestInstall } = useUpdater();
  const phase = status?.phase;
  const downloaded = phase === "ready";
  const description = phase === "checking" ? "正在检查 GitHub 上的正式版本…"
    : phase === "downloading" ? "正在预下载安装包，完成后可继续使用，稍后启动更新。"
    : downloaded ? "安装包已下载并通过签名校验。下次打开应用仍可安装，无需重新下载。"
    : phase === "installing" ? "正在安装更新，应用即将重启…"
    : phase === "available" ? "发现新版本，可先下载安装包，方便时再更新。"
    : phase === "up_to_date" ? "当前已是最新正式版本。"
    : !isTauri() ? "请在桌面应用中检查与安装更新。"
    : status && !status.supported ? "目前仅提供 Windows x64 的版本更新。"
    : status && !status.configured ? "此构建尚未启用应用内更新，可前往 GitHub Releases 获取正式安装包。"
    : "从 GitHub Releases 获取正式版本，安装前会校验安装包签名。";
  return <section className="space-y-4 rounded-xl border bg-card p-5" aria-labelledby="app-update-title">
    <div className="flex flex-wrap items-center justify-between gap-3"><div><h2 id="app-update-title" className="text-base font-semibold">版本更新</h2><p className="mt-1 text-xs text-muted-foreground">当前版本 {status ? `v${status.current_version}` : "—"}{status?.version && ` · 新版本 v${status.version}`}</p></div>
      <Button variant="outline" disabled={!status?.supported || !status?.configured || busy || downloaded} onClick={() => void run("check")}><RefreshCw className={phase === "checking" ? "animate-spin" : ""} />检查更新</Button></div>
    <p role="status" className="text-sm text-muted-foreground">{description}</p>
    {phase === "downloading" && <div className="space-y-2"><Progress aria-label="安装包下载进度" value={status?.total_bytes ? Math.min(100, status.downloaded_bytes / status.total_bytes * 100) : undefined} /><p className="text-xs text-muted-foreground">{mb(status?.downloaded_bytes || 0)}{status?.total_bytes ? ` / ${mb(status.total_bytes)}` : " · 正在下载"}</p></div>}
    {(error || status?.error) && <p role="alert" className="break-words text-sm text-destructive">{error || status?.error}</p>}
    {status?.notes && <details className="text-sm"><summary className="cursor-pointer">更新说明 · v{status.version}</summary><p className="mt-3 max-h-64 overflow-y-auto whitespace-pre-wrap break-words text-xs leading-relaxed text-muted-foreground">{status.notes}</p></details>}
    <div className="flex flex-wrap gap-2">
      {phase === "available" && <Button disabled={busy} onClick={() => void run("download")}><Download />{status?.error ? "重新下载" : "预下载安装包"}</Button>}
      {downloaded && <Button disabled={busy} onClick={requestInstall}><RotateCw />安装并重启</Button>}
      {(downloaded || phase === "error") && <Button variant="outline" disabled={busy} onClick={() => void run("clear")}>清除更新缓存</Button>}
      <Button variant="link" onClick={() => { void openExternal(releasesUrl).catch(e => toast.error("无法打开 GitHub", { description: String(e) })); }}>GitHub 发布记录</Button>
    </div>
  </section>;
}

export function UpdateIndicator({ onOpen }: { onOpen: () => void }) {
  const { status } = useUpdater();
  if (!status || !["available", "ready", "downloading", "installing"].includes(status.phase)) return null;
  const text = status.phase === "ready" ? `v${status.version} 已下载 · 启动更新`
    : status.phase === "downloading" ? `更新下载中 · ${status.total_bytes ? `${Math.min(100, Math.floor(status.downloaded_bytes / status.total_bytes * 100))}%` : mb(status.downloaded_bytes)}`
    : status.phase === "installing" ? "正在安装更新…" : `发现新版本 v${status.version}`;
  return <button type="button" title={text} className="min-w-0 max-w-36 truncate whitespace-nowrap text-xs text-link hover:underline focus-visible:underline" onClick={onOpen}>{text}</button>;
}
