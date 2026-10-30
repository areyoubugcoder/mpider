/** 公众号工作台：显式历史任务、独立正文任务、限流策略与定时游标实验。 */
import { useEffect, useState, type ReactNode } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import requestPolicyGuide from "../../../docs/request-policy.md?raw";
import { UpdatePanel } from "@/components/AppUpdater";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { DropdownMenuItem } from "@/components/ui/dropdown-menu";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Pager, usePaged, useServerPage } from "@/components/Pager";
import { DEFAULT_PAGE_SIZE } from "@/lib/pagination";
import { Switch } from "@/components/ui/switch";
import { TaskTabs } from "@/components/TaskTabs";
import { TextAction } from "@/components/TextAction";
import { HomeMonitoringCard } from "@/components/HomeSync";
import { TaskMoreActions, TaskPrimaryAction } from "@/components/TaskRowActions";
import { canCancelTask, sourceBodyAction } from "@/lib/action-visibility";
import { relativeTime } from "@/lib/relative-time";
import { recoveryIsWaiting, recoveryMessage } from "@/lib/home-sync";
import { Popover, PopoverTrigger, PopoverContent, PopoverClose } from "@/components/ui/popover";
import { coreHealth, nativeEnvironment, getArticleDetail, openExternal, listArticles, listAccounts, revealInFolder, listLogs, sysproxyResetStale, DEFAULT_CFG } from "@/lib/tauri";
import { logUiError, workbenchSnapshot, articleBodyStates, sourcePage, taskPage, requestPage, downloadArticles, exportArticles, deleteArticles, deleteSource, downloadSourceBodies, exportSourceArticles, syncSourceHome, createHistory, readArchive, savePolicy, exportRequests, pythonPath, type Policy, type Task, type DownloadReceipt, type MarkdownExport } from "@/lib/workbench";

const selectClass = "h-9 rounded-md border border-input bg-background px-3 text-sm";
const names: Record<string, string> = {
  redirect: "文章链接跳转",
  push_metadata: "推送文章解析", invalid_metadata: "文章信息不完整", rate_limited: "请求受限",
  starting: "正在准备", stopping: "正在停止", stopped: "已停止", observed: "已观察", observed_home: "用户原生列表",
  recovering: "正在恢复", queued: "排队中", running: "运行中", paused: "已暂停", needs_action: "待人工处理", done: "已结束", cancelled: "已取消", failed: "失败",
  complete: "正文与图片已保存", partial: "缺图待补齐", valid: "曾验证成功", unverified: "待验证", legacy_unverified: "旧检查点待验证", unusable: "确认不可复用", missed: "缺测",
  ok: "成功", pending: "等待响应", response: "已返回", interrupted: "已中断", business_error: "业务返回异常", authentication_failed: "认证失败",
  verification_required: "需要验证", unavailable: "正文不可用", empty_body: "未解析到正文", invalid_encoding: "编码异常", invalid_image: "图片格式异常", network_error: "网络异常",
  timeout: "超时", session_timeout: "会话已过期", http_error: "HTTP异常", network_or_storage_error: "网络或存储异常", protocol_or_session_error: "协议或会话异常",
  scheduled: "定时验证", control_home: "同会话首页对照", control_page: "新游标分页对照", recheck: "原游标复测",
  home_sync: "首页同步", list: "列表", body: "正文", image: "图片", webpage: "网页解析", home: "首页识别", history: "历史采集", history_rebuild: "历史重建", cursor_experiment: "游标实验",
  history_control_home: "历史首页对照", history_control_page: "历史新游标对照", history_recheck: "历史原游标复测", archive_control: "正文验证对照",
  cursor_control_home: "首页对照", cursor_control_page: "新游标对照", cursor_recheck: "原游标复测", archive: "正文下载", archive_retry: "下载重试",
  rate_signal: "访问频率信号", schema_or_identity_error: "结构或身份不符", worker_error: "运行环境异常",
};
const label = (s: string) => names[s] || s;
const dt = (t?: number | null) => t ? new Date(t * 1000).toLocaleString() : "—";
const useSnapshot = () => useQuery({ queryKey: ["workbench"], queryFn: workbenchSnapshot, refetchInterval: 2000 });
function useAction() {
  const qc = useQueryClient(); const [busy, setBusy] = useState(false);
  async function run(fn: () => Promise<unknown>, message?: string) {
    setBusy(true);
    try { await fn(); await qc.invalidateQueries(); if (message) toast.success(message); }
    catch (e) { toast.error(String(e)); void logUiError("action_failed", String(e)).catch(error => console.error("操作异常日志写入失败", error)); }
    finally { setBusy(false); }
  }
  return { busy, run };
}
function Page({ title, description, children }: { title: string; description: string; children: ReactNode }) {
  return <div className="mx-auto max-w-7xl space-y-5"><header><h1 className="text-xl font-semibold">{title}</h1><p className="mt-1 text-sm text-muted-foreground">{description}</p></header>{children}</div>;
}
function Panel({ title, description, children }: { title: string; description?: string; children: ReactNode }) {
  return <Card><CardHeader><CardTitle className="text-base">{title}</CardTitle>{description && <CardDescription>{description}</CardDescription>}</CardHeader><CardContent className="space-y-4">{children}</CardContent></Card>;
}
function State({ value }: { value: string }) { return <span className={`whitespace-nowrap rounded-full bg-muted px-2 py-1 text-xs ${["failed", "unusable"].includes(value) ? "text-destructive" : ["needs_action", "partial", "paused"].includes(value) ? "text-warning" : "text-foreground"}`}>{label(value)}</span>; }
function Empty({ children }: { children: ReactNode }) { return <p className="py-6 text-center text-sm text-muted-foreground">{children}</p>; }
function Check({ checked, onChange, name }: { checked: boolean; onChange: () => void; name: string }) { return <input type="checkbox" className="size-4 accent-primary" aria-label={name} checked={checked} onChange={onChange} />; }
function toggle(values: number[], id: number) { return values.includes(id) ? values.filter(v => v !== id) : [...values, id]; }

export function WorkbenchSources({onArticles}:{onArticles:(biz:string)=>void}) {
  const [now, setNow] = useState(() => Date.now() / 1000);
  useEffect(() => { const timer = setInterval(() => setNow(Date.now() / 1000), 30000); return () => clearInterval(timer); }, []);
  const snapshot = useSnapshot(); const { busy, run } = useAction();
  const health = useQuery({ queryKey: ["health"], queryFn: coreHealth });
  const [search, setSearch] = useState("");
  const [deletingSource,setDeletingSource]=useState<{biz:string;nickname:string;article_count:number}|null>(null);
  const [historySource, setHistorySource] = useState<{ biz: string; nickname: string } | null>(null);
  const [mode, setMode] = useState("count"); const [count, setCount] = useState(100); const [date, setDate] = useState("");
  const data = snapshot.data; const windows = health.data?.platform === "windows";
  const [page,setPage]=useState(0);const [pageSize,setPageSize]=useState(DEFAULT_PAGE_SIZE);
  const listing=useQuery({queryKey:["source-page",search,page,pageSize],queryFn:()=>sourcePage(search,pageSize,page*pageSize),refetchInterval:2000});
  const rows=listing.data?.items||[];const total=listing.data?.total;
  useServerPage(total,pageSize,page,setPage);
  const validTarget = mode === "count" ? Number.isInteger(count) && count > 0 && count <= 4294967295 : !!date && Number.isFinite(new Date(`${date}T00:00:00`).getTime());
  return <Page title="公众号" description="开启主页识别后，在微信手动打开公众号主页。发现的公众号均显示在列表，文章同步与历史采集按公众号单独操作。">
    <HomeMonitoringCard snapshot={data} windows={windows} />
    <Panel title="公众号列表" description="监测只登记公众号与会话；采集文章、下载正文和导出均通过操作列发起。">
      <Input aria-label="搜索公众号" placeholder="搜索名称或公众号 ID" className="max-w-sm" value={search} onChange={e => {setSearch(e.target.value);setPage(0);}} />
      {snapshot.error && <p role="alert" className="text-sm text-destructive">状态读取失败：{String(snapshot.error)}</p>}
      {rows.length ? <Table><TableHeader><TableRow>{["公众号", "已入库文章", "最近监测 / 采集", "状态", "操作"].map(h => <TableHead key={h}>{h}</TableHead>)}</TableRow></TableHeader><TableBody>{rows.map(s => {
        const articleCount = s.article_count;
        const home = s.home_task;
        const history = s.history_task;
        const pendingHome = canCancelTask(home);
        const pendingHistory = canCancelTask(history);
        const bodyAction = sourceBodyAction(articleCount, s.body_count);
        return <TableRow key={s.biz}>
          <TableCell><p className="font-medium">{s.nickname}</p><p className="max-w-52 break-all font-mono text-xs text-muted-foreground">{s.biz}</p></TableCell>
          <TableCell className="whitespace-nowrap">{`${articleCount} 篇`}</TableCell>
          <TableCell className="whitespace-nowrap text-xs"><time dateTime={Number.isFinite(s.updated_at) && s.updated_at > 0 ? new Date(s.updated_at * 1000).toISOString() : undefined} title={dt(s.updated_at)}>{relativeTime(s.updated_at, now)}</time></TableCell>
          <TableCell><p className="text-xs text-muted-foreground">{s.session_id ? "会话已登记" : "待打开主页"}</p>{home && <p className="mt-1 text-xs">首页同步：{label(home.state)}{home.state === "done" ? ` · 新入库 ${home.progress.new_articles} 篇` : ""}</p>}{history && <p className="mt-1 text-xs">历史采集：{label(history.state)}</p>}{home && recoveryIsWaiting(home) && <p role="status" className="mt-1 max-w-56 text-xs text-warning">{recoveryMessage(home, data?.discovery)}</p>}</TableCell>
          <TableCell><div className="flex flex-wrap items-center gap-x-3 gap-y-1">
            <TextAction disabled={busy} onClick={() => onArticles(s.biz)}>查看文章</TextAction>
            {pendingHome ? <TaskPrimaryAction task={home!} source={s} busy={busy} run={run} labels={{pause:"暂停同步",resume:home!.state === "failed" ? "重试同步" : "继续同步",recover:"更新会话并继续同步",stop_recovery:"取消同步自动继续"}} /> : <TextAction disabled={busy} onClick={() => run(() => syncSourceHome(s.biz), "首页同步任务已加入队列；会话未就绪时将提示打开该公众号主页")}>同步最新文章</TextAction>}
            {pendingHistory ? <TaskPrimaryAction task={history!} source={s} busy={busy} run={run} labels={{pause:"暂停历史采集",resume:history!.state === "failed" ? "重试历史采集" : "继续历史采集",recover:"更新会话并继续历史",stop_recovery:"取消历史自动继续"}} /> : <TextAction disabled={busy} onClick={() => { setMode("count"); setCount(100); setDate(""); setHistorySource({biz:s.biz,nickname:s.nickname}); }}>采集历史文章</TextAction>}

            {bodyAction === "download" && <Popover><PopoverTrigger asChild><TextAction disabled={busy}>下载全部正文</TextAction></PopoverTrigger><PopoverContent aria-label={`下载${s.nickname}全部正文`}>
              <p className="text-sm font-medium">下载全部正文</p><p className="mt-2 text-sm text-muted-foreground">获取本地已经入库的文章正文。</p><p className="mt-2 text-xs text-muted-foreground">共 {articleCount ?? 0} 篇，已下载的文章自动跳过。正文、封面和图片随首次下载保存，队列进度见窗口底部，可在任务页暂停或继续。之后新增文章可再次下载。</p>
              <PopoverClose asChild><Button className="mt-4" size="sm" disabled={busy} onClick={() => run(async () => { showDownloadReceipt(await downloadSourceBodies(s.biz)); })}>开始下载</Button></PopoverClose>
            </PopoverContent></Popover>}
            <Popover><PopoverTrigger asChild><TextAction disabled={busy}>导出所有文章</TextAction></PopoverTrigger><PopoverContent aria-label={`导出${s.nickname}所有文章`}>
              <p className="text-sm font-medium">导出所有文章</p><p className="mt-2 text-sm text-muted-foreground">导出该公众号本地已入库的全部 {articleCount} 篇文章，包含所有分页。同时生成 Markdown 和 HTML，并附带已保存的图片。</p>
              {bodyAction !== null && <p className="mt-2 text-xs text-muted-foreground">已下载正文 {s.body_count} / {articleCount} 篇。</p>}
              {bodyAction === "download" && <p role="status" className="mt-2 text-xs text-warning">还有 {articleCount - s.body_count} 篇正文未下载，请先下载，完成后再导出。</p>}
              {bodyAction === null && <p role="status" className="mt-2 text-xs text-muted-foreground">{articleCount === 0 ? "暂无可导出的文章，请先同步最新文章或采集历史文章。" : "暂时无法确认正文下载状态，请稍后重试。"}</p>}
              <div className="mt-4 flex flex-wrap gap-2">
                {bodyAction === "download" && <PopoverClose asChild><Button size="sm" variant="outline" disabled={busy} onClick={() => run(async () => showDownloadReceipt(await downloadSourceBodies(s.biz)))}>下载未完成正文</Button></PopoverClose>}
                <PopoverClose asChild><Button size="sm" disabled={busy || bodyAction !== "export"} onClick={() => run(async () => showExportResult(await exportSourceArticles(s.biz)))}>选择导出路径</Button></PopoverClose>
              </div>
            </PopoverContent></Popover>
            <TaskMoreActions tasks={[{task:home,label:"取消首页同步"},{task:history,label:"取消历史采集"}]} busy={busy} run={run}><DropdownMenuItem className="text-destructive focus:text-destructive" disabled={busy} onSelect={() => setDeletingSource({biz:s.biz,nickname:s.nickname,article_count:s.article_count})}>删除公众号</DropdownMenuItem></TaskMoreActions>
          </div></TableCell>
        </TableRow>;
      })}</TableBody></Table> : <Empty>{listing.isPending ? "正在读取公众号…" : listing.error ? "公众号读取失败，请重试。" : search ? "没有匹配的公众号。" : "尚无公众号。开启主页识别后，请在微信手动打开公众号主页。"}</Empty>}
      {listing.error && <p role="alert" className="text-sm text-destructive">公众号读取失败：{String(listing.error)}</p>}
      <Pager label="公众号" page={page} total={total??0} pageSize={pageSize} busy={listing.isFetching} onPage={setPage} onPageSize={size=>{setPageSize(size);setPage(0);}} unit="个" />
    </Panel>
    <Dialog open={!!deletingSource} onOpenChange={open => { if (!open && !busy) setDeletingSource(null); }}><DialogContent><DialogHeader><DialogTitle>删除公众号 · {deletingSource?.nickname}</DialogTitle><DialogDescription>将删除该公众号、全部文章正文及本地图片缓存，此操作无法撤销。关联采集与实验将停止，混合下载任务仅移除该号文章；请求、任务历史、受保护会话证据及已导出文件保留。</DialogDescription></DialogHeader><p className="text-sm">当前已入库 {deletingSource?.article_count ?? 0} 篇文章，删除范围以确认执行时的全部文章为准；在微信重新打开公众号主页后，可重新登记该公众号。</p><div className="flex justify-end gap-2"><Button variant="outline" disabled={busy} onClick={() => setDeletingSource(null)}>取消</Button><Button variant="destructive" disabled={busy || !deletingSource} onClick={() => run(async () => { if (!deletingSource) return; const result = await deleteSource(deletingSource.biz); setDeletingSource(null); if (result.cleanup_pending > 0) toast.warning(`已删除公众号及 ${result.deleted_articles} 篇文章`, {description:`仍有 ${result.cleanup_pending} 个本地缓存目录待清理，后台将自动重试。`}); else toast.success(`已删除公众号及 ${result.deleted_articles} 篇文章`); })}>{busy ? "正在删除…" : "删除公众号及全部文章"}</Button></div></DialogContent></Dialog>
    <Dialog open={!!historySource} onOpenChange={open => { if (!open && !busy) setHistorySource(null); }}><DialogContent><DialogHeader><DialogTitle>采集历史文章 · {historySource?.nickname}</DialogTitle><DialogDescription>只为该公众号创建任务，从保存的历史检查点继续；不影响其他公众号或监测开关。</DialogDescription></DialogHeader>
      <div className="space-y-4"><div className="space-y-1"><Label htmlFor="source-history-mode">采集范围</Label><select id="source-history-mode" className={`${selectClass} block w-full`} value={mode} onChange={e => setMode(e.target.value)}><option value="count">向历史追加篇数</option><option value="date">采集至指定日期（含当天）</option></select></div>
      {mode === "count" ? <div className="space-y-1"><Label htmlFor="source-history-count">追加历史篇数</Label><Input id="source-history-count" type="number" min={1} max={4294967295} step={1} value={count} onChange={e => setCount(Number(e.target.value))} /></div> : <div className="space-y-1"><Label htmlFor="source-history-date">截止日期</Label><Input id="source-history-date" type="date" value={date} onChange={e => setDate(e.target.value)} /></div>}
      <p className="text-xs text-muted-foreground">没有新会话时会自动启动监测，并提示你在微信打开该公众号主页，收到会话后自动继续。</p></div>
      <div className="flex justify-end gap-2"><Button variant="outline" disabled={busy} onClick={() => setHistorySource(null)}>取消</Button><Button disabled={busy || !historySource || !validTarget} onClick={() => run(async () => { if (!historySource) return; await createHistory([historySource.biz], mode === "count" ? {count} : {cutoff_at:Math.floor(new Date(`${date}T00:00:00`).getTime()/1000)}); setHistorySource(null); }, "该公众号的历史任务已创建")}>开始采集</Button></div>
    </DialogContent></Dialog>
  </Page>;
}

function taskTarget(t: Task) { return t.kind === "home" ? "最新文章（仅首页）" : t.kind === "archive" ? `${t.target.article_ids?.length ?? 0} 篇文章下载` : t.target.pages ? "获取下一页" : t.target.count ? `追加 ${t.target.count} 篇` : `采集至 ${dt(t.target.cutoff_at)}`; }
function pageResult(t: Task) {
  return `已获取 ${t.progress.pages} 页${t.progress.received_articles != null ? ` · 返回 ${t.progress.received_articles} 篇` : ""} · 新入库 ${t.progress.new_articles} 篇`;
}
export function WorkbenchTasks({kind,onKindChange}:{kind:Task["kind"];onKindChange:(kind:Task["kind"])=>void}) {
  const snapshot = useSnapshot(); const { busy, run } = useAction(); const data = snapshot.data;
  const [page,setPage]=useState(0);const [pageSize,setPageSize]=useState(DEFAULT_PAGE_SIZE);
  useEffect(()=>setPage(0),[kind]);
  const listing=useQuery({queryKey:["task-page",kind,page,pageSize],queryFn:()=>taskPage(kind,pageSize,page*pageSize),refetchInterval:2000});
  useServerPage(listing.data?.total,pageSize,page,setPage);
  return <Page title="任务" description="首页同步、历史列表和正文下载分别管理。一个公众号等待人工处理时，其他已就绪任务可以继续。">
    <TaskTabs value={kind} onChange={next=>{onKindChange(next);setPage(0);}} />
    <div role="tabpanel" id="task-panel" aria-labelledby={`task-tab-${kind}`} aria-label={kind==="home"?"首页同步任务":kind==="history"?"历史采集任务":"正文下载任务"}>
    <Panel title={kind === "home" ? "首页同步任务" : kind === "history" ? "历史文章任务" : "正文下载任务"}>
      {!!listing.data?.items.length && data ? <Table><TableHeader><TableRow>{["任务", "目标", "进度", "状态", "操作"].map(h => <TableHead key={h}>{h}</TableHead>)}</TableRow></TableHeader><TableBody>{listing.data.items.map(t => <TableRow key={t.id}>
        <TableCell><p>#{t.id} {data.sources.find(s => s.biz === t.biz)?.nickname || (kind === "archive" ? "正文下载" : "公众号")}</p><p className="text-xs text-muted-foreground">{dt(t.created_at)}</p></TableCell><TableCell>{taskTarget(t)}</TableCell>
        <TableCell>{kind !== "archive" ? <><p>{pageResult(t)}</p>{kind === "history" && !t.target.pages && <p className="text-xs text-muted-foreground">已计入历史目标 {t.progress.completed} 篇</p>}</> : <p>{t.progress.completed} 篇</p>}{kind === "archive" && <p className="text-xs text-muted-foreground">跳过 {t.progress.skipped ?? 0} · 缺图 {t.progress.partial} · 失败 {t.progress.failed}</p>}{t.progress.rebuilds > 0 && <p className="text-xs text-warning">重建 {t.progress.rebuilds} 次{t.progress.rebuilding ? " · 定位中" : ""}</p>}</TableCell>
        <TableCell><State value={recoveryIsWaiting(t) ? "recovering" : t.state} /><p className="mt-2 max-w-80 text-xs text-muted-foreground">{recoveryIsWaiting(t) ? recoveryMessage(t, data.discovery) : t.reason}</p>{t.state === "running" && t.next_action_at > Date.now() / 1000 && <p className="text-xs text-muted-foreground">下一次不早于 {dt(t.next_action_at)}</p>}</TableCell>
        <TableCell><div className="flex flex-wrap items-center gap-x-3 gap-y-1"><TaskPrimaryAction task={t} source={data.sources.find(s => s.biz === t.biz)} busy={busy} run={run} onRetry={async () => showDownloadReceipt(await downloadArticles(t.target.article_ids || []))} /><TaskMoreActions tasks={[{task:t,label:"取消任务"}]} busy={busy} run={run} /></div></TableCell>
      </TableRow>)}</TableBody></Table> : <Empty>{listing.isPending ? "正在读取任务…" : listing.error ? "任务读取失败，请重试。" : kind === "home" ? "暂无首页同步任务，请在公众号操作列发起。" : kind === "history" ? "暂无历史任务，请在公众号操作列设置范围后创建。" : "暂无正文任务，请在公众号列表或文章库点击下载。"}</Empty>}
      {listing.error && <p role="alert" className="text-sm text-destructive">任务读取失败：{String(listing.error)}</p>}
      <Pager label="任务" page={page} total={listing.data?.total??0} pageSize={pageSize} busy={listing.isFetching} onPage={setPage} onPageSize={size=>{setPageSize(size);setPage(0);}} />
    </Panel></div>
  </Page>;
}

function showDownloadReceipt(result: DownloadReceipt) {
  toast.success(result.queued ? `已加入下载队列 ${result.queued} 篇` : "无需新增下载任务", { description: `已下载跳过 ${result.already_downloaded} 篇 · 已在队列 ${result.already_queued} 篇` });
}
function showExportResult(result: MarkdownExport | null) {
  if (result) toast.success(`已导出 ${result.total} 篇 · Markdown 和 HTML`, { description: result.directory, duration: 12000, action: { label: "打开目录", onClick: () => { void revealInFolder(`${result.directory}/index.html`).catch(e => toast.error(String(e))); } } });
}
export function WorkbenchArticles({biz,onBizChange}:{biz:string;onBizChange:(biz:string)=>void}) {
  const snapshot = useSnapshot(); const { busy, run } = useAction();
  const [page, setPage] = useState(0); const [pageSize,setPageSize]=useState(DEFAULT_PAGE_SIZE); const [selected, setSelected] = useState<number[]>([]);
  const [deleting, setDeleting] = useState<number[] | null>(null);
  const [reader, setReader] = useState<{ title: string; html?: string; markdown?: string } | null>(null);
  const accounts = useQuery({ queryKey: ["article-accounts"], queryFn: () => listAccounts() });
  const articles = useQuery({ queryKey: ["articles", biz, page, pageSize], queryFn: () => listArticles(biz || undefined, pageSize, page * pageSize), refetchInterval: 3000 });
  const rows = articles.data?.items || []; const total = articles.data?.total || 0;
  const selectedStates = useQuery({queryKey:["selected-article-bodies",selected],queryFn:()=>articleBodyStates(selected),enabled:selected.length>0,refetchInterval:selected.length?3000:false});
  const selectionReady=!!selected.length && !!selectedStates.data && selected.every(id=>selectedStates.data[id]===true);
  const missingSelected=selectedStates.data?selected.filter(id=>!selectedStates.data[id]):[];
  useServerPage(articles.isSuccess?total:undefined,pageSize,page,setPage);
  async function read(id: number) {
    const detail = await getArticleDetail(id);
    if (!detail.content_md?.trim()) throw new Error("正文尚未下载，请先点击下载，完成后再阅读");
    const archive = snapshot.data?.archives.find(v => v.article_id === id);
    if (archive && ["complete", "partial"].includes(archive.state)) {
      try { setReader({ title: detail.title || "文章阅读", html: await readArchive(id) }); return; }
      catch { /* 文件不完整时仍可阅读数据库正文，图片不会远程补取。 */ }
    }
    setReader({ title: detail.title || "文章阅读", markdown: detail.content_md });
  }
  return <Page title="文章库" description="下载后即可阅读，并同时导出 Markdown 和 HTML。单篇与勾选下载进入统一队列，整号下载请在公众号列表操作。">
    <div className="flex flex-wrap items-center justify-between gap-3"><div className="flex items-center gap-2"><Label htmlFor="article-source">公众号</Label><select id="article-source" className={selectClass} value={biz} onChange={e => { onBizChange(e.target.value); setPage(0); setSelected([]); }}><option value="">全部公众号</option>{accounts.data?.items.map(s => <option key={s.biz} value={s.biz}>{s.nickname || s.biz}</option>)}</select></div><span className="text-sm text-muted-foreground">共 {total} 篇</span></div>
    {selected.length > 0 && <div className="flex flex-wrap items-center gap-3 rounded-lg border bg-card p-3"><span className="text-sm">已选择 {selected.length} 篇</span>{selectionReady ? <Button size="sm" variant="outline" disabled={busy} onClick={() => run(async () => showExportResult(await exportArticles(selected)))}>导出</Button> : missingSelected.length > 0 ? <Button size="sm" variant="outline" disabled={busy || selectedStates.isFetching} onClick={() => run(async () => showDownloadReceipt(await downloadArticles(missingSelected)))}>下载未完成正文</Button> : <span role="status" className="text-xs text-muted-foreground">{selectedStates.error ? "正文状态读取失败，请重试" : "正在检查正文状态…"}</span>}<Button size="sm" variant="destructive" disabled={busy} onClick={() => setDeleting([...selected])}>删除</Button><Button size="sm" variant="ghost" onClick={() => setSelected([])}>取消选择</Button></div>}
    {articles.error && <p role="alert" className="text-sm text-destructive">文章读取失败：{String(articles.error)}</p>}
    <Card><CardContent className="pt-4"><Table><TableHeader><TableRow><TableHead><Check name="选择本页全部文章" checked={!!rows.length && rows.every(a => selected.includes(a.id))} onChange={() => setSelected(old => rows.every(a => old.includes(a.id)) ? old.filter(id => !rows.some(a => a.id === id)) : Array.from(new Set([...old, ...rows.map(a => a.id)])))} /></TableHead>{["文章", "发布时间", "操作"].map(h => <TableHead key={h}>{h}</TableHead>)}</TableRow></TableHeader><TableBody>{rows.map(a => <TableRow key={a.id}>
      <TableCell><Check name={`选择文章${a.title || a.id}`} checked={selected.includes(a.id)} onChange={() => setSelected(old => toggle(old, a.id))} /></TableCell><TableCell className="max-w-xl"><p className="font-medium">{a.title || "无标题"}</p><p className="mt-1 text-xs text-muted-foreground">{a.account || a.author || ""}</p></TableCell><TableCell className="whitespace-nowrap text-xs">{dt(a.published_at)}</TableCell><TableCell><div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        {a.has_body ? <><TextAction disabled={busy} onClick={() => run(() => read(a.id))}>阅读</TextAction><TextAction disabled={busy} onClick={() => run(async () => showExportResult(await exportArticles([a.id])))}>导出</TextAction></> : <TextAction disabled={busy} onClick={() => run(async () => showDownloadReceipt(await downloadArticles([a.id])))}>下载</TextAction>}
        {a.content_url && <TextAction disabled={busy} onClick={() => run(() => openExternal(a.content_url!))}>查看原文</TextAction>}
      </div></TableCell>
    </TableRow>)}</TableBody></Table>{!rows.length && <Empty>{articles.isPending ? "正在读取文章…" : "尚无文章。请先获取公众号第一页。"}</Empty>}
    <Pager label="文章库" page={page} total={total} pageSize={pageSize} busy={articles.isFetching} unit="篇" onPage={setPage} onPageSize={size=>{setPageSize(size);setPage(0);}} /></CardContent></Card>
    <Dialog open={!!reader} onOpenChange={open => { if (!open) setReader(null); }}><DialogContent className="flex h-[85vh] max-w-4xl flex-col"><DialogHeader><DialogTitle>{reader?.title}</DialogTitle><DialogDescription>阅读本地正文；未保存的远程图片不会自动加载。</DialogDescription></DialogHeader>{reader?.html ? <iframe title="文章阅读" className="min-h-0 flex-1 rounded-lg border bg-white" sandbox="" srcDoc={reader.html} /> : <div className="prose min-h-0 max-w-none flex-1 overflow-auto dark:prose-invert"><ReactMarkdown remarkPlugins={[remarkGfm]} components={{ img: () => <span className="text-sm text-muted-foreground">［图片未保存］</span>, a: ({ href, children }) => <button className="text-link underline" onClick={() => { if (href && /^https?:\/\//.test(href)) void openExternal(href).catch(e => toast.error(String(e))); }}>{children}</button> }}>{reader?.markdown}</ReactMarkdown></div>}</DialogContent></Dialog>
    <Dialog open={!!deleting} onOpenChange={open => { if (!open && !busy) setDeleting(null); }}><DialogContent><DialogHeader><DialogTitle>删除所选 {deleting?.length ?? 0} 篇文章</DialogTitle><DialogDescription>文章将从文章库及后续整号下载、导出中移除。已有文件和任务记录保留。未结束下载任务中的文章需先取消相关任务。</DialogDescription></DialogHeader><div className="flex justify-end gap-2"><Button variant="outline" disabled={busy} onClick={() => setDeleting(null)}>取消</Button><Button variant="destructive" disabled={busy} onClick={() => run(async () => { const removed = await deleteArticles(deleting!); setSelected(old => old.filter(id => !deleting!.includes(id))); setDeleting(null); toast.success(`已删除 ${removed} 篇文章`); })}>确认删除</Button></div></DialogContent></Dialog>
  </Page>;
}

function PolicyForm({ policy }: { policy: Policy }) {
  const [draft, setDraft] = useState(policy); const { busy, run } = useAction();
  const fields: { key: keyof Policy; title: string; seconds?: boolean; min?: number }[] = [
    { key: "list_min_ms", title: "列表最小间隔（秒）", seconds: true }, { key: "list_max_ms", title: "列表最大间隔（秒）", seconds: true }, { key: "list_daily_budget", title: "近24小时列表预算", min: 1 },
    { key: "body_gap_ms", title: "正文请求间隔（秒）", seconds: true }, { key: "image_gap_ms", title: "图片请求间隔（秒）", seconds: true }, { key: "body_workers", title: "正文并发", min: 1 }, { key: "image_workers", title: "图片并发", min: 1 }, { key: "retries", title: "正文/图片重试次数" }, { key: "rebuild_limit", title: "每任务自动重建上限" }, { key: "cooldown_seconds", title: "异常访问冷却（秒）", min: 1 },
  ];
  return <div className="space-y-4">
    <p className="rounded-lg border border-warning/40 p-3 text-sm">系统默认值仅作为初始配置，并非最佳配置或安全阈值。可根据实际需求、网络环境和请求记录自行调整并保存。</p>
    <details className="rounded-lg border p-3"><summary className="cursor-pointer text-sm font-medium">抓取频率与预算说明</summary><div className="prose prose-sm mt-3 max-w-none text-sm dark:prose-invert prose-h2:text-base prose-strong:text-foreground"><ReactMarkdown remarkPlugins={[remarkGfm]}>{requestPolicyGuide.replace(/^# .*\n/, "").trim()}</ReactMarkdown></div></details>
    <div className="grid gap-4 md:grid-cols-3">{fields.map(f => <div className="space-y-1" key={f.key}><Label htmlFor={`policy-${f.key}`}>{f.title}</Label><Input id={`policy-${f.key}`} type="number" min={f.min ?? 0} step={f.seconds ? 0.001 : 1} value={draft[f.key] / (f.seconds ? 1000 : 1)} onChange={e => setDraft({ ...draft, [f.key]: Math.round(Number(e.target.value) * (f.seconds ? 1000 : 1)) })} /></div>)}</div><Button disabled={busy} onClick={() => run(async () => { setDraft(await savePolicy(draft)); }, "策略已保存，将从下一次尚未发送的请求生效")}>保存抓取策略</Button><p className="text-xs text-muted-foreground">列表与实验共享预算；间隔为 0 表示不增加额外等待，并发、预算和访问限制仍生效。</p></div>;
}
export function WorkbenchRate() {
  const snapshot = useSnapshot(); const { busy, run } = useAction(); const [category, setCategory] = useState(""); const data = snapshot.data; const requests = data?.requests || [];
  const [page,setPage]=useState(0);const [pageSize,setPageSize]=useState(DEFAULT_PAGE_SIZE);
  const listing=useQuery({queryKey:["request-page",category,page,pageSize],queryFn:()=>requestPage(category,Date.now()/1000-86400,pageSize,page*pageSize),refetchInterval:2000});
  const filtered=listing.data?.items||[];useServerPage(listing.data?.total,pageSize,page,setPage);
  return <Page title="限流配置" description="按实际发送时间分析请求频率与失败证据。策略可调整，实验、对照、重建和重试均纳入记账。">
    <div className="grid gap-4 md:grid-cols-3">{[["近24小时列表请求", data?.list_calls_24h ?? 0], ["展示样本失败/异常", requests.filter(r => !["ok", "pending", "redirect"].includes(r.outcome)).length], ["当前策略版本", data?.policy.version ?? "—"]].map(([k, v]) => <Card key={k}><CardContent className="pt-5"><p className="text-xs text-muted-foreground">{k}</p><p className="mt-2 text-2xl font-semibold">{v}</p></CardContent></Card>)}</div>
    {data && <Panel title="抓取频率与预算" description="正文和图片各自统一限制发送间隔，增加 worker 不会绕过间隔。"><PolicyForm key={data.policy.version} policy={data.policy} /></Panel>}
    <Panel title="实际请求明细" description="按近24小时完整请求账本分页；导出包含全部已保存请求。原生请求与其镜像日志只计算一次。">
      <div className="flex flex-wrap justify-between gap-2"><select aria-label="筛选请求类别" className={selectClass} value={category} onChange={e => {setCategory(e.target.value);setPage(0);}}><option value="">全部请求</option>{["list", "body", "image", "webpage"].map(c => <option key={c} value={c}>{label(c)}</option>)}</select><Button variant="outline" disabled={busy} onClick={() => run(async () => revealInFolder(await exportRequests()))}>导出请求记录</Button></div>
      <Table><TableHeader><TableRow>{["时间", "类别 / 用途", "结果", "实际间隔", "耗时"].map(h => <TableHead key={h}>{h}</TableHead>)}</TableRow></TableHeader><TableBody>{filtered.map(r => <TableRow key={r.id}><TableCell className="whitespace-nowrap text-xs">{dt(r.started_at)}</TableCell><TableCell>{label(r.category)} · {label(r.purpose)}<p className="text-xs text-muted-foreground">{r.owner_kind === "push_monitor" ? "推送监测" : r.owner_kind === "discovery" ? "主页识别" : r.owner_kind === "experiment" ? "实验" : r.owner_kind === "native" ? "首页识别" : "任务"} #{r.owner_id}</p></TableCell><TableCell><State value={r.outcome} />{r.detail.business_ret != null && <p className="mt-1 font-mono text-xs">业务码 {String(r.detail.business_ret)}</p>}</TableCell><TableCell>{r.gap_ms == null ? "—" : `${(r.gap_ms / 1000).toFixed(3)}s`}</TableCell><TableCell>{r.finished_at ? `${Math.round((r.finished_at - r.started_at) * 1000)}ms` : "—"}</TableCell></TableRow>)}</TableBody></Table>{!filtered.length && <Empty>尚无请求记录。</Empty>}
      {listing.error && <p role="alert" className="text-sm text-destructive">请求读取失败：{String(listing.error)}</p>}
      <Pager label="请求明细" page={page} total={listing.data?.total??0} pageSize={pageSize} busy={listing.isFetching} onPage={setPage} onPageSize={size=>{setPageSize(size);setPage(0);}} />
    </Panel>
  </Page>;
}

export function WorkbenchSettings({showLaboratory,onLaboratoryChange}:{showLaboratory:boolean;onLaboratoryChange:(visible:boolean)=>void}) {
  const health = useQuery({ queryKey: ["health"], queryFn: coreHealth }); const { busy, run } = useAction(); const [python, setPython] = useState(pythonPath); const [showLogs, setShowLogs] = useState(false);
  const logs = useQuery({ queryKey: ["settings-logs"], queryFn: () => listLogs({ limit: 100 }), enabled: showLogs, refetchInterval: showLogs ? 3000 : false });
  useEffect(() => { localStorage.setItem("mpider-python-path", python); }, [python]);
  const [logSize,setLogSize]=useState(DEFAULT_PAGE_SIZE);const recent=usePaged(logs.data||[],logSize,showLogs);
  return <Page title="系统设置" description="抓取频率与预算请到限流配置调整并查看说明。默认值并非最佳配置，可按实际需要自行调整；持续识别需要微信已登录。">
    <UpdatePanel />
    <Panel title="系统配置"><div className="flex items-center justify-between gap-4"><div><Label htmlFor="show-laboratory">显示实验室</Label><p className="mt-1 text-xs text-muted-foreground">用于验证游标复用与会话问题，普通采集无需开启。关闭入口不会停止已有实验；需在实验室内手动终止。</p></div><Switch id="show-laboratory" checked={showLaboratory} onCheckedChange={onLaboratoryChange} /></div></Panel>
    <Panel title="数据与运行"><p className="text-sm">关闭窗口后应用保留在托盘；从托盘退出将停止调度。保持专机运行可完成24小时游标实验。</p><p className="text-sm">下载包含正文、封面和正文图片。视频、音频、评论和阅读互动数据暂不采集。</p><Button variant="outline" onClick={() => run(() => revealInFolder(health.data?.db_path || ""))}>打开数据所在目录</Button></Panel>
    <details className="rounded-xl border bg-card p-4"><summary className="cursor-pointer text-sm font-medium">运行环境与诊断</summary><div className="mt-4 space-y-4"><div className="space-y-1"><Label htmlFor="runtime-python">Python 可执行文件</Label><Input id="runtime-python" value={python} onChange={e => setPython(e.target.value)} /><p className="text-xs text-muted-foreground">默认 python 会优先使用应用旁 runtime/python.exe；首次取样适配微信4.1.15.56。</p></div><Button variant="outline" disabled={busy} onClick={() => run(async () => { const env = await nativeEnvironment(python); if (!env.python || !env.frida || !env.cryptography) throw new Error("缺少运行依赖，请运行 tools/setup-native-runtime.ps1 准备环境"); }, "采集环境依赖已就绪")}>检查运行环境</Button><p className="break-all font-mono text-xs text-muted-foreground">{health.data?.db_path}</p><div className="flex gap-2"><Button variant="outline" onClick={() => setShowLogs(v => !v)}>{showLogs ? "收起日志" : "查看最近日志"}</Button><Button variant="outline" disabled={busy} onClick={() => run(async () => { const result = await sysproxyResetStale(DEFAULT_CFG.sysproxy_service); toast.info(result || "没有发现可清理的旧版本代理残留"); })}>清理旧版本代理残留</Button></div><p className="text-xs text-muted-foreground">旧代理清理只在点击后执行；应用启动不会设置或探测系统代理。</p>{showLogs && <div className="max-h-96 overflow-y-auto rounded-lg bg-muted/40 p-3 font-mono text-xs">{recent.slice.map(l => <p key={l.id} className="mb-2 break-all">{dt(l.ts)} · {l.message}</p>)}<Pager label="最近诊断日志" page={recent.page} total={recent.total} pageSize={logSize} onPage={recent.setPage} onPageSize={setLogSize} /><p className="mt-2 text-xs text-muted-foreground">此处仅展示最近100条；完整筛选请到系统日志。</p></div>}</div></details>
  </Page>;
}
