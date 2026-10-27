/** 游标有效期实验室：创建计划、查看真实过程、分清有效性证据与环境故障。 */
import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { CheckCircle2, Clock3, FlaskConical, Download, FileText, CircleAlert, Circle } from "lucide-react";
import { toast } from "sonner";
import ReactMarkdown from "react-markdown";
import laboratoryGuide from "../../../docs/laboratory.md?raw";
import { CollapsibleSection } from "@/components/ui/collapsible-section";
import { TextAction } from "@/components/TextAction";
import { Pager, usePaged } from "@/components/Pager";
import { DEFAULT_PAGE_SIZE } from "@/lib/pagination";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardHeader, CardTitle, CardDescription } from "@/components/ui/card";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { revealInFolder } from "@/lib/tauri";
import { workbenchSnapshot, createExperiment, cancelExperiment, experimentProbes, experimentReport, exportExperiment, fetchNextPage, logUiError, type Probe } from "@/lib/workbench";
import { TaskMoreActions, TaskPrimaryAction } from "@/components/TaskRowActions";
import { canCancelTask } from "@/lib/action-visibility";
import { duration, parseSchedule, experimentSummary, plannedChecks, phaseText, outcomeNames, roleNames } from "@/lib/cursor-lab";

const date = (ts?: number | null) => ts ? new Date(ts * 1000).toLocaleString("zh-CN", { hour12: false }) : "—";
const selectClass = "h-9 w-full rounded-md border border-input bg-background px-3 text-sm";
const stateNames: Record<string, string> = { running: "观察中", done: "已结束", cancelled: "已终止", failed: "执行异常" };
const sessionErrors = ["authentication_failed", "session_timeout", "protocol_or_session_error", "invalid_cursor_evidence"];
function Outcome({ probe }: { probe: Probe }) {
  return <Badge variant={probe.outcome === "valid" ? "success" : probe.outcome === "missed" ? "muted" : "warning"}>{outcomeNames[probe.outcome] || probe.outcome}</Badge>;
}
function ProbeEvidence({ probe }: { probe: Probe }) {
  return <div className="mt-2 flex flex-wrap gap-x-5 gap-y-1 text-xs text-muted-foreground">
    <span>{probe.request_id ? `实际发送 ${date(probe.actual_at)}` : `未发送请求 · 登记于 ${date(probe.actual_at)}`}</span>
    {probe.request_id && <span>当时游标年龄 {duration(probe.cursor_age_seconds)}</span>}
    {typeof probe.detail.received_articles === "number" && <span>返回 {probe.detail.received_articles} 篇</span>}
    {probe.detail.business_ret != null && <span>业务返回码 {String(probe.detail.business_ret)}</span>}
    {probe.request_id && <span className="break-all font-mono">请求 {probe.request_id}</span>}
  </div>;
}

export function CursorLab({ onLogs }: { onLogs: (id: number) => void }) {
  const [now, setNow] = useState(() => Date.now() / 1000);
  useEffect(() => { const timer = setInterval(() => setNow(Date.now() / 1000), 1000); return () => clearInterval(timer); }, []);
  const query = useQuery({ queryKey: ["workbench"], queryFn: workbenchSnapshot, refetchInterval: 2000 });
  const data = query.data; const qc = useQueryClient(); const [busy, setBusy] = useState(false);
  const [biz, setBiz] = useState(""); const [preset, setPreset] = useState("observe"); const [schedule, setSchedule] = useState("1,2,4,8,12,24");
  const [originalSession, setOriginalSession] = useState(false); const [creating, setCreating] = useState(false); const [chosen, setChosen] = useState<number | null>(null);
  const source = data?.sources.find(s => s.biz === biz) || data?.sources.find(s => s.current_cursor_id != null);
  const current = data?.cursors.find(c => c.id === source?.current_cursor_id);
  const experiment = chosen !== null ? data?.experiments.find(e => e.id === chosen) : data?.experiments[0];
  const probes = useQuery({ queryKey: ["experiment-probes", experiment?.id], queryFn: () => experimentProbes(experiment!.id), enabled: !!experiment, refetchInterval: 2000 });
  const fixed = data?.cursors.find(c => c.id === experiment?.cursor_id);
  const fixedSource = data?.sources.find(s => s.biz === fixed?.biz);
  const newerSession = !!fixedSource?.session_id && fixedSource.session_id !== experiment?.session_id;
  const future = preset === "now" ? [] : parseSchedule(schedule);
  const duplicate = data?.experiments.find(e => e.state === "running" && e.cursor_id === current?.id);
  const points = experiment ? plannedChecks(experiment, probes.data || []) : [];
  const summary = experiment ? experimentSummary(experiment, probes.data || []) : null;
  const controls = (probes.data || []).filter(p => p.role !== "scheduled");
  const usedRequests = new Set((probes.data || []).map(p => p.request_id).filter(Boolean));
  const inFlight = data?.requests.filter(r => r.owner_kind === "experiment" && r.owner_id === experiment?.id && r.outcome === "pending") || [];
  async function action(fn: () => Promise<unknown>) {
    setBusy(true);
    try { await fn(); await qc.invalidateQueries(); }
    catch (error) { toast.error(String(error)); void logUiError("action_failed", String(error)).catch(() => {}); }
    finally { setBusy(false); }
  }
  function choosePreset(value: string) {
    setPreset(value);
    if (value === "observe") setSchedule("1,2,4,8,12,24");
    if (value === "quick") setSchedule("0.05,0.25,1");
  }
  const [sourceSize,setSourceSize]=useState(DEFAULT_PAGE_SIZE);const sourceListing=usePaged(data?.sources||[],sourceSize);
  const [timelineSize,setTimelineSize]=useState(DEFAULT_PAGE_SIZE);const timelineListing=usePaged(points,timelineSize,experiment?.id);
  const [controlSize,setControlSize]=useState(DEFAULT_PAGE_SIZE);const controlListing=usePaged(controls,controlSize,experiment?.id);
  return <div className="space-y-5">
    <div><h1 className="flex items-center gap-2 text-xl font-semibold"><FlaskConical className="size-5" />游标有效期实验室</h1><p className="mt-1 text-sm text-muted-foreground">固定同一个游标和会话，用实际请求回答“到了这个年龄，它还能不能用”。</p></div>
    <CollapsibleSection title="实验室说明：用途、操作与结果解读" summary="真实请求 · 共享预算" contentClassName="prose prose-sm max-w-none p-4 pt-0 text-sm dark:prose-invert prose-h2:text-base prose-p:my-2 prose-strong:text-foreground">
      <ReactMarkdown>{laboratoryGuide.replace(/^# .*\n/, "").trim()}</ReactMarkdown>
    </CollapsibleSection>
    <p className="text-xs text-warning">实验会发送真实列表请求并占用共享预算；关闭实验室入口不会停止已运行实验。</p>
    <div className="flex flex-wrap gap-2"><Button variant={!creating && !!data?.experiments.length ? "default" : "outline"} onClick={() => setCreating(false)}>实验过程{data?.experiments.length ? ` · ${data.experiments.length}` : ""}</Button><Button variant={creating || data?.experiments.length === 0 ? "default" : "outline"} onClick={() => setCreating(true)}>新建实验</Button></div>
    {(creating || data?.experiments.length === 0) && <>
    <div className="grid gap-3 text-sm sm:grid-cols-3">{[["1", "固定实验对象", "选择公众号，锁定本次游标和会话"], ["2", "按计划验证", "即时基线 → 等待 → 定时复测 → 失败对照"], ["3", "只按证据下结论", "分别显示已证实可用、确认不可复用和无法判断"]].map(([n, title, text]) => <div key={n} className="rounded-lg border p-3"><p className="font-medium"><span className="mr-2 text-muted-foreground">{n}</span>{title}</p><p className="mt-1 text-xs text-muted-foreground">{text}</p></div>)}</div>
    <Card><CardHeader><CardTitle className="text-base">新建实验</CardTitle><CardDescription>实验只保存验证证据，不新增文章、不推进采集游标，也不重置旧游标的获取时间。</CardDescription></CardHeader><CardContent className="space-y-4">
      <div className="grid gap-4 lg:grid-cols-2">
        <div className="space-y-2"><Label htmlFor="lab-source">① 实验公众号</Label><select id="lab-source" className={selectClass} value={source?.biz || ""} onChange={e => setBiz(e.target.value)}><option value="" disabled>请选择已保存游标的公众号</option>{data?.sources.map(s => <option key={s.biz} value={s.biz} disabled={!s.current_cursor_id}>{s.nickname}{!s.current_cursor_id ? " · 尚无可测试游标" : ""}</option>)}</select>
          {current ? <div className="rounded-md bg-muted/40 p-3 text-xs"><p className="font-medium">当前游标 #{current.id} · {current.source_page ? `第 ${current.source_page} 页取得` : "页码未知"}</p><p className="mt-1">当前年龄 {duration(now - current.observed_at)} · 获取于 {date(current.observed_at)}</p><p className="mt-1 text-muted-foreground">这只是当前年龄，是否可用需要实际验证。</p></div> : <p className="text-xs text-muted-foreground">请先在公众号页开始识别，并打开公众号主页准备游标和会话。</p>}
        </div>
        <div className="space-y-2"><Label htmlFor="lab-plan">② 验证计划</Label><select id="lab-plan" className={selectClass} value={preset} onChange={e => choosePreset(e.target.value)}><option value="now">立即检查 · 只验证现在能否使用</option><option value="observe">24 小时观察 · 基线及 1 / 2 / 4 / 8 / 12 / 24 小时</option><option value="quick">快速试验 · 基线及 3 分钟 / 15 分钟 / 1 小时</option><option value="custom">自定义验证时点</option></select>
          {preset !== "now" && <><Label htmlFor="lab-schedule" className="text-xs">实验开始后多少小时验证（最多24个，最大24小时）</Label><Input id="lab-schedule" value={schedule} onChange={e => { setSchedule(e.target.value); setPreset("custom"); }} /><p className="text-xs text-muted-foreground">例如 0.5,1,2 表示实验开始后 30 分钟、1 小时、2 小时；游标原有年龄会另外记录。</p></>}
          {!future && <p role="alert" className="text-xs text-destructive">请输入0到24之间的正数时点，用逗号分隔。即时基线会自动添加。</p>}
        </div>
      </div>
      <details className="text-xs"><summary className="cursor-pointer text-muted-foreground">会话选择与测量边界</summary><div className="mt-2 space-y-2 rounded-md border p-3"><label className="flex items-center gap-2"><input type="checkbox" checked={originalSession} onChange={e => setOriginalSession(e.target.checked)} />使用游标获取时的原会话（默认固定该号最新会话）</label><p>启动后不会中途更换会话。定时复用可能影响服务端有效期；该计划测量定时复用条件下的可用年龄，不能直接当作无人使用时的精确TTL。若只希望间隔一段时间复测，可自定义一个时点。</p></div></details>
      <div className="flex flex-wrap items-center justify-between gap-3 rounded-md border p-3"><div className="text-xs"><p>启动即验证基线{future?.length ? `，随后 ${future.length} 次复测` : ""} · 计划 {1 + (future?.length || 0)} 次列表请求</p><p className="mt-1 text-muted-foreground">失败确认最多追加3次对照；共享预算与间隔。当前24小时预算 {data?.list_calls_24h ?? "—"} / {data?.policy.list_daily_budget ?? "—"}。基线失败且无法判定时结束本次实验。</p></div>
        <Button disabled={busy || !current || !source?.session_id || future === null || !!duplicate} onClick={() => { void action(async () => { const id = await createExperiment(current!.id, future!, originalSession ? current!.session_id : null); setChosen(id); setCreating(false); toast.success("实验已创建，即将开始基线验证"); }); }}>{duplicate ? `该游标的实验 #${duplicate.id} 正在进行` : preset === "now" ? "开始立即检查" : "开始有效期实验"}</Button>
      </div>
      <p className="text-xs text-muted-foreground">关闭窗口后请保持应用在托盘运行；专机休眠、断网、预算不足或会话故障都会影响验证。错过的时点会明确标为缺测。</p>
    </CardContent></Card></>}

    {!creating && !!data?.experiments.length && <Card><CardHeader><CardTitle className="text-base">实验过程与结论</CardTitle><CardDescription>选择一项实验，查看它固定的对象、完整计划和实际请求结果。</CardDescription></CardHeader><CardContent className="space-y-5">
      <select aria-label="选择要查看的实验" className={selectClass} value={experiment?.id || ""} onChange={e => setChosen(Number(e.target.value))}><option value="" disabled>尚无实验</option>{data?.experiments.map(e => { const c = data.cursors.find(c => c.id === e.cursor_id); const s = data.sources.find(s => s.biz === c?.biz); return <option key={e.id} value={e.id}>#{e.id} · {s?.nickname || "公众号"} · 游标 #{e.cursor_id} · {stateNames[e.state] || e.state} · {date(e.started_at)}</option>; })}</select>
      {experiment && fixed && <div className="flex flex-wrap gap-x-8 gap-y-2 rounded-md bg-muted/40 p-3 text-xs"><span>固定公众号 <strong>{fixedSource?.nickname || "公众号"}</strong></span><span>固定游标 <strong>#{fixed.id}</strong> · 获取于 {date(fixed.observed_at)}</span><span>固定会话 <code>{experiment.session_id.slice(0, 12)}</code></span><span>实验开始 {date(experiment.started_at)}</span><span>启动时游标年龄 {duration(experiment.started_at - fixed.observed_at)}</span><span>当前游标年龄 {duration(now - fixed.observed_at)}</span></div>}
      {experiment && fixed && <div className="space-y-2 text-xs text-muted-foreground">
        <p>{fixed.session_id !== experiment.session_id ? "本实验使用不同于游标获取时的会话，成功样本可证明该旧游标跨会话仍可用。" : "本实验使用游标获取时的会话。会话过期只会截断测量，不能据此确定游标到期。"} 当前年龄会持续增加，已证实可用年龄只来自成功请求。</p>
        <p>{newerSession ? `该公众号已保存另一会话（${fixedSource!.session_id.slice(0, 12)}），本实验仍保留原固定会话。` : "更新会话须先到公众号页开始识别，再打开或刷新主页；仅刷新微信页面不会更新已停止识别的软件。"}</p>
        {experiment.state !== "running" && newerSession && !data?.experiments.some(e => e.state === "running" && e.cursor_id === fixed.id) && <Button size="sm" variant="outline" disabled={busy} onClick={() => { void action(async () => { const id = await createExperiment(fixed.id, []); setChosen(id); toast.success("已创建旧游标与最新会话的即时复测"); }); }}>用最新会话复测此旧游标</Button>}
      </div>}
      {probes.error && <p role="alert" className="text-sm text-destructive">验证记录读取失败：{String(probes.error)}。请刷新重试，暂不显示结论。</p>}
      {experiment && probes.isPending && <p className="text-sm text-muted-foreground">正在读取本实验的真实验证记录…</p>}
      {experiment && summary && probes.data && !probes.error && <>
        <div className={`rounded-lg border p-4 ${summary.tone === "destructive" ? "border-destructive/40" : summary.tone === "warning" ? "border-warning/40" : "border-border"}`}>
          <p className={`text-base font-semibold ${summary.tone === "destructive" ? "text-destructive" : summary.tone === "warning" ? "text-warning" : ""}`}>{summary.title}</p><p className="mt-2 text-sm text-muted-foreground">{summary.explanation}</p>
          {experiment.conclusion && <p className="mt-2 text-xs text-muted-foreground">记录结论：{experiment.conclusion}</p>}
        </div>
        <div className="grid gap-3 sm:grid-cols-3"><div className="rounded-md border p-3"><p className="text-xs text-muted-foreground">当前正在做什么</p><p className="mt-2 text-sm font-medium" role="status">{phaseText(experiment, now)}</p>{inFlight.length > 0 && <p className="mt-1 text-xs text-muted-foreground">请求已发送，正在等待响应</p>}</div><div className="rounded-md border p-3"><p className="text-xs text-muted-foreground">即时基线</p><p className="mt-2 text-sm font-medium">{summary.baseline ? outcomeNames[summary.baseline.outcome] || summary.baseline.outcome : experiment.state === "running" ? "等待执行" : "未取得样本"}</p><p className="mt-1 text-xs text-muted-foreground">{summary.baseline?.request_id ? `发送于 ${date(summary.baseline.actual_at)}` : "成功基线是观察有效期的起点"}</p></div><div className="rounded-md border p-3"><p className="text-xs text-muted-foreground">实际请求与计划进度</p><p className="mt-2 text-sm font-medium">已发送 {usedRequests.size + inFlight.length} 次 · 已记录 {points.filter(p => p.probe).length} / {points.length} 个时点</p><p className="mt-1 text-xs text-muted-foreground">缺测 {points.filter(p => p.status === "missed").length} · 对照请求 {controls.filter(p => p.request_id).length} 次</p></div></div>
        <div><div className="mb-3 flex flex-wrap items-center justify-between gap-2"><h3 className="text-sm font-semibold">验证时间线</h3><p className="text-xs text-muted-foreground">计划时间与实际时间分开展示；未发送请求不算失败样本。</p></div>
          <ol className="space-y-3">{timelineListing.slice.map(point => { const p = point.probe; const valid = p?.outcome === "valid"; const inactive = ["pending", "not_run", "missed"].includes(point.status); const Icon = valid ? CheckCircle2 : point.status === "next" ? Clock3 : p && !inactive ? CircleAlert : Circle;
            return <li key={point.index} className="flex gap-3 rounded-lg border p-3"><Icon className={`mt-0.5 size-4 shrink-0 ${valid ? "text-success" : inactive ? "text-muted-foreground" : "text-warning"}`} /><div className="min-w-0 flex-1"><div className="flex flex-wrap items-center justify-between gap-2"><p className="text-sm font-medium">{point.index === 0 ? "即时基线" : `第 ${point.index} 次复测 · 开始后 ${duration(point.hours * 3600)}`}</p>{p ? <Outcome probe={p} /> : <Badge variant="muted">{point.status === "not_run" ? "未执行 · 实验已结束" : point.status === "next" ? "下一验证时点" : "待执行"}</Badge>}</div><p className="mt-1 text-xs text-muted-foreground">计划 {date(point.scheduledAt)}{fixed ? ` · 预计游标年龄 ${duration(point.scheduledAt - fixed.observed_at)}` : ""}</p>{p ? <><ProbeEvidence probe={p} /><p className="mt-2 text-xs">{p.outcome === "valid" ? "原游标取得有效页面，计入已证实可用年龄；不写入正式文章和游标。" : p.outcome === "missed" ? "该时点没有完成请求，不作为游标失效证据。" : sessionErrors.includes(p.outcome) ? "请求在会话或证据层失败，不能判断原游标是否过期。" : p.outcome === "business_error" ? "业务响应拒绝原游标，需要下方对照链区分会话问题与游标不可复用。" : "未取得可判定的页面，不把本次异常当作游标到期。"}</p></> : point.status === "next" && <p className="mt-2 text-xs text-muted-foreground">{phaseText(experiment, now)}</p>}</div></li>;
          })}</ol><Pager label="验证时间线" page={timelineListing.page} total={timelineListing.total} pageSize={timelineSize} onPage={timelineListing.setPage} onPageSize={setTimelineSize} />
        </div>
        {(controls.length > 0 || ["control_home", "control_page", "recheck"].includes(experiment.phase || "")) && <div className="space-y-3 rounded-lg border p-4"><h3 className="text-sm font-semibold">失败后的对照过程</h3><p className="text-xs text-muted-foreground">只有“同一会话首页成功 → 新游标分页成功 → 原游标复测再次被拒绝”才确认原游标不可复用。对照游标成功不计入原游标的可用年龄。</p>{controlListing.slice.map(p => <div key={p.id} className="border-t pt-3"><div className="flex flex-wrap items-center justify-between gap-2"><p className="text-sm">{roleNames[p.role] || p.role}</p><Outcome probe={p} /></div><ProbeEvidence probe={p} /></div>)}<Pager label="对照记录" page={controlListing.page} total={controlListing.total} pageSize={controlSize} onPage={controlListing.setPage} onPageSize={setControlSize} /></div>}
        <div className="flex flex-wrap gap-2"><Button size="sm" variant="outline" disabled={busy} onClick={() => { void action(async () => { const result = await exportExperiment(experiment.id); if (result) toast.success(`实验数据已导出 · ${result.count} 条验证记录`, { description: result.path }); }); }}><Download className="mr-2 size-4" />下载实验数据</Button><Button size="sm" variant="outline" disabled={busy} onClick={() => { void action(async () => revealInFolder(await experimentReport(experiment.id))); }}><FileText className="mr-2 size-4" />生成并打开报告</Button><Button size="sm" variant="outline" onClick={() => onLogs(experiment.id)}>查看本实验系统日志</Button>{experiment.state === "running" && <Button size="sm" variant="outline" disabled={busy} onClick={() => { void action(() => cancelExperiment(experiment.id)); }}>终止实验，保留已取得证据</Button>}</div>
      </>}
      {!experiment && <p className="py-6 text-center text-sm text-muted-foreground">尚无实验。选择上方公众号和计划后，启动第一次基线验证。</p>}
    </CardContent></Card>}

    <details className="rounded-lg border p-4"><summary className="cursor-pointer text-sm font-medium">采集游标与最近翻页结果</summary><p className="my-3 text-xs text-muted-foreground">这里是正式采集操作，会取得新游标并更新当前年龄。实验始终使用启动时固定的旧游标。</p><Table><TableHeader><TableRow>{["公众号 / 当前游标", "最近单页结果", "操作"].map(h => <TableHead key={h}>{h}</TableHead>)}</TableRow></TableHeader><TableBody>{sourceListing.slice.map(s => { const latest = data?.tasks.find(t => t.biz === s.biz && t.kind === "history" && t.target.pages); const active = data?.tasks.find(t => t.biz === s.biz && t.kind === "history" && canCancelTask(t)); return <TableRow key={s.biz}><TableCell>{s.nickname}<p className="text-xs text-muted-foreground">游标 {s.current_cursor_id ? `#${s.current_cursor_id}` : "尚无"}</p></TableCell><TableCell className="text-xs">{latest ? `已获取 ${latest.progress.pages} 页${latest.progress.received_articles != null ? ` · 返回 ${latest.progress.received_articles} 篇` : ""} · 新入库 ${latest.progress.new_articles} 篇` : "尚无单页任务"}</TableCell><TableCell><div className="flex flex-wrap items-center gap-x-3 gap-y-1"><>{active ? <TaskPrimaryAction task={active} source={s} busy={busy} run={action} labels={{pause:"暂停历史采集",resume:"继续历史采集"}} /> : s.current_cursor_id && s.session_id ? <TextAction disabled={busy} onClick={() => { void action(() => fetchNextPage(s.biz)); }}>获取下一页</TextAction> : <span className="text-xs text-muted-foreground">请先登记游标与会话</span>}<TaskMoreActions tasks={[{task:active,label:"取消历史采集"}]} busy={busy} run={action} /></></div></TableCell></TableRow>; })}</TableBody></Table><Pager label="实验游标" page={sourceListing.page} total={sourceListing.total} pageSize={sourceSize} unit="个" onPage={sourceListing.setPage} onPageSize={setSourceSize} /></details>
  </div>;
}
