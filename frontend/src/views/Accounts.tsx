import { useCallback, useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { listen } from "@tauri-apps/api/event";
import {
  DownloadCloud,
  History,
  Pause,
  Play,
  XCircle,
  KeyRound,
  ListPlus,
  Loader2,
  MoreHorizontal,
  Newspaper,
  PauseCircle,
  PlayCircle,
  RefreshCw,
  RotateCw,
  Trash2,
  User,
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
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { DetailProgressBar } from "@/components/DetailProgressBar";
import { Progress } from "@/components/ui/progress";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  dateToTs,
  fmtSecs,
  historyLine,
  historyPercent,
  isActiveJob,
  targetSummary,
} from "@/lib/history";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  addLinks,
  deleteAccount,
  fmtLogDateTime,
  getCredential,
  historyCancel,
  historyEstimate,
  historyPause,
  historyResume,
  historyStart,
  listAccounts,
  runStatus,
  setAccountSweepEnabled,
  sweepRetryAccount,
  type Account,
  type Credential,
  type HistoryEstimate,
  type HistoryJobView,
  type HistoryTarget,
  type AddLinksProgress,
  type AddLinksSummary,
  type RealRunConfig,
} from "@/lib/tauri";
import { cn, msg } from "@/lib/utils";
import { Pager } from "@/components/Pager";
import { InfoTip } from "@/components/InfoTip";
import type { View } from "@/views/types";

/** 每页公众号数。 */
const PAGE_SIZE = 20;

/** 相对时刻：把 epoch 秒显示成「N 秒/分钟/小时/天前」，null 显示「—」。 */
function ago(epoch: number | null, now: number): string {
  if (!epoch) return "—";
  const d = Math.max(0, now - epoch);
  if (d < 60) return `${Math.floor(d)} 秒前`;
  if (d < 3600) return `${Math.floor(d / 60)} 分钟前`;
  if (d < 86400) return `${Math.floor(d / 3600)} 小时前`;
  return `${Math.floor(d / 86400)} 天前`;
}

/** 「最新更新」列显示：3 天内用相对时刻（N 小时前 / 1 天前），超过 3 天显示原始 `YYYY-MM-DD HH:MM:SS`。 */
function recent(epoch: number | null, now: number): string {
  if (!epoch) return "—";
  return now - epoch < 3 * 86400 ? ago(epoch, now) : fmtLogDateTime(epoch);
}

/** 剩余时长：正数显示「N 分钟」，用于凭证距预估过期的倒计时。 */
function remaining(secs: number): string {
  if (secs < 60) return `${Math.floor(secs)} 秒`;
  if (secs < 3600) return `${Math.floor(secs / 60)} 分钟`;
  return `${Math.floor(secs / 3600)} 小时 ${Math.floor((secs % 3600) / 60)} 分`;
}

/** 凭证状态徽标：有效（带剩余倒计时）/ 已失效（实测 ret=-3）/ 已过期（超 TTL）/ 无。 */
function CredBadge({ account, now }: { account: Account; now: number }) {
  if (!account.cred_has_key) {
    return <span className="text-muted-foreground">无</span>;
  }
  if (account.cred_invalidated_at) {
    return (
      <Badge variant="destructive" title={`实测失效于 ${fmtLogDateTime(account.cred_invalidated_at)}`}>
        已失效
      </Badge>
    );
  }
  const left = (account.cred_expires_at ?? 0) - now;
  if (!account.cred_fresh || left <= 0) {
    return (
      <Badge
        variant="secondary"
        title={account.cred_expires_at ? `预估过期于 ${fmtLogDateTime(account.cred_expires_at)}` : undefined}
      >
        已过期
      </Badge>
    );
  }
  return (
    <Badge
      variant={left < 5 * 60 ? "warning" : "success"}
      title={`预估过期于 ${fmtLogDateTime(account.cred_expires_at ?? 0)}`}
    >
      有效 · 剩 {remaining(left)}
    </Badge>
  );
}

/** 巡检状态徽标：已暂停 / 未巡检 / 正常（上次时刻）/ 失败 / 无法续期；悬停显示原因。 */
function SweepBadge({ account, now }: { account: Account; now: number }) {
  if (!account.sweep_enabled) {
    return (
      <Badge variant="secondary" title="该号已被排除，不参与定时巡检">
        已暂停
      </Badge>
    );
  }
  if (!account.sweep_checked_at) {
    return <span className="text-muted-foreground">未巡检</span>;
  }
  const when = `${ago(account.sweep_checked_at, now)}（${fmtLogDateTime(account.sweep_checked_at)}）`;
  if (account.sweep_status === "ok") {
    return (
      <Badge variant="success" title={`上次巡检成功 ${when}`}>
        正常 · {ago(account.sweep_checked_at, now)}
      </Badge>
    );
  }
  if (account.sweep_status === "no_sample") {
    return (
      <Badge variant="warning" title={`${account.sweep_error ?? "无可用文章链接"}（${when}）`}>
        无法续期
      </Badge>
    );
  }
  return (
    <Badge variant="destructive" title={`${account.sweep_error ?? "巡检失败"}（${when}）`}>
      失败 · {ago(account.sweep_checked_at, now)}
    </Badge>
  );
}

/** 公众号头像：round_head_img 有值渲染 <img>，为 null 用占位圆。 */
function Avatar({ account }: { account: Account }) {
  const [broken, setBroken] = useState(false);
  const src = account.round_head_img;
  if (src && !broken) {
    return (
      <img
        src={src}
        alt={account.nickname ?? account.biz}
        referrerPolicy="no-referrer"
        onError={() => setBroken(true)}
        className="size-8 rounded-full object-cover"
      />
    );
  }
  return (
    <div className="grid size-8 place-items-center rounded-full bg-muted text-muted-foreground">
      <User className="size-4" />
    </div>
  );
}

/** 凭证数据对话框的一行：左标签、右值（值可为 null → 「—」）。 */
function CredRow({ label, value, mono }: { label: string; value: string | null | undefined; mono?: boolean }) {
  return (
    <div className="grid grid-cols-[6.5rem_1fr] items-start gap-2 py-1 text-xs">
      <div className="text-muted-foreground">{label}</div>
      {value ? (
        <div className={mono ? "break-all font-mono text-xs leading-5" : ""}>{value}</div>
      ) : (
        <div className="text-muted-foreground">—</div>
      )}
    </div>
  );
}

/** 「查看凭证数据」对话框：拉该号 credentials 表整行，先摆热数据统计，再摆参数本体。 */
function CredentialDialog({
  account,
  now,
  onClose,
}: {
  account: Account | null;
  now: number;
  onClose: () => void;
}) {
  const biz = account?.biz ?? null;
  const cred = useQuery({
    queryKey: ["credential", biz],
    queryFn: () => getCredential(biz!),
    enabled: biz !== null,
  });
  const c: Credential | null | undefined = cred.data;
  const fmt = (t: number | null | undefined) => (t ? fmtLogDateTime(t) : null);
  return (
    <Dialog open={account !== null} onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-xl">
        <DialogHeader>
          <DialogTitle className="text-sm">
            凭证数据 · {account?.nickname ?? account?.biz}
          </DialogTitle>
          <DialogDescription>
            credentials 表该号最新一份（热数据）。参数本体仅本机展示，不会进入日志。
          </DialogDescription>
        </DialogHeader>
        {cred.isPending && <div className="text-sm text-muted-foreground">加载中…</div>}
        {cred.isError && (
          <div className="text-sm text-destructive">读取失败：{msg(cred.error)}</div>
        )}
        {cred.isSuccess && !c && (
          <div className="text-sm text-muted-foreground">该号还没有抓到过凭证。</div>
        )}
        {c && (
          <div className="max-h-[60vh] overflow-y-auto pr-1">
            <div className="mb-1 flex items-center gap-2 text-xs font-medium">
              状态 {account && <CredBadge account={account} now={now} />}
            </div>
            <CredRow label="抓到时间" value={fmt(c.captured_at)} />
            <CredRow label="预估过期" value={fmt(c.expires_at)} />
            <CredRow label="实测失效" value={fmt(c.invalidated_at)} />
            <CredRow label="最近回放" value={fmt(c.last_used_at)} />
            <CredRow label="回放次数" value={String(c.use_count)} />
            <CredRow label="换 key 次数" value={String(c.refresh_count)} />
            <div className="my-2 h-px bg-border" />
            <CredRow label="__biz" value={c.biz} mono />
            <CredRow label="uin" value={c.uin} mono />
            <CredRow label="key" value={c.key} mono />
            <CredRow label="pass_ticket" value={c.pass_ticket} mono />
            <CredRow label="wxtoken" value={c.wxtoken} mono />
            <CredRow label="x5" value={c.x5} mono />
            <CredRow label="appmsg_token" value={c.appmsg_token} mono />
            <CredRow label="cookie" value={c.cookie} mono />
            <CredRow label="extra" value={c.extra} mono />
          </div>
        )}
        <DialogFooter>
          <Button variant="outline" size="sm" onClick={onClose}>
            关闭
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** 「历史抓取」列：进度条 + 一行状态文字；没有任务显示「—」。已结束的任务保留最后结果，悬停看目标。 */
function HistoryCell({ job, now }: { job: HistoryJobView | null; now: number }) {
  if (!job) return <span className="text-muted-foreground">—</span>;
  const pct = historyPercent(job);
  const active = isActiveJob(job);
  const tone =
    job.status === "failed"
      ? "text-destructive"
      : job.status === "paused" && job.paused_reason !== "user"
        ? "text-warning"
        : job.status === "running"
          ? "text-foreground"
          : "text-muted-foreground";
  return (
    <div className="flex w-44 flex-col gap-1" title={`目标：${targetSummary(job.target)}`}>
      {active && (
        <Progress
          value={pct ?? 100}
          className={cn("h-1", job.status === "running" && pct === null && "animate-pulse")}
        />
      )}
      <span className={cn("truncate text-xs tabular-nums", tone)}>{historyLine(job, now)}</span>
    </div>
  );
}

/** 「抓取历史文章」对话框：设目标（条数 / 起始 / 截止，都可空），看参数摘要与估算，提交启动。 */
function HistoryStartDialog({
  account,
  cfg,
  onClose,
  onStarted,
}: {
  account: Account | null;
  cfg: RealRunConfig;
  onClose: () => void;
  onStarted: () => void;
}) {
  const { history } = useApp();
  const [count, setCount] = useState("");
  const [since, setSince] = useState("");
  const [until, setUntil] = useState("");
  const [est, setEst] = useState<HistoryEstimate | null>(null);
  const [estError, setEstError] = useState<string | null>(null);

  // 打开时清空上次输入。
  useEffect(() => {
    if (account) {
      setCount("");
      setSince("");
      setUntil("");
      setEst(null);
      setEstError(null);
    }
  }, [account]);

  const target: HistoryTarget = {
    count: count.trim() ? Math.max(0, Math.round(Number(count))) || null : null,
    since_ts: dateToTs(since, false),
    until_ts: dateToTs(until, true),
  };
  const countInvalid = count.trim() !== "" && (!Number.isFinite(Number(count)) || Number(count) < 1);
  const rangeInvalid =
    target.since_ts !== null && target.until_ts !== null && target.since_ts >= target.until_ts;
  const invalid = countInvalid || rangeInvalid;

  // 输入变化后 400ms 再估算，避免每个字符都打后端。
  useEffect(() => {
    if (!account || invalid) return;
    const t = setTimeout(() => {
      historyEstimate(target, cfg)
        .then((e) => {
          setEst(e);
          setEstError(null);
        })
        .catch((e) => setEstError(msg(e)));
    }, 400);
    return () => clearTimeout(t);
    // target 是每次渲染新对象，按它的三个字段依赖即可。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [account, invalid, target.count, target.since_ts, target.until_ts, cfg]);

  const start = useMutation({
    mutationFn: () => historyStart(account!.biz, target, cfg),
    onSuccess: (job) => {
      toast.success(`已开始抓取「${account?.nickname ?? account?.biz}」的历史文章`, {
        description: `目标：${targetSummary(job.target)} · 每页 ${job.page_count} 条 · 间隔 ${fmtSecs(job.gap_secs)}`,
      });
      onStarted();
      onClose();
    },
    onError: (e) => toast.error("启动历史抓取失败", { description: msg(e) }),
  });

  const budgetLeft = history
    ? Math.max(0, history.budget - history.budget_used - history.budget_reserve)
    : null;

  return (
    <Dialog open={!!account} onOpenChange={(v) => !v && onClose()}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>抓取历史文章 · {account?.nickname ?? account?.biz}</DialogTitle>
          <DialogDescription className="flex items-center gap-1">
            <span>从最新一页往旧翻，按目标停下；同一时刻只抓一个号。</span>
            <InfoTip>
              三项目标都可空：空即不限，全空等于翻到底。每页条数与页间隔按系统设置里的历史抓取参数；
              与巡检共用当前微信号的 24 小时预算，用到只剩保留额度时自动暂停、额度腾出后继续。
              巡检开着时不能抓历史，先在控制面板停止巡检。
            </InfoTip>
          </DialogDescription>
        </DialogHeader>

        <div className="grid grid-cols-1 gap-3 md:grid-cols-3">
          <div className="flex flex-col gap-1.5">
            <Label className="text-xs text-muted-foreground">条数</Label>
            <Input
              type="number"
              min={1}
              step={1}
              placeholder="不限"
              value={count}
              onChange={(e) => setCount(e.target.value)}
            />
            <small className="text-[11px] text-muted-foreground">范围内抓到多少篇即停（含库里已有的）</small>
          </div>
          <div className="flex flex-col gap-1.5">
            <Label className="text-xs text-muted-foreground">起始日期</Label>
            <Input type="date" value={since} onChange={(e) => setSince(e.target.value)} />
            <small className="text-[11px] text-muted-foreground">翻到早于它的文章即停</small>
          </div>
          <div className="flex flex-col gap-1.5">
            <Label className="text-xs text-muted-foreground">截止日期</Label>
            <Input type="date" value={until} onChange={(e) => setUntil(e.target.value)} />
            <small className="text-[11px] text-muted-foreground">晚于它的文章跳过不计</small>
          </div>
        </div>

        {countInvalid && <div className="text-xs text-destructive">条数必须是不小于 1 的整数</div>}
        {rangeInvalid && <div className="text-xs text-destructive">起始日期必须早于截止日期</div>}

        <div className="rounded-lg bg-muted/50 p-3 text-xs leading-relaxed text-muted-foreground">
          <div>
            每页 <b className="text-foreground">{cfg.history_page_count}</b> 条 · 页间隔{" "}
            <b className="text-foreground">{fmtSecs(cfg.history_gap_seconds)}</b>
            {history && (
              <>
                {" "}· 当前微信号预算剩余{" "}
                <b className="text-foreground">{budgetLeft}</b> 次
                <span>（已用 {history.budget_used}/{history.budget || "不限"}，为巡检保留 {history.budget_reserve}）</span>
              </>
            )}
            <span className="ml-1">参数在「系统设置 → 历史文章抓取」修改</span>
          </div>
          {!invalid && est && (
            <div className="mt-1 text-foreground">
              预计 {est.pages > 0 ? `${est.pages} 页` : "不限页数"} · 约{" "}
              {est.seconds > 0 ? fmtSecs(est.seconds) : "—"}
              {est.note && <span className="ml-1 text-muted-foreground">{est.note}</span>}
            </div>
          )}
          {estError && <div className="mt-1 text-destructive">估算失败：{estError}</div>}
          {!invalid && est?.exceeds_budget && (
            <div className="mt-1 text-warning">
              超出当前可用预算：预算用完会自动暂停，额度腾出后继续，预计分多日完成
            </div>
          )}
          {target.until_ts !== null && (
            <div className="mt-1 text-warning">截止日期越早，跳过的页越多，同样计入预算</div>
          )}
        </div>

        <DialogFooter>
          <Button variant="outline" size="sm" onClick={onClose}>
            取消
          </Button>
          <Button size="sm" disabled={invalid} loading={start.isPending} onClick={() => start.mutate()}>
            {!start.isPending && <History />}
            开始抓取
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** 一行结果的状态徽标。 */
function AddLinkBadge({ status }: { status: AddLinksSummary["items"][number]["status"] }) {
  switch (status) {
    case "added":
      return <Badge variant="success">已添加</Badge>;
    case "updated":
      return <Badge variant="secondary">已更新</Badge>;
    case "rejected":
      return <Badge variant="destructive">已拒绝</Badge>;
    case "skipped":
      return <Badge variant="warning">未处理</Badge>;
  }
}

/** 「批量添加」对话框：多行粘贴公众号文章短链，后端逐条匿名直连文章页解析公众号名称 / biz 建号（不开微信）。 */
function BatchAddDialog({
  open,
  sweepOn,
  onClose,
  onNav,
}: {
  open: boolean;
  /** 巡检开关是否打开：决定成功后的提示文案。 */
  sweepOn: boolean;
  onClose: () => void;
  onNav: (v: View) => void;
}) {
  const qc = useQueryClient();
  const [text, setText] = useState("");
  const [result, setResult] = useState<AddLinksSummary | null>(null);
  const [progress, setProgress] = useState<AddLinksProgress | null>(null);
  const lines = text
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter((l) => l.length > 0);
  // 解析进度（addlinks://progress）：只在对话框打开期间订阅。
  useEffect(() => {
    if (!open) return;
    let disposed = false;
    let unlisten: (() => void) | null = null;
    void listen<AddLinksProgress>("addlinks://progress", (e) => setProgress(e.payload)).then((f) => {
      if (disposed) f();
      else unlisten = f;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [open]);
  const submit = useMutation({
    mutationFn: (links: string[]) => addLinks(links),
    onMutate: () => {
      setResult(null);
      setProgress({ done: 0, total: 0, current: "" });
    },
    onSuccess: (r) => {
      setResult(r);
      setProgress(null);
      const ok = r.added + r.updated;
      if (ok > 0) {
        toast.success(`已添加 ${r.added} 个公众号${r.updated > 0 ? `，更新 ${r.updated} 个` : ""}`, {
          description: sweepOn
            ? "巡检运行中，新号会在下一批起被巡检"
            : "到控制面板「开始巡检」即可采集它们的最新文章",
        });
        setText("");
        void qc.invalidateQueries({ queryKey: ["accounts"] });
        void qc.invalidateQueries({ queryKey: ["sweep-status"] });
      } else {
        toast.error("没有添加任何公众号", { description: "每行都被拒绝了，原因见下方" });
      }
    },
    onError: (e) => {
      setProgress(null);
      toast.error("批量添加失败", { description: msg(e) });
    },
  });
  const close = () => {
    if (submit.isPending) return;
    setResult(null);
    onClose();
  };
  return (
    <Dialog open={open} onOpenChange={(o) => !o && close()}>
      <DialogContent className="max-w-xl">
        <DialogHeader>
          <DialogTitle className="text-sm">批量添加公众号</DialogTitle>
          <DialogDescription className="flex items-center gap-1">
            <span>
              每行一条该公众号任意一篇<b className="text-foreground">图文文章的短链</b>，读出公众号信息即刻建号，不开微信。
            </span>
            <InfoTip>
              短链是文章页右上角「复制链接」得到的 https://mp.weixin.qq.com/s/… 形式。软件直接访问文章页读出公众号名称与
              biz 建号，之后由定时巡检持续采集它的最新文章。长链、视频 / 图片 / 文字消息类文章、已删除的文章会被拒绝并说明原因。
            </InfoTip>
          </DialogDescription>
        </DialogHeader>
        <textarea
          value={text}
          onChange={(e) => setText(e.target.value)}
          rows={8}
          spellCheck={false}
          disabled={submit.isPending}
          placeholder={"每行一条公众号文章短链，例如：\nhttps://mp.weixin.qq.com/s/xxxxxxxxxxxxxxxxxxxxxx"}
          className="w-full resize-y rounded-md border border-input bg-background px-3 py-2 font-mono text-xs leading-5 shadow-sm placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1 focus-visible:ring-offset-background disabled:opacity-60"
        />
        {submit.isPending && progress && (
          <div className="text-xs text-muted-foreground">
            <div className="flex items-center gap-2">
              <Loader2 className="size-3.5 animate-spin" />
              {progress.total > 0
                ? `正在解析第 ${Math.min(progress.done + 1, progress.total)} / ${progress.total} 条…`
                : "正在校验链接…"}
            </div>
            {progress.current && (
              <div className="mt-0.5 truncate font-mono text-xs" title={progress.current}>
                {progress.current}
              </div>
            )}
            {progress.total > 0 && (
              <Progress value={Math.round((progress.done / progress.total) * 100)} className="mt-1.5 h-1.5" />
            )}
          </div>
        )}
        {result && (
          <div className="max-h-56 overflow-y-auto rounded-md bg-muted/50 p-2 text-xs">
            <div>
              新增 <b>{result.added}</b> 个
              {result.updated > 0 && (
                <>
                  ，更新 <b>{result.updated}</b> 个
                </>
              )}
              {result.rejected > 0 && (
                <>
                  ，拒绝 <b>{result.rejected}</b> 条
                </>
              )}
              {result.skipped > 0 && (
                <>
                  ，未处理 <b>{result.skipped}</b> 条
                </>
              )}
            </div>
            <ul className="mt-1 space-y-0.5">
              {result.items.map((r, i) => (
                <li key={i} className="flex min-w-0 items-center gap-2" title={r.line}>
                  <AddLinkBadge status={r.status} />
                  <span className="truncate">
                    {r.nickname ? (
                      <span className="text-foreground">{r.nickname}</span>
                    ) : (
                      <span className="text-foreground">{r.reason}</span>
                    )}
                    <span className="text-muted-foreground"> · {r.line}</span>
                  </span>
                </li>
              ))}
            </ul>
          </div>
        )}
        <DialogFooter className="gap-2 sm:justify-between">
          <div className="text-xs text-muted-foreground">
            {lines.length > 0 ? `${lines.length} 行` : " "}
          </div>
          <div className="flex gap-2">
            {result !== null && result.added + result.updated > 0 && !sweepOn && (
              <Button variant="outline" size="sm" onClick={() => onNav("dashboard")}>
                去控制面板开始巡检
              </Button>
            )}
            <Button variant="outline" size="sm" onClick={close} disabled={submit.isPending}>
              关闭
            </Button>
            <Button
              size="sm"
              disabled={lines.length === 0}
              loading={submit.isPending}
              onClick={() => submit.mutate(lines)}
            >
              添加
            </Button>
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export function Accounts({ onNav }: { onNav: (v: View) => void }) {
  const {
    setArticlesBiz,
    runDetail,
    detailRunning,
    detailError,
    refreshCounts,
    cfg,
    running: sweepOn,
    stopping: sweepStopping,
  } = useApp();
  const qc = useQueryClient();

  // 运行状态：有执行单元（巡检批次 / 历史页 / 单号重试）在处理时「重新巡检」不可用；
  // 主循环长驻但单元之间空闲时可用。
  const run = useQuery({
    queryKey: ["run-status"],
    queryFn: runStatus,
    refetchInterval: 3_000,
  });
  const running = run.data?.running ?? false;

  const [batchOpen, setBatchOpen] = useState(false);

  /** 「重新巡检」：立即执行——凭证有效直采；不可用则当场单链接接力换 key 再采（toast 说明走了哪条路）。 */
  const retrySweep = useMutation({
    mutationFn: (biz: string) => sweepRetryAccount(biz, cfg),
    onSuccess: (r) => {
      if (r.collected) {
        toast.success(
          `重新巡检完成：${r.relayed ? "接力换 key 后" : ""}采到 ${r.total} 篇，新增 ${r.new_articles} 篇`,
          { description: r.message },
        );
      } else {
        toast.warning("重新巡检未成功", { description: r.message });
      }
    },
    onError: (e) => toast.error("重新巡检失败", { description: msg(e) }),
    onSettled: () => {
      void qc.invalidateQueries({ queryKey: ["accounts"] });
      void qc.invalidateQueries({ queryKey: ["articles"] });
      void qc.invalidateQueries({ queryKey: ["sweep-status"] });
      void qc.invalidateQueries({ queryKey: ["run-status"] });
      void refreshCounts();
    },
  });

  // —— 历史文章抓取：唯一名额；活动任务（running / paused）来自 store 的 2 秒轮询 ——
  const { history, refreshHistory } = useApp();
  const activeHistory = isActiveJob(history?.job) ? history!.job : null;
  const [historyOf, setHistoryOf] = useState<Account | null>(null);
  const afterHistoryOp = () => {
    void refreshHistory();
    void qc.invalidateQueries({ queryKey: ["accounts"] });
  };
  const pauseHistory = useMutation({
    mutationFn: (biz: string) => historyPause(biz, cfg),
    onSuccess: () => toast.success("历史抓取已暂停", { description: "当前页结束后生效；可随时继续" }),
    onError: (e) => toast.error("暂停失败", { description: msg(e) }),
    onSettled: afterHistoryOp,
  });
  const resumeHistory = useMutation({
    mutationFn: (biz: string) => historyResume(biz, cfg),
    onSuccess: () => toast.success("历史抓取已继续"),
    onError: (e) => toast.error("继续失败", { description: msg(e) }),
    onSettled: afterHistoryOp,
  });
  const cancelHistory = useMutation({
    mutationFn: (biz: string) => historyCancel(biz, cfg),
    onSuccess: () => toast.success("历史抓取已取消", { description: "已抓到的文章保留在库里" }),
    onError: (e) => toast.error("取消失败", { description: msg(e) }),
    onSettled: afterHistoryOp,
  });

  // 待确认删除的公众号（null = 关闭确认框）。
  const [pendingDelete, setPendingDelete] = useState<Account | null>(null);
  // 正在查看凭证数据的公众号（null = 关闭对话框）。
  const [credOf, setCredOf] = useState<Account | null>(null);

  /** 确认后执行：软删除公众号 + 级联软删除其全部文章。失败留在确认框里显示，便于重试。 */
  const removeAccount = useMutation({
    mutationFn: (biz: string) => deleteAccount(biz),
    onSuccess: (articleCount, biz) => {
      const name = pendingDelete?.nickname ?? biz;
      toast.success(`已删除「${name}」及其 ${articleCount} 篇文章`, {
        description: "软删除，数据仍保留在库中；重新采到该号会自动恢复",
      });
      setPendingDelete(null);
      void qc.invalidateQueries({ queryKey: ["accounts"] });
      void qc.invalidateQueries({ queryKey: ["articles"] });
      void refreshCounts();
    },
    onError: (e) => toast.error("删除公众号失败", { description: msg(e) }),
  });

  // 翻页（0 起）。查询键以 ["accounts"] 开头，别处 invalidate 时各页一起失效。
  const [page, setPage] = useState(0);

  // 每 30 秒重拉一次，让凭证热数据（剩余有效期 / 回放次数）跟着采集进度刷新。
  const accounts = useQuery({
    queryKey: ["accounts", "page", page],
    queryFn: () => listAccounts(PAGE_SIZE, page * PAGE_SIZE),
    refetchInterval: 30_000,
  });

  const total = accounts.data?.total ?? 0;
  const pageCount = Math.max(1, Math.ceil(total / PAGE_SIZE));

  // 删到当前页空了（且不是第一页）就退回上一页，避免停在空页。
  useEffect(() => {
    if (accounts.isSuccess && page > 0 && page + 1 > pageCount) {
      setPage(pageCount - 1);
    }
  }, [accounts.isSuccess, page, pageCount]);

  // 当前时刻（秒），每 10 秒走一格，驱动「剩余 N 分钟」「N 分钟前」的相对显示。
  const [now, setNow] = useState(() => Date.now() / 1000);
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now() / 1000), 10_000);
    return () => clearInterval(t);
  }, []);

  /** 暂停 / 恢复该号的定时巡检。 */
  const toggleSweep = useMutation({
    mutationFn: ({ biz, enabled }: { biz: string; enabled: boolean; name: string }) =>
      setAccountSweepEnabled(biz, enabled),
    onSuccess: (_r, v) =>
      toast.success(v.enabled ? `已恢复「${v.name}」的巡检` : `已暂停「${v.name}」的巡检`, {
        description: v.enabled ? "下一轮起重新参与" : "该号不再参与定时巡检，可随时恢复",
      }),
    onError: (e, v) =>
      toast.error(v.enabled ? "恢复巡检失败" : "暂停巡检失败", { description: msg(e) }),
    onSettled: () => {
      void qc.invalidateQueries({ queryKey: ["accounts"] });
      void qc.invalidateQueries({ queryKey: ["sweep-status"] });
    },
  });

  /** 补采该号所有缺详情的文章；结束后失效相关列表。 */
  const fetchDetails = useMutation({
    mutationFn: (biz: string) => runDetail(biz),
    onSettled: () => {
      void qc.invalidateQueries({ queryKey: ["accounts"] });
      void qc.invalidateQueries({ queryKey: ["articles"] });
    },
  });

  /** 跳到「公众号文章」并带上该号过滤。 */
  const gotoArticles = useCallback(
    (biz: string) => {
      setArticlesBiz(biz);
      onNav("articles");
    },
    [setArticlesBiz, onNav],
  );

  const rows = accounts.data?.items ?? [];

  return (
    <div>
      <div className="mb-3">
        <h1 className="text-xl font-semibold tracking-tight">公众号列表</h1>
        <p className="mt-0.5 text-xs text-muted-foreground">
          整链采集到的公众号、凭证有效期、最新一篇文章的发布时间、定时巡检与历史抓取进度；操作里可抓取历史文章（一次一个号），「更多」里可查看凭证完整数据、暂停 / 恢复该号巡检
        </p>
      </div>

      <DetailProgressBar className="mb-3" />
      {detailError && (
        <div className="mb-3 text-xs text-destructive">{detailError}</div>
      )}

      <Card>
        <CardHeader className="flex-row items-center justify-between space-y-0 p-4 pb-2">
          <div>
            <CardTitle className="text-sm">公众号</CardTitle>
            <CardDescription className="mt-1">
              {accounts.isSuccess ? `共 ${total} 个` : " "}
            </CardDescription>
          </div>
          <div className="flex items-center gap-1">
            <Button size="sm" onClick={() => setBatchOpen(true)} title="粘贴多条文章短链，批量添加公众号">
              <ListPlus />
              批量添加
            </Button>
            <Button
              variant="ghost"
              size="sm"
              loading={accounts.isFetching}
              onClick={() => void accounts.refetch()}
            >
              {!accounts.isFetching && <RefreshCw />}
              刷新
            </Button>
          </div>
        </CardHeader>
        <CardContent className="p-2 pt-0">
          {accounts.isPending && (
            <div className="p-4 text-sm text-muted-foreground">加载中…</div>
          )}
          {accounts.isError && (
            <div className="p-4 text-sm text-destructive">
              list_accounts 失败：{msg(accounts.error)}
            </div>
          )}
          {accounts.isSuccess && rows.length === 0 && (
            <div className="p-4 text-sm text-muted-foreground">
              还没有公众号。点右上角「批量添加」粘贴文章短链即可建号，之后在控制面板「开始巡检」采集它们的最新文章。
            </div>
          )}
          {accounts.isSuccess && rows.length > 0 && (
            <Table className="min-w-[1120px]">
              <TableHeader>
                <TableRow>
                  <TableHead className="w-12">头像</TableHead>
                  <TableHead>名称</TableHead>
                  <TableHead>__biz</TableHead>
                  <TableHead className="w-36">凭证</TableHead>
                  <TableHead className="w-36" title="该号已采到的最新一篇文章的发布时间">
                    最新更新
                  </TableHead>
                  <TableHead className="w-36" title="定时巡检：上次处理结果与时刻；「更多」里可暂停 / 恢复">
                    巡检
                  </TableHead>
                  <TableHead className="w-48" title="历史文章抓取：按号往旧翻页的长任务，一次只跑一个号">
                    历史抓取
                  </TableHead>
                  <TableHead className="w-48">操作</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {rows.map((a) => (
                  <TableRow key={a.biz}>
                    <TableCell>
                      <Avatar account={a} />
                    </TableCell>
                    <TableCell className="font-medium">
                      {a.nickname ?? (
                        <span className="text-muted-foreground">(未命名)</span>
                      )}
                    </TableCell>
                    <TableCell>
                      <code className="rounded bg-muted px-1.5 py-0.5 text-xs">
                        {a.biz}
                      </code>
                    </TableCell>
                    <TableCell>
                      <CredBadge account={a} now={now} />
                    </TableCell>
                    <TableCell
                      className="whitespace-nowrap text-xs tabular-nums text-muted-foreground"
                      title={
                        a.latest_published_at
                          ? `最新一篇发布于 ${fmtLogDateTime(a.latest_published_at)}`
                          : "该号还没有带发布时间的文章"
                      }
                    >
                      {recent(a.latest_published_at, now)}
                    </TableCell>
                    <TableCell className="whitespace-nowrap">
                      <SweepBadge account={a} now={now} />
                    </TableCell>
                    <TableCell>
                      <HistoryCell
                        job={activeHistory?.biz === a.biz ? activeHistory : a.history}
                        now={now}
                      />
                    </TableCell>
                    <TableCell>
                      <div className="flex items-center gap-0.5">
                        {activeHistory?.biz === a.biz ? (
                          <>
                            {activeHistory.status === "running" ? (
                              <Button
                                variant="ghost"
                                size="sm"
                                className="px-2 text-link"
                                title="暂停历史抓取（当前页结束后生效）"
                                loading={pauseHistory.isPending}
                                onClick={() => pauseHistory.mutate(a.biz)}
                              >
                                {!pauseHistory.isPending && <Pause />}
                              </Button>
                            ) : (
                              <Button
                                variant="ghost"
                                size="sm"
                                className="px-2 text-link"
                                title={
                                  activeHistory.paused_reason && activeHistory.paused_reason !== "user"
                                    ? "继续历史抓取（暂停原因未消除时会再次自动暂停）"
                                    : "继续历史抓取"
                                }
                                loading={resumeHistory.isPending}
                                onClick={() => resumeHistory.mutate(a.biz)}
                              >
                                {!resumeHistory.isPending && <Play />}
                              </Button>
                            )}
                            <Button
                              variant="ghost"
                              size="sm"
                              className="px-2 text-muted-foreground hover:text-destructive"
                              title="取消历史抓取（已抓到的文章保留）"
                              loading={cancelHistory.isPending}
                              onClick={() => cancelHistory.mutate(a.biz)}
                            >
                              {!cancelHistory.isPending && <XCircle />}
                            </Button>
                          </>
                        ) : (
                          <Button
                            variant="ghost"
                            size="sm"
                            className="px-2 text-link"
                            disabled={!!activeHistory || sweepOn}
                            title={
                              activeHistory
                                ? `『${activeHistory.nickname ?? activeHistory.biz}』的历史抓取进行中，同一时刻只抓一个号`
                                : sweepOn
                                  ? sweepStopping
                                    ? "巡检正在停止：等当前批次结束后可用"
                                    : "巡检进行中：先在控制面板「停止巡检」，等本批结束后可用"
                                  : "抓取历史文章：从最新一页往旧翻，按条数 / 时间范围设目标"
                            }
                            onClick={() => setHistoryOf(a)}
                          >
                            <History />
                          </Button>
                        )}
                        <Button
                          variant="ghost"
                          size="sm"
                          className="px-2 text-link"
                          title="查看该号的文章列表"
                          onClick={() => gotoArticles(a.biz)}
                        >
                          <Newspaper />
                        </Button>
                        <Button
                          variant="ghost"
                          size="sm"
                          className="px-2 text-link"
                          disabled={detailRunning && !fetchDetails.isPending}
                          loading={fetchDetails.isPending && fetchDetails.variables === a.biz}
                          title="并发抓取该号所有缺详情的文章正文（Markdown）"
                          onClick={() => fetchDetails.mutate(a.biz)}
                        >
                          {!(fetchDetails.isPending && fetchDetails.variables === a.biz) && (
                            <DownloadCloud />
                          )}
                        </Button>
                        <Button
                          variant="ghost"
                          size="sm"
                          className="px-2 text-link"
                          disabled={
                            !a.sweep_enabled ||
                            running ||
                            !!activeHistory ||
                            (retrySweep.isPending && retrySweep.variables !== a.biz)
                          }
                          loading={retrySweep.isPending && retrySweep.variables === a.biz}
                          title={
                            !a.sweep_enabled
                              ? "该号已暂停巡检"
                              : activeHistory
                                ? `『${activeHistory.nickname ?? activeHistory.biz}』的历史抓取未结束，巡检与历史抓取只能跑一个`
                                : running && !retrySweep.isPending
                                  ? "巡检进行中，完成后可用"
                                  : a.cred_fresh
                                  ? "重新巡检：凭证有效，立刻直接获取最新文章列表"
                                  : "重新巡检：凭证不可用，立刻打开该号最新一篇换 key 后采集（会起代理 / 开微信）"
                          }
                          onClick={() => retrySweep.mutate(a.biz)}
                        >
                          {!(retrySweep.isPending && retrySweep.variables === a.biz) && <RotateCw />}
                        </Button>
                        <Button
                          variant="ghost"
                          size="sm"
                          className="px-2 text-muted-foreground hover:text-destructive"
                          title="删除该公众号（会同步删除相关文章）"
                          onClick={() => setPendingDelete(a)}
                        >
                          <Trash2 />
                        </Button>
                        <DropdownMenu>
                          <DropdownMenuTrigger asChild>
                            <Button
                              variant="ghost"
                              size="sm"
                              className="px-2 text-muted-foreground"
                              title="更多"
                            >
                              <MoreHorizontal />
                            </Button>
                          </DropdownMenuTrigger>
                          <DropdownMenuContent align="end">
                            <DropdownMenuItem onSelect={() => setCredOf(a)}>
                              <KeyRound />
                              查看凭证数据
                            </DropdownMenuItem>
                            <DropdownMenuItem
                              disabled={toggleSweep.isPending}
                              onSelect={() =>
                                toggleSweep.mutate({
                                  biz: a.biz,
                                  enabled: !a.sweep_enabled,
                                  name: a.nickname ?? a.biz,
                                })
                              }
                            >
                              {toggleSweep.isPending && toggleSweep.variables?.biz === a.biz ? (
                                <Loader2 className="animate-spin" />
                              ) : a.sweep_enabled ? (
                                <PauseCircle />
                              ) : (
                                <PlayCircle />
                              )}
                              {a.sweep_enabled ? "暂停巡检" : "恢复巡检"}
                            </DropdownMenuItem>
                          </DropdownMenuContent>
                        </DropdownMenu>
                      </div>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
          {accounts.isSuccess && (
            <Pager page={page} pageCount={pageCount} total={total} unit="个" busy={accounts.isFetching} onPage={setPage} />
          )}
        </CardContent>
      </Card>

      <HistoryStartDialog
        account={historyOf}
        cfg={cfg}
        onClose={() => setHistoryOf(null)}
        onStarted={afterHistoryOp}
      />

      <BatchAddDialog
        open={batchOpen}
        sweepOn={sweepOn}
        onClose={() => setBatchOpen(false)}
        onNav={onNav}
      />

      <CredentialDialog account={credOf} now={now} onClose={() => setCredOf(null)} />

      {/* 删除确认：明确告知会级联删除相关文章（软删除） */}
      <Dialog
        open={pendingDelete !== null}
        onOpenChange={(open) => !open && setPendingDelete(null)}
      >
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle className="text-sm">
              删除公众号「{pendingDelete?.nickname ?? pendingDelete?.biz}」？
            </DialogTitle>
            <DialogDescription className="leading-relaxed">
              删除后该公众号将从列表中消失，并会<b className="text-foreground">同步删除它的全部相关文章</b>。
              这是软删除——数据仍保留在数据库中，之后重新采集到该号会自动恢复。
            </DialogDescription>
          </DialogHeader>
          {removeAccount.isError && (
            <div className="text-xs text-destructive">删除失败：{msg(removeAccount.error)}</div>
          )}
          <DialogFooter>
            <Button variant="outline" size="sm" onClick={() => setPendingDelete(null)}>
              取消
            </Button>
            <Button
              variant="destructive"
              size="sm"
              loading={removeAccount.isPending}
              onClick={() => {
                if (pendingDelete) removeAccount.mutate(pendingDelete.biz);
              }}
            >
              {!removeAccount.isPending && <Trash2 />}
              确认删除
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
