import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Check,
  Copy,
  Download,
  Eraser,
  FolderOpen,
  RefreshCw,
  Trash2,
} from "lucide-react";
import { toast } from "sonner";
import { useApp } from "@/store";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { msg } from "@/lib/utils";
import {
  clearLogsDb,
  exportLogs,
  fmtLogDateTime,
  fmtLogTime,
  formatLogLine,
  listLogs,
  logStats,
  revealInFolder,
  STAGE_LABELS,
  STAGE_ORDER,
  type LogEvent,
  type LogLevel,
  type LogStage,
  type LogStats,
} from "@/lib/tauri";

/** 「时间范围」筛选：秒；0 = 全部（库里最多 3 天）。 */
const RANGE_OPTIONS: Array<{ key: string; label: string; secs: number }> = [
  { key: "1h", label: "近 1 小时", secs: 3600 },
  { key: "6h", label: "近 6 小时", secs: 6 * 3600 },
  { key: "1d", label: "近 1 天", secs: 86400 },
  { key: "all", label: "全部（近 3 天）", secs: 0 },
];

type LevelFilter = "" | "warn" | "error";

/** 「自动滚动」开关的本机记忆（localStorage）；没记过默认开。 */
const AUTOSCROLL_KEY = "mp.logs.autoscroll";

function loadAutoScroll(): boolean {
  try {
    const raw = localStorage.getItem(AUTOSCROLL_KEY);
    return raw == null ? true : raw === "1";
  } catch {
    return true;
  }
}

function saveAutoScroll(on: boolean) {
  try {
    localStorage.setItem(AUTOSCROLL_KEY, on ? "1" : "0");
  } catch {
    /* 本机存储不可用时只影响记忆，不影响功能 */
  }
}

/** 级别 → 徽章样式 / 文案。 */
function levelBadge(level: LogLevel): { variant: "muted" | "warning" | "destructive" | "success"; text: string } {
  switch (level) {
    case "warn":
      return { variant: "warning", text: "警告" };
    case "error":
      return { variant: "destructive", text: "错误" };
    case "debug":
      return { variant: "muted", text: "调试" };
    default:
      return { variant: "success", text: "信息" };
  }
}

/** 环节 → 徽章配色（同一环节同一色，便于扫读）。 */
const STAGE_TONE: Record<LogStage, string> = {
  app: "bg-muted text-muted-foreground",
  task: "bg-sky-500/15 text-sky-700 dark:text-sky-300",
  proxy: "bg-slate-500/15 text-slate-700 dark:text-slate-300",
  rpa: "bg-violet-500/15 text-violet-700 dark:text-violet-300",
  capture: "bg-amber-500/15 text-amber-700 dark:text-amber-300",
  list: "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300",
  refresh: "bg-orange-500/15 text-orange-700 dark:text-orange-300",
  report: "bg-teal-500/15 text-teal-700 dark:text-teal-300",
  detail: "bg-pink-500/15 text-pink-700 dark:text-pink-300",
  sweep: "bg-indigo-500/15 text-indigo-700 dark:text-indigo-300",
};

/** 一行日志。 */
function LogRow({ e }: { e: LogEvent }) {
  const lb = levelBadge(e.level);
  const tone =
    e.level === "error"
      ? "text-destructive"
      : e.level === "warn"
        ? "text-warning"
        : e.transient
          ? "text-muted-foreground"
          : "text-foreground";
  return (
    <div
      className={`flex items-start gap-2 border-b border-border/40 px-2 py-1 text-xs leading-relaxed last:border-0 ${
        e.transient ? "opacity-70" : ""
      }`}
      title={`${fmtLogDateTime(e.ts)}${e.job_id ? ` · job#${e.job_id}` : ""}${
        e.transient ? " · 瞬态提示（不入库）" : ""
      }`}
    >
      <span className="w-[62px] shrink-0 font-mono tabular-nums text-muted-foreground">
        {fmtLogTime(e.ts)}
      </span>
      <span
        className={`inline-flex w-[64px] shrink-0 justify-center rounded px-1 py-0.5 text-[11px] font-medium ${
          STAGE_TONE[e.stage] ?? STAGE_TONE.app
        }`}
      >
        {STAGE_LABELS[e.stage] ?? e.stage}
      </span>
      {e.level !== "info" && (
        <Badge variant={lb.variant} className="shrink-0 px-1.5 py-0 text-[10px]">
          {lb.text}
        </Badge>
      )}
      <span className={`min-w-0 flex-1 whitespace-pre-wrap break-words ${tone}`}>
        {e.message}
      </span>
      {e.job_id != null && (
        <span className="shrink-0 font-mono text-[10px] text-muted-foreground">
          #{e.job_id}
        </span>
      )}
    </div>
  );
}

/**
 * 日志页：整链各环节的步骤日志。
 *
 * - 实时：后端 `agent://log` 事件流入 store.logs（含瞬态倒计时行，灰显）；
 * - 历史：「刷新」按当前筛选从 SQLite `app_log` 取入库的关键节点（只保留近 3 天），替换视图；
 * - 出问题时：「导出文件」落一份文本供分析；
 * - 「自动滚动」开关：开着时每来一行都滚到底部（默认开，记在本机）；关掉后停在当前位置便于翻看。
 */
export function Logs() {
  const { logs, clearLogs, replaceLogs } = useApp();
  const listRef = useRef<HTMLDivElement>(null);
  const [stage, setStage] = useState<LogStage | "">("");
  const [level, setLevel] = useState<LevelFilter>("");
  const [range, setRange] = useState("1d");
  const [keyword, setKeyword] = useState("");
  const [stats, setStats] = useState<LogStats | null>(null);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState<"" | "export" | "clear">("");
  const [notice, setNotice] = useState<{ ok: boolean; text: string; path?: string } | null>(null);
  const [copied, setCopied] = useState(false);
  const [autoScroll, setAutoScroll] = useState(loadAutoScroll);

  const refreshStats = useCallback(async () => {
    try {
      setStats(await logStats());
    } catch (e) {
      console.error("log_stats", msg(e));
    }
  }, []);

  /** 从库加载历史（按环节 / 级别 / 时间范围），替换视图；之后实时事件继续追加。 */
  const loadHistory = useCallback(async () => {
    setLoading(true);
    try {
      const secs = RANGE_OPTIONS.find((r) => r.key === range)?.secs ?? 0;
      const rows = await listLogs({
        sinceSecs: secs || undefined,
        stage,
        minLevel: level,
        limit: 3000,
      });
      replaceLogs(
        rows.map((r) => ({
          seq: -r.id, // 负数避免与实时序号撞车
          ts: r.ts,
          level: r.level,
          stage: r.stage,
          job_id: r.job_id,
          message: r.message,
          transient: false,
        })),
      );
      await refreshStats();
    } catch (e) {
      setNotice({ ok: false, text: "加载历史失败：" + msg(e) });
    } finally {
      setLoading(false);
    }
  }, [range, stage, level, replaceLogs, refreshStats]);

  // 首次进入：加载近 1 天历史 + 概况。
  useEffect(() => {
    void loadHistory();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 实时视图按当前筛选过滤（历史已按筛选取过，再过一遍无害）。
  const visible = useMemo(() => {
    const kw = keyword.trim().toLowerCase();
    return logs.filter((e) => {
      if (stage && e.stage !== stage) return false;
      if (level === "warn" && e.level !== "warn" && e.level !== "error") return false;
      if (level === "error" && e.level !== "error") return false;
      if (kw && !e.message.toLowerCase().includes(kw)) return false;
      return true;
    });
  }, [logs, stage, level, keyword]);

  // 开关开着：视图每次变化（新日志 / 换筛选 / 刷新历史）都滚到底；刚打开开关时也立即滚一次。
  useEffect(() => {
    if (autoScroll && listRef.current) listRef.current.scrollTop = listRef.current.scrollHeight;
  }, [visible, autoScroll]);

  const toggleAutoScroll = (on: boolean) => {
    setAutoScroll(on);
    saveAutoScroll(on);
  };

  const copy = () => {
    void navigator.clipboard
      ?.writeText(visible.map((e) => `${fmtLogDateTime(e.ts)} ${formatLogLine(e).slice(9)}`).join("\n"))
      .then(() => {
        setCopied(true);
        setTimeout(() => setCopied(false), 1500);
      })
      .catch((e) => toast.error("复制失败", { description: msg(e) }));
  };

  // 导出 / 清空：结果既 toast 一下（即时反馈），也留在卡片顶部的 notice 里（带「打开所在文件夹」等后续动作）。
  const doExport = async () => {
    setBusy("export");
    setNotice(null);
    try {
      const out = await exportLogs();
      setNotice({ ok: true, text: `已导出 ${out.count} 行到 ${out.path}`, path: out.path });
      toast.success(`已导出 ${out.count} 行日志`, { description: out.path });
    } catch (e) {
      setNotice({ ok: false, text: "导出失败：" + msg(e) });
      toast.error("导出日志失败", { description: msg(e) });
    } finally {
      setBusy("");
    }
  };

  const doClearDb = async () => {
    setBusy("clear");
    setNotice(null);
    try {
      const n = await clearLogsDb();
      clearLogs();
      await refreshStats();
      setNotice({ ok: true, text: `已清空库内日志 ${n} 行` });
      toast.success(`已清空库内日志 ${n} 行`);
    } catch (e) {
      setNotice({ ok: false, text: "清空失败：" + msg(e) });
      toast.error("清空日志失败", { description: msg(e) });
    } finally {
      setBusy("");
    }
  };

  const warnCount = visible.filter((e) => e.level === "warn").length;
  const errCount = visible.filter((e) => e.level === "error").length;

  return (
    <div className="flex h-full flex-col">
      <div className="mb-3">
        <h1 className="text-xl font-semibold tracking-tight">日志</h1>
        <p className="mt-0.5 text-xs text-muted-foreground">
          整链各环节的步骤记录：取任务 → 开抓凭证代理 → 点种子 → 抓凭证 → 逐页取列表 → 续期 → 上报。
          关键节点入库，保留近 {stats?.retention_days ?? 3} 天；出问题时可导出文件。
        </p>
      </div>

      <Card className="flex min-h-0 flex-1 flex-col">
        <CardHeader className="space-y-2 p-4 pb-2">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div>
              <CardTitle className="text-sm">运行日志</CardTitle>
              <CardDescription className="mt-1">
                显示 {visible.length} 行
                {warnCount > 0 && <span className="text-warning">，警告 {warnCount}</span>}
                {errCount > 0 && <span className="text-destructive">，错误 {errCount}</span>}
                {stats && <span>，库内共 {stats.count} 行</span>}
                {!autoScroll && <span>，自动滚动已关闭</span>}
              </CardDescription>
            </div>
            <div className="flex flex-wrap gap-2">
              <Button variant="outline" size="sm" onClick={() => void loadHistory()} loading={loading}>
                {!loading && <RefreshCw />}
                刷新
              </Button>
              <Button variant="outline" size="sm" onClick={copy} disabled={visible.length === 0}>
                {copied ? <Check /> : <Copy />}
                {copied ? "已复制" : "复制"}
              </Button>
              <Button
                variant="outline"
                size="sm"
                onClick={() => void doExport()}
                disabled={busy !== ""}
                loading={busy === "export"}
              >
                {busy !== "export" && <Download />}
                {busy === "export" ? "导出中…" : "导出文件"}
              </Button>
              <Button variant="outline" size="sm" onClick={clearLogs} disabled={logs.length === 0} title="只清空当前视图，不动库">
                <Eraser />
                清空视图
              </Button>
              <Button
                variant="outline"
                size="sm"
                onClick={() => void doClearDb()}
                disabled={busy !== "" || !stats || stats.count === 0}
                loading={busy === "clear"}
                title="删除库里全部日志（不可恢复）"
              >
                {busy !== "clear" && <Trash2 />}
                {busy === "clear" ? "清空中…" : "清空库"}
              </Button>
            </div>
          </div>

          <div className="flex flex-wrap items-center gap-2">
            <Select value={stage || "all"} onValueChange={(v) => setStage(v === "all" ? "" : (v as LogStage))}>
              <SelectTrigger className="h-8 w-[130px] text-xs">
                <SelectValue placeholder="全部环节" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">全部环节</SelectItem>
                {STAGE_ORDER.map((s) => (
                  <SelectItem key={s} value={s}>
                    {STAGE_LABELS[s]}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Select value={level || "all"} onValueChange={(v) => setLevel(v === "all" ? "" : (v as LevelFilter))}>
              <SelectTrigger className="h-8 w-[120px] text-xs">
                <SelectValue placeholder="全部级别" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">全部级别</SelectItem>
                <SelectItem value="warn">警告及以上</SelectItem>
                <SelectItem value="error">仅错误</SelectItem>
              </SelectContent>
            </Select>
            <Select value={range} onValueChange={setRange}>
              <SelectTrigger className="h-8 w-[140px] text-xs">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {RANGE_OPTIONS.map((r) => (
                  <SelectItem key={r.key} value={r.key}>
                    {r.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Input
              className="h-8 w-[200px] text-xs"
              placeholder="按关键词过滤（当前视图）"
              value={keyword}
              onChange={(e) => setKeyword(e.target.value)}
            />
            <div
              className="flex h-8 shrink-0 items-center gap-2"
              title="开着时新日志到达自动滚到底部；关掉后停在当前位置，方便往回翻看"
            >
              <Switch id="logs-autoscroll" checked={autoScroll} onCheckedChange={toggleAutoScroll} />
              <Label htmlFor="logs-autoscroll" className="cursor-pointer text-xs">
                自动滚动
              </Label>
            </div>
          </div>

          {notice && (
            <div
              className={`flex items-center gap-2 rounded-md px-2.5 py-1.5 text-xs ${
                notice.ok ? "bg-success/10 text-success" : "bg-destructive/10 text-destructive"
              }`}
            >
              <span className="min-w-0 flex-1 break-all">{notice.text}</span>
              {notice.path && (
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-6 px-2 text-xs"
                  onClick={() =>
                    void revealInFolder(notice.path!).catch((e) =>
                      toast.error("打开文件夹失败", { description: msg(e) }),
                    )
                  }
                >
                  <FolderOpen />
                  打开所在文件夹
                </Button>
              )}
            </div>
          )}
        </CardHeader>

        <CardContent className="min-h-0 flex-1 p-4 pt-0">
          <div ref={listRef} className="h-full overflow-y-auto rounded-lg bg-muted/60 font-mono">
            {visible.length ? (
              visible.map((e) => <LogRow key={`${e.transient ? "t" : "p"}${e.seq}`} e={e} />)
            ) : (
              <div className="p-3 text-xs text-muted-foreground">
                {loading ? "加载中…" : "（暂无记录：运行时各环节的步骤会实时记录在这里）"}
              </div>
            )}
          </div>
        </CardContent>
      </Card>
    </div>
  );
}
