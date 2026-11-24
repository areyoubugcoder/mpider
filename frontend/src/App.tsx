/** Windows 工作台外壳：工作台与系统日志入口，启动只读状态，不执行旧代理、证书或 RPA 探测。 */
import { useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { QueueProgressTrigger } from "@/components/QueueProgressTrigger";
import { laboratoryVisible, saveLaboratoryVisible } from "@/lib/ui-settings";
import { toast } from "sonner";
import { DownloadQueueBar } from "@/components/DownloadQueueBar";
import { ExportQueueBar } from "@/components/ExportQueueBar";
import { ProjectLinks } from "@/components/ProjectLinks";
import { AdvertisementCard } from "@/components/AdvertisementCard";
import { UpdateProvider, UpdateIndicator } from "@/components/AppUpdater";
import { AboutDialog } from "@/components/AboutDialog";
import { version as bundledVersion } from "../../package.json";
import { HomeSyncStatus, SessionRecoveryPanel } from "@/components/HomeSync";
import { Sidebar } from "@/components/Sidebar";
import { Toaster } from "@/components/ui/sonner";
import { articleSelectionIds, coreHealth } from "@/lib/tauri";
import { fetchArticleInteractions, fetchArticleComments, type ArticleCollection, type Task, workbenchSnapshot } from "@/lib/workbench";
import { WorkbenchSources, WorkbenchTasks, WorkbenchArticles, WorkbenchRate, WorkbenchSettings } from "@/views/Workbench";
import { CursorLab } from "@/views/CursorLab";
import { SystemLogs } from "@/views/SystemLogs";
import { Faq } from "@/views/Faq";
import { PushMonitor } from "@/views/PushMonitor";
import { pushStatus, pushPhase, pushQueuePhase } from "@/lib/push-monitor";
import type { View } from "@/views/types";

export function App() {
  const [aboutOpen, setAboutOpen] = useState(false);
  const [showLaboratory,setShowLaboratory]=useState(laboratoryVisible);
  function changeLaboratory(visible:boolean){
    try { saveLaboratoryVisible(visible); setShowLaboratory(visible); if(!visible && view==="lab")setView("settings"); }
    catch { toast.error("界面设置保存失败，请重试"); }
  }
  const [view, setView] = useState<View>("accounts");
  const [articleSource,setArticleSource]=useState("");
  const [taskKind,setTaskKind]=useState<Task["kind"]>("home");
  const [logExperiment, setLogExperiment] = useState<number | null>(null);
  const queueLock = useRef(false);
  const [collecting, setCollecting] = useState(false);
  const [progress, setProgress] = useState("");
  const [batchErrors, setBatchErrors] = useState<string[]>([]);
  async function collectArticles(target: { biz: string } | { ids: number[] }, kind: ArticleCollection) {
    const name = kind === "interactions" ? "互动数据" : "评论";
    const fetch = kind === "interactions" ? fetchArticleInteractions : fetchArticleComments;
    if (queueLock.current) return;
    queueLock.current = true;
    setCollecting(true);
    setBatchErrors([]);
    setProgress("正在准备所选文章…");
    try {
      const ids = "biz" in target ? await articleSelectionIds(target.biz) : [...new Set(target.ids)];
      const errors: string[] = [];
      for (const [index, id] of ids.entries()) {
        setProgress(`${name}获取中：${index + 1}/${ids.length}`);
        try {
          await fetch(id);
        } catch (error) {
          errors.push(`文章 ${id}：${String(error)}`);
          setBatchErrors([...errors]);
        }
      }
      setBatchErrors(errors);
      setProgress(ids.length ? `${name}获取结束：成功 ${ids.length - errors.length}/${ids.length}，失败 ${errors.length}` : "没有可采集的文章");
    } catch (error) {
      setProgress(`${name}获取失败`);
      setBatchErrors([String(error)]);
    } finally {
      queueLock.current = false;
      setCollecting(false);
    }
  }
  const health = useQuery({ queryKey: ["health"], queryFn: coreHealth });
  const snapshot = useQuery({ queryKey: ["workbench"], queryFn: workbenchSnapshot, refetchInterval: 2000 });
  const push = useQuery({ queryKey: ["push-monitor"], queryFn: pushStatus, refetchInterval: 2000 });
  return <UpdateProvider><div className="flex h-screen flex-col overflow-hidden bg-background text-foreground">
    <div className="flex min-h-0 flex-1"><Sidebar onAbout={() => setAboutOpen(true)} showLaboratory={showLaboratory} view={view} onNav={next => { if (next === "logs") setLogExperiment(null); if (next === "articles") setArticleSource(""); setView(next); }} />
      <div className="relative flex min-h-0 min-w-0 flex-1 flex-col">
      <main className="min-h-0 min-w-0 flex-1 overflow-y-auto px-6 py-5">
        {snapshot.error && <p role="alert" className="mb-4 rounded-lg border border-destructive p-3 text-sm">无法读取持久化工作台：{String(snapshot.error)}。请检查数据库和后台日志。</p>}
        {!!snapshot.data && snapshot.data.policy.cooldown_until > Date.now() / 1000 && <p role="status" className="mb-4 rounded-lg border p-3 text-sm text-warning">全局冷却至 {new Date(snapshot.data.policy.cooldown_until * 1000).toLocaleString()}，列表、文章下载和图片请求等待；推送解析遵从文章间隔及预算。</p>}
        <SessionRecoveryPanel snapshot={snapshot.data} />
        {!!batchErrors.length && <section role="alert" className="mb-4 rounded-lg border border-warning/40 bg-card p-3 text-sm"><div className="flex items-center justify-between gap-3"><p className="font-medium">部分文章未能获取数据，请按提示操作后重试</p><button type="button" className="shrink-0 text-xs text-link" onClick={() => setBatchErrors([])}>收起提示</button></div><ul className="mt-2 max-h-48 space-y-2 overflow-y-auto text-xs text-muted-foreground">{batchErrors.map((error, i) => <li key={i}>{error}</li>)}</ul></section>}
        {view === "accounts" && <WorkbenchSources collecting={collecting} onCollect={(biz, kind) => collectArticles({ biz }, kind)} onArticles={biz => { setArticleSource(biz); setView("articles"); }} />}
        {view === "push-monitor" && <PushMonitor onRate={() => setView("ratelimit")} onArticles={biz => { setArticleSource(biz); setView("articles"); }} />}
        {view === "jobs" && <WorkbenchTasks kind={taskKind} onKindChange={setTaskKind} />}
        {view === "articles" && <WorkbenchArticles collecting={collecting} onCollect={(ids, kind) => collectArticles({ ids }, kind)} key={articleSource} biz={articleSource} onBizChange={setArticleSource} />}
        {view === "ratelimit" && <WorkbenchRate />}
        {showLaboratory && view === "lab" && <CursorLab onLogs={id => { setLogExperiment(id); setView("logs"); }} />}
        {view === "settings" && <WorkbenchSettings showLaboratory={showLaboratory} onLaboratoryChange={changeLaboratory} />}
        {view === "logs" && <SystemLogs initialExperimentId={logExperiment} />}
        {view === "faq" && <Faq onNav={setView} />}
      </main>
      <AdvertisementCard />
      </div>
    </div>
    <footer className="flex h-10 shrink-0 items-center gap-3 border-t px-3 text-xs text-muted-foreground">
      <ProjectLinks />
      {progress && <span role="status" title={batchErrors.join("\n")}>{progress}</span>}
      <div className="ml-auto flex min-w-0 items-center gap-2"><UpdateIndicator onOpen={() => setView("settings")} /><DownloadQueueBar snapshot={snapshot.data} onTasks={() => { setTaskKind("archive");setView("jobs"); }} />
      <ExportQueueBar />
      {!!snapshot.data?.active_queues?.history.task_count && <QueueProgressTrigger queue={snapshot.data.active_queues.history} label="历史采集" hint="点击查看历史采集任务" onClick={()=>{setTaskKind("history");setView("jobs");}} />}</div>
      <HomeSyncStatus snapshot={snapshot.data} />
      <button type="button" className="min-w-0 max-w-64 truncate hover:text-foreground" onClick={() => setView("push-monitor")} title={`查看推送监测 · 扫描：${pushPhase(push.data?.run)} · 处理：${pushQueuePhase(push.data?.run.queue, push.data?.pending)}`}>扫描：{pushPhase(push.data?.run)} · 处理：{pushQueuePhase(push.data?.run.queue, push.data?.pending)}</button>
    </footer><AboutDialog open={aboutOpen} onOpenChange={setAboutOpen} version={health.data?.app_version || bundledVersion} /><Toaster />
  </div></UpdateProvider>;
}
