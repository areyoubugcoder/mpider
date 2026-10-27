/** 系统日志：持久化记录的模块、关联标识与分析字段；分页查看和完整筛选导出。 */
import { useDeferredValue, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Pager, useServerPage } from "@/components/Pager";
import { DEFAULT_PAGE_SIZE } from "@/lib/pagination";
import { Download, RefreshCw } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent } from "@/components/ui/card";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { STAGE_LABELS, revealInFolder } from "@/lib/tauri";
import { SYSTEM_LOG_MODULE_LABELS } from "@/lib/system-log-modules";
import { systemLogs, exportSystemLogs, type SystemLogFilter } from "@/lib/workbench";

const levels: Record<string, string> = { info: "信息", warn: "警告", error: "错误", debug: "调试" };
const fields: Record<string, string> = {
  reason: "原因", phase: "实验阶段", next_action_at: "最早执行时刻", verdict: "结论分类",
  biz: "公众号 ID", nickname: "公众号", task_id: "任务 ID", experiment_id: "实验 ID", discovery_id: "识别轮次", article_id: "文章 ID", request_id: "请求 ID",
  cursor_id: "使用游标", next_cursor_id: "返回游标", current_cursor_id: "当前游标", cursor_age_seconds: "请求时游标年龄（秒）", age_reset: "切换新游标计龄",
  received_articles: "返回篇数", new_articles: "新入库篇数", pages: "页数", completed: "已处理篇数", failed: "失败数", skipped: "跳过数", partial: "缺图数",
  requested: "申请篇数", queued: "入队篇数", already_downloaded: "已有正文", already_queued: "已在队列", duration_ms: "耗时（毫秒）", outcome: "结果", state: "状态", previous_state: "原状态",
  purpose: "请求用途", category: "请求类别", policy_version: "策略版本", http_status: "HTTP 状态", business_ret: "业务返回码", outer_ret: "外层返回码",
  list_min_ms: "列表最小间隔（毫秒）", list_max_ms: "列表最大间隔（毫秒）", list_daily_budget: "每日列表预算", body_gap_ms: "正文间隔（毫秒）", image_gap_ms: "图片间隔（毫秒）", cooldown_until: "冷却截止时间", scheduled_at: "计划时间", role: "验证角色", count: "数量", format: "文件格式", reason_code: "原因分类", is_end: "到达末页", source_page: "响应页码",
};
const selectClass = "h-9 rounded-md border border-input bg-background px-3 text-sm";
function dateInput(time: number) {
  const date = new Date(time); return new Date(time - date.getTimezoneOffset() * 60000).toISOString().slice(0, 16);
}

export function SystemLogs({ initialExperimentId = null }: { initialExperimentId?: number | null }) {
  const [module, setModule] = useState(""); const [level, setLevel] = useState("");
  const [since, setSince] = useState(() => dateInput(Date.now() - 86400000)); const [until, setUntil] = useState("");
  const [keyword, setKeyword] = useState(""); const search = useDeferredValue(keyword);
  const [taskId, setTaskId] = useState(""); const [experimentId, setExperimentId] = useState(initialExperimentId?.toString() || ""); const [auto, setAuto] = useState(true);
  const [page,setPage]=useState(0);const [pageSize,setPageSize]=useState(DEFAULT_PAGE_SIZE); const [format, setFormat] = useState<"json" | "txt">("json"); const [exporting, setExporting] = useState(false);
  const filter: SystemLogFilter = { module: module || undefined, level: level || undefined, keyword: search.trim() || undefined,
    since: since ? new Date(since).getTime() / 1000 : undefined, until: until ? new Date(until).getTime() / 1000 : undefined, task_id: taskId ? Number(taskId) : undefined, experiment_id: experimentId ? Number(experimentId) : undefined };
  const invalid = (filter.since !== undefined && !Number.isFinite(filter.since)) || (filter.until !== undefined && !Number.isFinite(filter.until)) || (filter.since !== undefined && filter.until !== undefined && filter.since > filter.until) || (!!taskId && (!Number.isSafeInteger(filter.task_id) || Number(taskId) < 1)) || (!!experimentId && (!Number.isSafeInteger(filter.experiment_id) || Number(experimentId) < 1));
  // 筛选变化立即回到首页；旧页游标不能用于新条件。
  const filterKey = JSON.stringify(filter); const [lastFilter, setLastFilter] = useState(filterKey);
  if (lastFilter !== filterKey) { setLastFilter(filterKey); setPage(0); }
  const currentPage=lastFilter===filterKey?page:0;
  const query = useQuery({ queryKey: ["system-logs", filter, currentPage, pageSize], queryFn: () => systemLogs(filter, currentPage, pageSize), enabled: !invalid,
    refetchInterval: auto && currentPage === 0 ? 2000 : false, retry: 1 });
  useServerPage(query.data?.total,pageSize,currentPage,setPage);
  async function download() {
    setExporting(true);
    try {
      const result = await exportSystemLogs(filter, format);
      if (result) toast.success(`已导出 ${result.count} 条系统日志`, { description: result.path, duration: 10000, action: { label: "打开目录", onClick: () => { void revealInFolder(result.path).catch(e => toast.error(String(e))); } } });
    } catch (error) { toast.error(`日志导出失败：${String(error)}`); }
    finally { setExporting(false); }
  }
  return <div className="space-y-5">
    <div><h1 className="text-xl font-semibold">系统日志</h1><p className="mt-1 text-sm text-muted-foreground">按业务模块查看运行过程，使用任务、请求和游标标识追踪结果。</p></div>
    <Card><CardContent className="space-y-4 pt-5">
      <div className="flex flex-wrap items-end gap-3">
        <div className="flex min-w-36 flex-col gap-1"><Label htmlFor="log-module">业务模块</Label><select id="log-module" className={selectClass} value={module} onChange={e => setModule(e.target.value)}><option value="">全部模块</option>{Object.entries(SYSTEM_LOG_MODULE_LABELS).map(([value, name]) => <option key={value} value={value}>{name}</option>)}</select></div>
        <div className="flex min-w-32 flex-col gap-1"><Label htmlFor="log-level">级别</Label><select id="log-level" className={selectClass} value={level} onChange={e => setLevel(e.target.value)}><option value="">全部级别</option>{Object.entries(levels).filter(([key]) => key !== "debug").map(([key, name]) => <option key={key} value={key}>{name}</option>)}</select></div>
        <div className="flex flex-col gap-1"><Label htmlFor="log-since">开始时间</Label><Input id="log-since" type="datetime-local" value={since} onChange={e => setSince(e.target.value)} /></div>
        <div className="flex flex-col gap-1"><Label htmlFor="log-until">结束时间</Label><Input id="log-until" type="datetime-local" value={until} onChange={e => setUntil(e.target.value)} /></div>
        <Button variant="outline" onClick={() => { setSince(""); setUntil(""); }}>全部保留时间</Button>
      </div>
      <div className="flex flex-wrap items-end gap-3">
        <div className="flex min-w-48 flex-1 flex-col gap-1"><Label htmlFor="log-keyword">关键字 / 关联 ID</Label><Input id="log-keyword" placeholder="搜索说明、公众号、请求或游标 ID" maxLength={250} value={keyword} onChange={e => setKeyword(e.target.value)} /></div>
        <div className="flex w-32 flex-col gap-1"><Label htmlFor="log-task">任务 ID</Label><Input id="log-task" type="number" min={1} step={1} placeholder="全部任务" value={taskId} onChange={e => setTaskId(e.target.value)} /></div>
        <div className="flex w-32 flex-col gap-1"><Label htmlFor="log-experiment">实验 ID</Label><Input id="log-experiment" type="number" min={1} step={1} placeholder="全部实验" value={experimentId} onChange={e => setExperimentId(e.target.value)} /></div>
        <div className="flex flex-col gap-1"><Label htmlFor="log-auto">自动刷新</Label><div className="flex h-9 items-center"><Switch id="log-auto" checked={auto} onCheckedChange={setAuto} /></div></div>
        <Button variant="outline" disabled={invalid || query.isFetching} onClick={() => { void query.refetch(); }}><RefreshCw className="mr-2 size-4" />刷新</Button>
      </div>
      <div className="flex flex-wrap items-center justify-between gap-3 border-t pt-3">
        <p className="text-xs text-muted-foreground">保留近 {query.data?.retention_days ?? "—"} 天，最多 {query.data?.max_rows.toLocaleString() ?? "—"} 条。导出包含全部匹配记录。</p>
        <div className="flex items-end gap-2"><div className="flex flex-col gap-1"><Label htmlFor="log-format">导出格式</Label><select id="log-format" aria-label="日志导出格式" className={selectClass} value={format} onChange={e => setFormat(e.target.value as "json" | "txt")}><option value="json">JSON · 结构化分析</option><option value="txt">文本 · 阅读</option></select></div><Button disabled={exporting || invalid || search !== keyword || query.isPending || !!query.error} onClick={() => { void download(); }}><Download className="mr-2 size-4" />{exporting ? "正在导出…" : "导出系统日志"}</Button></div>
      </div>
    </CardContent></Card>
    {invalid && <p role="alert" className="text-sm text-destructive">请检查时间范围、任务 ID 和实验 ID。开始时间不能晚于结束时间。</p>}
    {!invalid && query.error && <p role="alert" className="text-sm text-destructive">日志读取失败：{String(query.error)}。可点击刷新重试。</p>}
    {!invalid && <Card><CardContent className="pt-4">
      <div className="mb-3 flex items-center justify-between text-xs text-muted-foreground"><span>匹配 {query.data?.total ?? 0} 条 · 每页 {pageSize} 条</span><span>{query.isFetching ? "读取中…" : currentPage === 0 && auto ? "每 2 秒刷新" : "当前页已暂停自动刷新"}</span></div>
      <Table><TableHeader><TableRow>{["时间 / 级别", "业务模块", "日志说明", "关键数据"].map(h => <TableHead key={h}>{h}</TableHead>)}</TableRow></TableHeader><TableBody>{query.data?.rows.map(row => <TableRow key={row.id}>
        <TableCell className="whitespace-nowrap align-top"><p className="font-mono text-xs">{new Date(row.ts * 1000).toLocaleString("zh-CN", { hour12: false })}</p><Badge className="mt-2" variant={row.level === "error" ? "destructive" : row.level === "warn" ? "warning" : "muted"}>{levels[row.level] || row.level}</Badge><span className="ml-2 font-mono text-xs text-muted-foreground">#{row.id}</span></TableCell>
        <TableCell className="whitespace-nowrap align-top">{SYSTEM_LOG_MODULE_LABELS[row.module] || STAGE_LABELS[row.module as keyof typeof STAGE_LABELS] || row.module}</TableCell>
        <TableCell className="min-w-48 max-w-xl whitespace-pre-wrap break-words align-top text-sm">{row.message}</TableCell>
        <TableCell className="min-w-48 max-w-sm align-top"><div className="space-y-1 text-xs">{Object.entries(row.context).filter(([, v]) => v != null).map(([key, value]) => <p key={key} className="break-all"><span className="text-muted-foreground">{fields[key] || key}：</span><span className="font-mono">{typeof value === "boolean" ? value ? "是" : "否" : typeof value === "number" && !Number.isInteger(value) ? value.toFixed(2) : String(value)}</span></p>)}{!Object.values(row.context).some(v => v != null) && <span className="text-muted-foreground">—</span>}</div></TableCell>
      </TableRow>)}</TableBody></Table>
      {!query.isPending && !query.error && !query.data?.rows.length && <p className="py-10 text-center text-sm text-muted-foreground">当前筛选没有日志。可扩大时间范围或选择其他模块。</p>}
      <Pager label="系统日志" page={currentPage} total={query.data?.total??0} pageSize={pageSize} busy={query.isFetching||invalid} onPage={setPage} onPageSize={size=>{setPageSize(size);setPage(0);}} />
    </CardContent></Card>}
  </div>;
}
