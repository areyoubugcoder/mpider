import { useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Check,
  Copy,
  Eraser,
  FileJson,
  ListChecks,
  RefreshCw,
  Send,
  Trash2,
} from "lucide-react";
import { toast } from "sonner";
import { Pager } from "@/components/Pager";
import { InfoTip } from "@/components/InfoTip";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  clearJobs,
  deleteJob,
  fmtLogDateTime,
  getJob,
  listJobs,
  type JobListItem,
  type JobOutcome,
  type JobRow,
} from "@/lib/tauri";
import { cn, msg } from "@/lib/utils";

/**
 * 「任务列表」页：每条巡检批次（含单号重新巡检；老版本的手动任务行只作留档）从开始处理到收尾结束的留档——
 * 任务 ID、开始 / 完成时间、耗时、状态（正常 / 超时 / 服务端错误 / 错误）、错误原因，
 * 以及「任务数据」（链接与翻页依据）与「上报数据」（采集结果 / 上报载荷）两个弹窗。
 * 数据来自后端 jobs 表（`list_jobs` / `get_job`），列表每 10 秒自动刷新。
 * 逐条「删除」与页头「清空已结束」都是**物理删除、不可恢复**，各有二次确认弹窗；进行中的任务不能删。
 */

/** 是否进行中（还没有结果分类且处于处理阶段）——删除按钮据此禁用，与后端判据一致。 */
function isRunning(it: JobListItem): boolean {
  return it.outcome == null && ["received", "capturing", "collecting"].includes(it.status);
}

const PAGE_SIZE = 50;

/** 耗时短写：42s / 3m12s / 1h05m。 */
function fmtDuration(secs: number): string {
  const s = Math.max(0, Math.round(secs));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m${String(s % 60).padStart(2, "0")}s`;
  return `${Math.floor(s / 3600)}h${String(Math.floor((s % 3600) / 60)).padStart(2, "0")}m`;
}

/** 毫秒短写：850ms / 1.2s / 3m12s。 */
function fmtMs(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(ms < 10_000 ? 1 : 0)}s`;
  return fmtDuration(ms / 1000);
}

/** 耗时列 hover 文案：各阶段一行（按时间顺序），末尾合计；便于看出该调哪个环节。 */
function phasesTitle(it: JobListItem): string {
  if (it.phases.length === 0) return "";
  const w = Math.max(...it.phases.map((p) => p.name.length));
  const total = it.phases.reduce((s, p) => s + p.ms, 0);
  const lines = it.phases.map((p) => {
    const pct = total > 0 ? Math.round((p.ms / total) * 100) : 0;
    return `${p.name.padEnd(w, "　")}  ${fmtMs(p.ms)}（${pct}%）`;
  });
  lines.push(`合计 ${fmtMs(total)}`);
  return lines.join("\n");
}

/** 流转状态的中文（跑完前的过程态）。 */
const STATUS_LABEL: Record<string, string> = {
  received: "已接收",
  capturing: "接力抓凭证",
  collecting: "采集中",
  reported: "已上报",
  done: "已完成",
  error: "错误",
};

/** 结果分类 → 徽章样式与文案。跑完前（outcome 为空）按流转状态显示「进行中」。 */
function outcomeBadge(it: JobListItem): { label: string; className: string } {
  const map: Record<JobOutcome, { label: string; className: string }> = {
    ok: { label: "正常", className: "border-emerald-200 bg-emerald-50 text-emerald-700" },
    timeout: { label: "超时", className: "border-amber-200 bg-amber-50 text-amber-700" },
    server_error: { label: "服务端错误", className: "border-red-200 bg-red-50 text-red-700" },
    error: { label: "错误", className: "border-red-200 bg-red-50 text-red-700" },
  };
  if (it.outcome) return map[it.outcome];
  if (it.status === "error") return map.error;
  return {
    label: `进行中 · ${STATUS_LABEL[it.status] ?? it.status}`,
    className: "border-sky-200 bg-sky-50 text-sky-700",
  };
}

/** JSON 弹窗内容：哪条任务、看哪一份。 */
interface DialogState {
  id: number;
  which: "task" | "report";
}

export function Jobs() {
  const [page, setPage] = useState(0);
  const [dialog, setDialog] = useState<DialogState | null>(null);
  // 待确认物理删除的任务 / 是否打开「清空」确认框（null / false = 关闭）。
  const [pendingDelete, setPendingDelete] = useState<JobListItem | null>(null);
  const [confirmClear, setConfirmClear] = useState(false);
  const qc = useQueryClient();

  const jobs = useQuery({
    queryKey: ["jobs", page],
    queryFn: () => listJobs(undefined, PAGE_SIZE, page * PAGE_SIZE),
    refetchInterval: 10_000,
  });
  const total = jobs.data?.total ?? 0;
  const pageCount = Math.max(1, Math.ceil(total / PAGE_SIZE));

  /** 物理删除单条（确认后执行）。失败留在确认框里显示，便于重试。 */
  const removeJob = useMutation({
    mutationFn: (id: number) => deleteJob(id),
    onSuccess: (deleted, id) => {
      setPendingDelete(null);
      toast.success(deleted ? `已删除任务记录 #${id}` : `任务 #${id} 已不存在`, {
        description: "物理删除，不可恢复",
      });
      void qc.invalidateQueries({ queryKey: ["jobs"] });
    },
  });

  /** 一键清空已结束的任务（确认后执行）。 */
  const clearAll = useMutation({
    mutationFn: () => clearJobs(),
    onSuccess: (n) => {
      setConfirmClear(false);
      toast.success(`已清空 ${n} 条已结束的任务记录`, {
        description: "进行中的任务已保留；物理删除，不可恢复",
      });
      setPage(0);
      void qc.invalidateQueries({ queryKey: ["jobs"] });
    },
  });

  const summary = useMemo(() => {
    const items = jobs.data?.items ?? [];
    const c = { ok: 0, timeout: 0, server_error: 0, error: 0 };
    for (const it of items) if (it.outcome) c[it.outcome] += 1;
    return c;
  }, [jobs.data]);

  return (
    <>
      <Card>
        <CardHeader className="flex flex-row flex-wrap items-start justify-between gap-3">
          <div>
            <CardTitle className="flex items-center gap-2 text-sm">
              <ListChecks className="size-4 text-foreground" />
              任务列表
            </CardTitle>
            <CardDescription className="flex items-center gap-1">
              <span>
                每个巡检批次的留档。本页：正常 {summary.ok} · 超时 {summary.timeout} · 服务端错误 {summary.server_error} · 错误{" "}
                {summary.error}
              </span>
              <InfoTip>
                每行是一个批次从开始处理到收尾结束的记录：起止时间、耗时、结果分类与错误原因；点「任务数据」看任务链接，
                「上报数据」看采集结果与上报载荷。开了数据上报时每个号列表采完即当场上报，列表没获取到的号不报。
              </InfoTip>
            </CardDescription>
          </div>
          <div className="flex items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              disabled={jobs.isFetching}
              onClick={() => void jobs.refetch()}
            >
              <RefreshCw className={cn(jobs.isFetching && "animate-spin")} />
              刷新
            </Button>
            <Button
              variant="outline"
              size="sm"
              className="text-destructive hover:text-destructive"
              disabled={total === 0}
              onClick={() => setConfirmClear(true)}
            >
              <Eraser />
              清空已结束
            </Button>
          </div>
        </CardHeader>
        <CardContent className="p-0">
          {jobs.isError && (
            <div className="px-4 py-6 text-sm text-destructive">读取任务列表失败：{msg(jobs.error)}</div>
          )}
          {jobs.isSuccess && jobs.data.items.length === 0 && (
            <div className="px-4 py-10 text-center text-sm text-muted-foreground">
              还没有任务记录。开始巡检后，每个批次会依次出现在这里。
            </div>
          )}
          {jobs.isSuccess && jobs.data.items.length > 0 && (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead className="w-[190px]">任务 ID</TableHead>
                  <TableHead className="w-[150px]">开始时间</TableHead>
                  <TableHead className="w-[150px]">完成时间</TableHead>
                  <TableHead className="w-[80px] text-right" title="悬停耗时可看各阶段耗时">
                    耗时
                  </TableHead>
                  <TableHead className="w-[130px]">状态</TableHead>
                  <TableHead className="w-[130px] text-right" title="任务链接数 / 采到的文章链接数；副行：已上报的号数 / 任务里的号数">
                    链接 / 结果
                  </TableHead>
                  <TableHead>错误原因</TableHead>
                  <TableHead className="w-[230px] text-right">数据 / 操作</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {jobs.data.items.map((it) => {
                  const badge = outcomeBadge(it);
                  const dur =
                    it.started_at != null && it.finished_at != null
                      ? fmtDuration(it.finished_at - it.started_at)
                      : it.started_at != null
                        ? fmtDuration(Date.now() / 1000 - it.started_at) + "…"
                        : "—";
                  return (
                    <TableRow key={it.id}>
                      <TableCell>
                        <div className="truncate text-xs">
                          {it.kind === "sweep" ? "巡检批次" : "手动任务（旧）"}
                        </div>
                        <div className="text-[11px] text-muted-foreground">
                          本地 #{it.id}
                          {it.report_http_status != null && ` · 上报 HTTP ${it.report_http_status}`}
                        </div>
                      </TableCell>
                      <TableCell className="whitespace-nowrap text-xs">
                        {it.started_at != null ? fmtLogDateTime(it.started_at) : fmtLogDateTime(it.received_at)}
                      </TableCell>
                      <TableCell className="whitespace-nowrap text-xs">
                        {it.finished_at != null ? fmtLogDateTime(it.finished_at) : "—"}
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs">
                        {it.phases.length > 0 ? (
                          <span
                            className="cursor-help underline decoration-dotted underline-offset-2"
                            title={phasesTitle(it)}
                          >
                            {dur}
                          </span>
                        ) : (
                          dur
                        )}
                      </TableCell>
                      <TableCell>
                        <Badge variant="outline" className={cn("whitespace-nowrap font-normal", badge.className)}>
                          {badge.label}
                        </Badge>
                      </TableCell>
                      <TableCell className="whitespace-nowrap text-right font-mono text-xs">
                        {it.links} / {it.urls}
                        {it.truncated && (
                          <span className="ml-1 text-amber-700" title={`交回 ${it.remaining} 条链接`}>
                            ↩{it.remaining}
                          </span>
                        )}
                        {it.kind !== "sweep" && it.accounts > 0 && (
                          <div
                            className={cn(
                              "text-[11px]",
                              it.reported_accounts < it.accounts ? "text-amber-700" : "text-muted-foreground",
                            )}
                            title="已上报成功的号数 / 任务里的号数（列表没获取到的号不上报）"
                          >
                            已报 {it.reported_accounts}/{it.accounts} 号
                          </div>
                        )}
                      </TableCell>
                      <TableCell>
                        <div
                          className={cn(
                            "line-clamp-2 max-w-[360px] text-xs",
                            it.error ? "text-foreground" : "text-muted-foreground",
                          )}
                          title={it.error ?? ""}
                        >
                          {it.error ?? "—"}
                        </div>
                      </TableCell>
                      <TableCell className="text-right">
                        <div className="flex justify-end gap-1">
                          <Button
                            variant="ghost"
                            size="sm"
                            className="h-7 px-2 text-xs"
                            onClick={() => setDialog({ id: it.id, which: "task" })}
                          >
                            <FileJson />
                            任务数据
                          </Button>
                          <Button
                            variant="ghost"
                            size="sm"
                            className="h-7 px-2 text-xs"
                            disabled={it.kind === "sweep" && it.outcome == null}
                            onClick={() => setDialog({ id: it.id, which: "report" })}
                          >
                            <Send />
                            上报数据
                          </Button>
                          <Button
                            variant="ghost"
                            size="icon"
                            className="size-7 text-muted-foreground hover:text-destructive"
                            disabled={isRunning(it)}
                            title={
                              isRunning(it)
                                ? "任务进行中，跑完后才能删除"
                                : "删除该任务记录（物理删除，不可恢复）"
                            }
                            onClick={() => setPendingDelete(it)}
                          >
                            <Trash2 />
                          </Button>
                        </div>
                      </TableCell>
                    </TableRow>
                  );
                })}
              </TableBody>
            </Table>
          )}
          {jobs.isSuccess && (
            <Pager page={page} pageCount={pageCount} total={total} unit="条" busy={jobs.isFetching} onPage={setPage} />
          )}
        </CardContent>
      </Card>

      <JobJsonDialog state={dialog} onClose={() => setDialog(null)} />

      {/* 逐条物理删除确认 */}
      <Dialog open={pendingDelete !== null} onOpenChange={(o) => !o && setPendingDelete(null)}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle className="text-sm">
              删除任务「本地 #{pendingDelete?.id ?? ""}」？
            </DialogTitle>
            <DialogDescription className="leading-relaxed">
              这是<b className="text-foreground">物理删除</b>——这条任务的记录（起止时间、结果、上报载荷）
              将从数据库中彻底移除，<b className="text-foreground">不可恢复</b>。已采到的文章与限流分析留档不受影响。
            </DialogDescription>
          </DialogHeader>
          {removeJob.isError && (
            <div className="text-xs text-destructive">删除失败：{msg(removeJob.error)}</div>
          )}
          <DialogFooter>
            <Button variant="outline" size="sm" onClick={() => setPendingDelete(null)}>
              取消
            </Button>
            <Button
              variant="destructive"
              size="sm"
              loading={removeJob.isPending}
              onClick={() => {
                if (pendingDelete) removeJob.mutate(pendingDelete.id);
              }}
            >
              {!removeJob.isPending && <Trash2 />}
              确认删除
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 一键清空确认 */}
      <Dialog open={confirmClear} onOpenChange={(o) => !o && setConfirmClear(false)}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle className="text-sm">清空全部已结束的任务记录？</DialogTitle>
            <DialogDescription className="leading-relaxed">
              将<b className="text-foreground">物理删除</b>库里所有已结束的任务（共约 {total} 条，
              进行中的保留），<b className="text-foreground">不可恢复</b>。已采到的文章与限流分析留档不受影响。
            </DialogDescription>
          </DialogHeader>
          {clearAll.isError && (
            <div className="text-xs text-destructive">清空失败：{msg(clearAll.error)}</div>
          )}
          <DialogFooter>
            <Button variant="outline" size="sm" onClick={() => setConfirmClear(false)}>
              取消
            </Button>
            <Button
              variant="destructive"
              size="sm"
              loading={clearAll.isPending}
              onClick={() => clearAll.mutate()}
            >
              {!clearAll.isPending && <Eraser />}
              确认清空
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}

/** 任务数据 / 上报数据弹窗：按需拉一条任务的完整记录，格式化成 JSON 展示并可复制。 */
function JobJsonDialog({ state, onClose }: { state: DialogState | null; onClose: () => void }) {
  const [copied, setCopied] = useState(false);
  const job = useQuery({
    queryKey: ["job", state?.id],
    queryFn: () => getJob(state!.id),
    enabled: state != null,
  });

  const { title, desc, text } = useMemo(() => {
    if (!state) return { title: "", desc: "", text: "" };
    const row: JobRow | null | undefined = job.data;
    if (state.which === "task") {
      const body =
        row?.raw ??
        (row
          ? {
              note: row.kind === "sweep" ? "巡检合成任务；以下为本地落库内容" : "旧版手动任务；以下为本地落库内容",
              links: row.links,
              last_updated_at: row.last_updated_at,
              link_since: row.link_since,
            }
          : null);
      return {
        title: `任务数据 · 本地 #${state.id}`,
        desc: "任务的链接与翻页依据（本地落库内容）",
        text: body == null ? "" : JSON.stringify(body, null, 2),
      };
    }
    const body = row?.result ?? null;
    return {
      title: `上报数据 · 本地 #${state.id}`,
      desc:
        row?.kind === "sweep"
          ? "本批的采集结果（urls / truncated / remaining_links / accounts）；开了数据上报时各号采完即上报"
          : `本地完整结果；accounts 里 finished=true 的号在采完时各自向上报地址 POST 了一次（无新文章也报），reported 表示被接受；finished=false 的号列表没获取到、不上报${row?.report_http_status != null ? `（上报地址最近返回 HTTP ${row.report_http_status}）` : ""}`,
      text: body == null ? "" : JSON.stringify(body, null, 2),
    };
  }, [state, job.data]);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch (e) {
      toast.error("复制失败", { description: msg(e) });
    }
  };

  return (
    <Dialog open={state != null} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-w-3xl">
        <DialogHeader>
          <DialogTitle className="text-sm">{title}</DialogTitle>
          <DialogDescription>{desc}</DialogDescription>
        </DialogHeader>
        {job.isPending && <div className="py-6 text-center text-sm text-muted-foreground">加载中…</div>}
        {job.isError && <div className="py-4 text-sm text-destructive">读取失败：{msg(job.error)}</div>}
        {job.isSuccess && (
          <>
            {text ? (
              <pre className="max-h-[60vh] overflow-auto rounded-md bg-muted p-3 text-xs leading-relaxed">
                {text}
              </pre>
            ) : (
              <div className="py-6 text-center text-sm text-muted-foreground">
                {state?.which === "report" ? "尚未产生上报数据（任务还没跑完或未进入采集）" : "无数据"}
              </div>
            )}
            <div className="flex justify-end">
              <Button variant="secondary" size="sm" disabled={!text} onClick={() => void copy()}>
                {copied ? <Check className="text-emerald-600" /> : <Copy />}
                {copied ? "已复制" : "复制 JSON"}
              </Button>
            </div>
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}
