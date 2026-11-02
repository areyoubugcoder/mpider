import { useEffect, useState } from "react";
import { useInfiniteQuery, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { Radio, Square } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Card, CardHeader, CardTitle, CardDescription, CardContent } from "@/components/ui/card";
import { Table, TableBody, TableRow, TableHead, TableHeader, TableCell } from "@/components/ui/table";
import { logUiError, sourcePage } from "@/lib/workbench";
import { recentPushEvents } from "@/lib/push-feed";
import { defaultPushConfig, pushStatus, pushEvents, startPushMonitor, stopPushMonitor, startPushQueue, stopPushQueue, pushPhase, pushQueuePhase, pushOutcome, type PushConfig, type PushEventCursor } from "@/lib/push-monitor";

const date = (value?: number | null) => value ? new Date(value * 1000).toLocaleString() : "—";
export function PushMonitor({ onArticles, onRate }: { onArticles: (biz: string) => void; onRate: () => void }) {
  const status = useQuery({ queryKey: ["push-monitor"], queryFn: pushStatus, refetchInterval: 2000 });
  const feed = useInfiniteQuery({
    queryKey: ["push-events"], gcTime: 0, initialPageParam: null as PushEventCursor | null,
    queryFn: ({ pageParam }) => pushEvents(pageParam),
    getNextPageParam: page => page.next_cursor ?? undefined,
    refetchInterval: 2000,
  });
  const [search, setSearch] = useState("");
  const [searchQuery, setSearchQuery] = useState("");
  useEffect(() => { const timer = setTimeout(() => setSearchQuery(search.trim()), 200); return () => clearTimeout(timer); }, [search]);
  const accounts = useInfiniteQuery({
    queryKey: ["push-scope-accounts", searchQuery], initialPageParam: 0,
    queryFn: ({ pageParam }) => sourcePage(searchQuery, 50, pageParam),
    getNextPageParam: (last, pages) => {
      const loaded = pages.reduce((count, page) => count + page.items.length, 0);
      return loaded < last.total ? loaded : undefined;
    },
  });
  const qc = useQueryClient();
  const [config, setConfig] = useState<PushConfig>(defaultPushConfig);
  const [allAccounts, setAllAccounts] = useState(true);
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState(false);
  const [queueBusy, setQueueBusy] = useState(false);
  const data = status.data;
  const active = !!data?.run.active;
  const processing = !!data?.run.queue.active;
  useEffect(() => { if (data && !dirty) { setConfig(data.run.config); setAllAccounts(!data.run.config.bizzes.length); } }, [data, dirty]);
  const change = (value: Partial<PushConfig>) => { setDirty(true); setConfig(previous => ({ ...previous, ...value })); };
  async function act() {
    setBusy(true);
    try {
      if (active) await stopPushMonitor();
      else { await startPushMonitor({ ...config, bizzes: allAccounts ? [] : config.bizzes }); setDirty(false); }
      await qc.invalidateQueries({ queryKey: ["push-monitor"] });
    } catch (e) { toast.error(String(e)); void logUiError("action_failed", String(e)).catch(() => {}); }
    finally { setBusy(false); }
  }
  async function processQueue() {
    setQueueBusy(true);
    try {
      if (processing) await stopPushQueue();
      else await startPushQueue();
      await qc.invalidateQueries({ queryKey: ["push-monitor"] });
    } catch (e) { toast.error(String(e)); void logUiError("action_failed", String(e)).catch(() => {}); }
    finally { setQueueBusy(false); }
  }
  const number = (key: "scan_seconds" | "recent_hours", text: string, min: number, max: number) => <div className="space-y-2"><Label htmlFor={key}>{text}</Label><Input id={key} type="number" min={min} max={max} disabled={active || busy} value={config[key]} onChange={e => change({ [key]: Number(e.target.value) })} /></div>;
  const events = recentPushEvents(feed.data?.pages ?? [], Date.now() / 1000);
  const options = accounts.data?.pages.flatMap(page => page.items) ?? [];
  return <div className="mx-auto max-w-7xl space-y-5">
    <header><h1 className="text-xl font-semibold">推送监测</h1><p className="mt-1 text-sm text-muted-foreground">扫描新链接与处理已保存链接分别控制，解析后的文章自动加入文章库。</p></header>
    {status.error && <p role="alert" className="rounded-lg border border-destructive p-3 text-sm">无法读取监测状态：{String(status.error)}</p>}
    {data && !data.supported && <p role="status" className="rounded-lg border p-3 text-sm">扫描新链接需在已登录微信的 Windows 电脑开启；此处仍可独立处理已保存的链接和查看记录。</p>}
    <Card><CardHeader><div className="flex flex-wrap items-center justify-between gap-4"><div><CardTitle className="flex items-center gap-2 text-base"><Radio className="size-4" aria-hidden="true" />后台发现新文章</CardTitle><CardDescription className="mt-2">持续扫描微信内存并追加新链接；停止监测不影响下方已开启的链接处理。与「主页识别」独立运行。</CardDescription></div><Button loading={busy} variant={active ? "outline" : "default"} disabled={!data?.supported || !!status.error || (!active && !allAccounts && !config.bizzes.length)} onClick={() => { void act(); }}>{active ? <Square className="mr-2 size-3" /> : null}{active ? "停止推送监测" : "开启推送监测"}</Button></div></CardHeader>
      <CardContent className="space-y-4"><p role="status" className={data?.run.phase === "failed" ? "text-sm text-destructive" : "text-sm font-medium"}>{pushPhase(data?.run)}</p>
        {data?.run.reason && <p className="text-sm text-muted-foreground">{data.run.reason}</p>}
        <p className="text-xs text-muted-foreground">上次扫描：{date(data?.run.last_scan_at)} · {data?.run.rounds ?? 0} 轮 · 最近一轮读取 {((data?.run.read_bytes ?? 0) / 1048576).toFixed(0)} MB · 微信进程 {data?.run.processes ?? 0}。监测记录保留 30 天；文章保存在文章库。</p>
      </CardContent></Card>
    <Card><CardHeader><div className="flex flex-wrap items-center justify-between gap-4"><div><CardTitle className="text-base">待处理链接</CardTitle><CardDescription className="mt-2">独立解析已保存的链接，无需开启监测或保持微信运行。处理开启后，新追加的链接也会自动处理；暂停处理不影响扫描。</CardDescription></div><Button loading={queueBusy} variant={processing ? "outline" : "default"} disabled={!data || !!status.error} onClick={() => { void processQueue(); }}>{processing ? <Square className="mr-2 size-3" /> : null}{processing ? "暂停处理" : "开始处理"}</Button></div></CardHeader>
      <CardContent className="space-y-4"><p role="status" className={data?.run.queue.phase === "failed" ? "text-sm text-destructive" : "text-sm font-medium"}>{pushQueuePhase(data?.run.queue, data?.pending)}</p>
        {data?.run.queue.reason && <p className="text-sm text-muted-foreground">{data.run.queue.reason}</p>}
        {processing && !!data?.pending && data.run.queue.next_fetch_at > Date.now() / 1000 && <p className="text-xs text-muted-foreground">文章解析不早于 {date(data.run.queue.next_fetch_at)} 继续。</p>}
        <div className="grid grid-cols-2 gap-4 lg:grid-cols-3">{[["待处理链接", data?.pending ?? 0], ["新增入库", data?.imported ?? 0], ["近24小时文章请求", `${data?.requests_24h ?? 0} / ${data?.daily_budget ?? "—"}`]].map(([name, value]) => <div key={name} className="rounded-lg bg-muted/40 p-3"><p className="text-xs text-muted-foreground">{name}</p><p className="mt-2 text-xl font-semibold tabular-nums">{value}</p></div>)}</div>
      </CardContent></Card>
    <Card><CardHeader><CardTitle className="text-base">监测范围与频率</CardTitle><CardDescription>开启监测时保存设置，仅影响之后发现的链接；已有链接保留发现时的范围与时间窗。停止和重启均保留待处理链接。</CardDescription></CardHeader><CardContent className="space-y-4">
      <div className="grid gap-4 md:grid-cols-2">{number("scan_seconds", "每轮扫描后的间隔（秒）", 5, 300)}{number("recent_hours", "发现时回溯文章时间窗（小时）", 1, 168)}</div>
      <fieldset disabled={active || busy} className="space-y-2"><legend className="mb-2 text-sm font-medium">公众号范围</legend>
        <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={allAccounts} onChange={e => { setAllAccounts(e.target.checked); setDirty(true); }} />全部公众号（包括新发现的公众号）</label>
        <details><summary className="cursor-pointer text-sm text-muted-foreground">选择已登记公众号{!allAccounts ? `（已选 ${config.bizzes.length} 个）` : ""}</summary>
          <div className="mt-3 space-y-3"><Input aria-label="搜索已登记公众号" placeholder="搜索公众号名称或 ID" value={search} onChange={e => setSearch(e.target.value)} />
            {accounts.error && <p role="alert" className="text-sm text-destructive">公众号读取失败：{String(accounts.error)} <button type="button" className="underline" onClick={() => { void accounts.refetch(); }}>重试</button></p>}
            <div className="grid max-h-48 gap-2 overflow-auto md:grid-cols-3">{options.map(account => <label key={account.biz} className="flex items-center gap-2 text-sm"><input type="checkbox" checked={!allAccounts && config.bizzes.includes(account.biz)} onChange={e => { setAllAccounts(false); change({ bizzes: e.target.checked ? allAccounts ? [account.biz] : Array.from(new Set([...config.bizzes, account.biz])) : config.bizzes.filter(b => b !== account.biz) }); }} />{account.nickname || account.biz}</label>)}</div>
            {!options.length && !accounts.error && <p className="text-sm text-muted-foreground">{accounts.isPending ? "正在读取公众号…" : searchQuery ? "没有匹配的公众号" : "暂无已登记公众号"}</p>}
            {accounts.hasNextPage && <Button type="button" variant="outline" size="sm" loading={accounts.isFetchingNextPage} onClick={() => { void accounts.fetchNextPage(); }}>加载更多公众号</Button>}
          </div>
        </details>
        {!allAccounts && !config.bizzes.length && <p className="text-sm text-muted-foreground">请至少选择一个公众号，或勾选全部公众号。</p>}
      </fieldset>
      <div className="flex flex-wrap items-center justify-between gap-3"><p className="text-xs text-muted-foreground">文章解析与文章下载共用下载间隔和近24小时文章预算。文章、图片的频率及重试设置统一在限流配置调整。</p><Button variant="outline" size="sm" onClick={onRate}>前往限流配置</Button></div>
    </CardContent></Card>
    <Card><CardHeader><div className="flex items-center justify-between gap-3"><div><CardTitle className="text-base">最近发现</CardTitle><CardDescription className="mt-2">仅查询最近一小时内首次发现的记录，首次加载 20 条，点击“加载更多”再读取下一批 20 条。发现时间不是微信推送送达时间；打开旧文章也可能产生记录。</CardDescription></div><Button variant="outline" onClick={() => onArticles("")}>查看文章库</Button></div></CardHeader>
      <CardContent className="space-y-4">
        {feed.error && <p role="alert" className="text-sm text-destructive">发现记录读取失败：{String(feed.error)} <button type="button" className="underline" onClick={() => { void feed.refetch(); }}>重试</button></p>}
        <Table><TableHeader><TableRow>{["发现时间", "公众号 / 文章", "发布时间", "入库结果"].map(s => <TableHead key={s}>{s}</TableHead>)}</TableRow></TableHeader><TableBody>
          {events.map(event => <TableRow key={event.id}><TableCell className="whitespace-nowrap text-xs">{date(event.first_seen_at)}</TableCell><TableCell><p className="text-xs text-muted-foreground">{event.nickname || "等待解析公众号"}</p>{event.article_id ? <button className="mt-1 text-left text-sm hover:underline" onClick={() => onArticles(event.biz)}>{event.title}</button> : <p className="mt-1 text-sm">{event.title || "文章链接已保存"}</p>}</TableCell><TableCell className="whitespace-nowrap text-xs">{date(event.published_at)}</TableCell><TableCell><p className="text-sm">{pushOutcome(event.outcome)}</p>{event.reason && <p className="mt-1 text-xs text-muted-foreground">{pushOutcome(event.reason)}</p>}</TableCell></TableRow>)}
          {!events.length && <TableRow><TableCell colSpan={4} className="py-8 text-center text-muted-foreground">{feed.isPending ? "正在读取…" : "最近一小时暂无发现记录。"}</TableCell></TableRow>}
        </TableBody></Table>
        {feed.hasNextPage ? <Button variant="outline" className="w-full" loading={feed.isFetchingNextPage} onClick={() => { void feed.fetchNextPage(); }}>加载更多</Button> : events.length > 0 && <p className="text-center text-xs text-muted-foreground">已显示最近一小时的全部记录</p>}
      </CardContent>
    </Card>
  </div>;
}
