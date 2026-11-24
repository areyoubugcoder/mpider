/** Windows 工作台外壳：工作台与系统日志入口，启动只读状态，不执行旧代理、证书或 RPA 探测。 */
import { useState } from "react";
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
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription } from "@/components/ui/dialog";
import { Sidebar } from "@/components/Sidebar";
import { Toaster } from "@/components/ui/sonner";
import { coreHealth } from "@/lib/tauri";
import { createCollectionTask, type CollectionTarget, type ArticleCollection, type Task, workbenchSnapshot } from "@/lib/workbench";
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
  const [collecting,setCollecting] = useState(false);
  const [collection,setCollection] = useState<{target:CollectionTarget;kind:ArticleCollection}|null>(null);
  const [refresh,setRefresh] = useState(false);
  async function collectArticles(target:CollectionTarget,kind:ArticleCollection) {setRefresh(false);setCollection({target,kind});}
  async function createCollection() {
    if(!collection)return;
    setCollecting(true);
    try {
      const receipt=await createCollectionTask(collection.kind,collection.target,refresh);
      const kind=collection.kind;
      toast.success(receipt.queued ? `已创建${kind==="comments"?"文章评论":"互动数据"}任务：${receipt.queued} 篇` : "没有需要新增的采集任务",{description:`跳过已获取 ${receipt.skipped} 篇 · 已在任务中 ${receipt.already_queued} 篇`,action:receipt.task_id?{label:"查看任务",onClick:()=>{setTaskKind(kind);setView("jobs");}}:undefined});
      setCollection(null);
    } catch(error) {toast.error(String(error),{duration:15000});}
    finally {setCollecting(false);}
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
        {snapshot.data?.tasks.filter(t=>(t.kind==="interactions"||t.kind==="comments")&&t.state==="needs_action").map(t=><section key={t.id} role="alert" className="mb-3 rounded-lg border border-warning/40 bg-card p-3 text-sm"><div className="flex items-center justify-between gap-3"><p className="font-medium">{t.kind==="comments"?"文章评论":"互动数据"}任务 #{t.id} 需要处理</p><Button size="sm" variant="outline" onClick={()=>{setTaskKind(t.kind);setView("jobs");}}>查看任务</Button></div><p className="mt-2 text-xs text-muted-foreground">{t.reason}</p></section>)}
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
      <div className="ml-auto flex min-w-0 items-center gap-2"><UpdateIndicator onOpen={() => setView("settings")} /><DownloadQueueBar snapshot={snapshot.data} onTasks={() => { setTaskKind("archive");setView("jobs"); }} />
      <ExportQueueBar />
      {!!snapshot.data?.active_queues?.history.task_count && <QueueProgressTrigger queue={snapshot.data.active_queues.history} label="历史采集" hint="点击查看历史采集任务" onClick={()=>{setTaskKind("history");setView("jobs");}} />}</div>
      <HomeSyncStatus snapshot={snapshot.data} />
      <button type="button" className="min-w-0 max-w-64 truncate hover:text-foreground" onClick={() => setView("push-monitor")} title={`查看推送监测 · 扫描：${pushPhase(push.data?.run)} · 处理：${pushQueuePhase(push.data?.run.queue, push.data?.pending)}`}>扫描：{pushPhase(push.data?.run)} · 处理：{pushQueuePhase(push.data?.run.queue, push.data?.pending)}</button>
    </footer><Dialog open={!!collection} onOpenChange={open=>{if(!open&&!collecting)setCollection(null);}}><DialogContent><DialogHeader><DialogTitle>创建{collection?.kind==="comments"?"文章评论":"互动数据"}任务</DialogTitle><DialogDescription>{collection && "biz" in collection.target ? `采集「${snapshot.data?.sources.find(s=>s.biz===(collection.target as {biz:string}).biz)?.nickname || "所选公众号"}」的未删除文章。` : `采集所选 ${collection && "ids" in collection.target ? collection.target.ids.length : 0} 篇文章。`}</DialogDescription></DialogHeader><label className="space-y-2 text-sm"><span className="block">已有数据处理</span><select className="h-9 w-full rounded-md border bg-background px-3" value={String(refresh)} onChange={e=>setRefresh(e.target.value==="true")}><option value="false">仅补采未获取（跳过已完成）</option><option value="true">刷新已有数据，同时补采缺失</option></select></label><p className="text-xs text-muted-foreground">任务创建后可在任务页暂停、继续或取消。评论刷新会比对已有留言，只保存变化并补齐缺失回复。</p><div className="flex justify-end gap-2"><Button variant="outline" disabled={collecting} onClick={()=>setCollection(null)}>取消</Button><Button disabled={collecting} onClick={()=>void createCollection()}>{collecting?"正在创建…":"创建任务"}</Button></div></DialogContent></Dialog><AboutDialog open={aboutOpen} onOpenChange={setAboutOpen} version={health.data?.app_version || bundledVersion} /><Toaster />
  </div></UpdateProvider>;
}
