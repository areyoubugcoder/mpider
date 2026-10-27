/** Windows 工作台外壳：工作台与系统日志入口，启动只读状态，不执行旧代理、证书或 RPA 探测。 */
import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { QueueProgressTrigger } from "@/components/QueueProgressTrigger";
import { laboratoryVisible, saveLaboratoryVisible } from "@/lib/ui-settings";
import { toast } from "sonner";
import { DownloadQueueBar } from "@/components/DownloadQueueBar";
import { ProjectLinks } from "@/components/ProjectLinks";
import { UpdateProvider, UpdateIndicator } from "@/components/AppUpdater";
import { AboutDialog } from "@/components/AboutDialog";
import { version as bundledVersion } from "../../package.json";
import { HomeSyncStatus, SessionRecoveryPanel } from "@/components/HomeSync";
import { Sidebar } from "@/components/Sidebar";
import { Toaster } from "@/components/ui/sonner";
import { coreHealth } from "@/lib/tauri";
import { type Task, workbenchSnapshot } from "@/lib/workbench";
import { WorkbenchSources, WorkbenchTasks, WorkbenchArticles, WorkbenchRate, WorkbenchSettings } from "@/views/Workbench";
import { CursorLab } from "@/views/CursorLab";
import { SystemLogs } from "@/views/SystemLogs";
import { Faq } from "@/views/Faq";
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
  const health = useQuery({ queryKey: ["health"], queryFn: coreHealth });
  const snapshot = useQuery({ queryKey: ["workbench"], queryFn: workbenchSnapshot, refetchInterval: 2000 });
  return <UpdateProvider><div className="flex h-screen flex-col overflow-hidden bg-background text-foreground">
    <div className="flex min-h-0 flex-1"><Sidebar onAbout={() => setAboutOpen(true)} showLaboratory={showLaboratory} view={view} onNav={next => { if (next === "logs") setLogExperiment(null); if (next === "articles") setArticleSource(""); setView(next); }} />
      <main className="min-h-0 min-w-0 flex-1 overflow-y-auto px-6 py-5">
        {snapshot.error && <p role="alert" className="mb-4 rounded-lg border border-destructive p-3 text-sm">无法读取持久化工作台：{String(snapshot.error)}。请检查数据库和后台日志。</p>}
        {!!snapshot.data && snapshot.data.policy.cooldown_until > Date.now() / 1000 && <p role="status" className="mb-4 rounded-lg border p-3 text-sm text-warning">全局冷却至 {new Date(snapshot.data.policy.cooldown_until * 1000).toLocaleString()}，各类请求均已等待。</p>}
        <SessionRecoveryPanel snapshot={snapshot.data} />
        {view === "accounts" && <WorkbenchSources onArticles={biz => { setArticleSource(biz); setView("articles"); }} />}
        {view === "jobs" && <WorkbenchTasks kind={taskKind} onKindChange={setTaskKind} />}
        {view === "articles" && <WorkbenchArticles key={articleSource} biz={articleSource} onBizChange={setArticleSource} />}
        {view === "ratelimit" && <WorkbenchRate />}
        {showLaboratory && view === "lab" && <CursorLab onLogs={id => { setLogExperiment(id); setView("logs"); }} />}
        {view === "settings" && <WorkbenchSettings showLaboratory={showLaboratory} onLaboratoryChange={changeLaboratory} />}
        {view === "logs" && <SystemLogs initialExperimentId={logExperiment} />}
        {view === "faq" && <Faq onNav={setView} />}
      </main>
    </div>
    <footer className="flex h-10 shrink-0 items-center gap-3 border-t px-3 text-xs text-muted-foreground">
      <ProjectLinks />
      <div className="ml-auto flex min-w-0 items-center gap-2"><UpdateIndicator onOpen={() => setView("settings")} /><DownloadQueueBar snapshot={snapshot.data} onTasks={() => { setTaskKind("archive");setView("jobs"); }} />
      {!!snapshot.data?.active_queues?.history.task_count && <QueueProgressTrigger queue={snapshot.data.active_queues.history} label="历史采集" hint="点击查看历史采集任务" onClick={()=>{setTaskKind("history");setView("jobs");}} />}</div>
      <HomeSyncStatus snapshot={snapshot.data} />
    </footer><AboutDialog open={aboutOpen} onOpenChange={setAboutOpen} version={health.data?.app_version || bundledVersion} /><Toaster />
  </div></UpdateProvider>;
}
