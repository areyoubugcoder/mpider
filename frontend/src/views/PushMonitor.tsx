import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { Radio, Square } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Card, CardHeader, CardTitle, CardDescription, CardContent } from "@/components/ui/card";
import { Table, TableBody, TableRow, TableHead, TableHeader, TableCell } from "@/components/ui/table";
import { listAccounts } from "@/lib/tauri";
import { logUiError } from "@/lib/workbench";
import { defaultPushConfig, pushStatus, startPushMonitor, stopPushMonitor, pushPhase, pushOutcome, type PushConfig } from "@/lib/push-monitor";

const date = (value?: number | null) => value ? new Date(value*1000).toLocaleString() : "—";
export function PushMonitor({ onArticles }: { onArticles: (biz: string) => void }) {
  const status = useQuery({ queryKey: ["push-monitor"], queryFn: pushStatus, refetchInterval: 2000 });
  const accounts = useQuery({ queryKey: ["push-scope-accounts"], queryFn: async () => (await listAccounts()).items });
  const qc = useQueryClient();
  const [config, setConfig] = useState<PushConfig>(defaultPushConfig);
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState(false);
  const data = status.data;
  const active = !!data?.run.active;
  useEffect(() => { if (data && !dirty) setConfig(data.run.config); }, [data, dirty]);
  const change = (value: Partial<PushConfig>) => { setDirty(true); setConfig(previous => ({ ...previous, ...value })); };
  async function act() {
    setBusy(true);
    try { if (active) await stopPushMonitor(); else { await startPushMonitor(config); setDirty(false); } await qc.invalidateQueries({ queryKey: ["push-monitor"] }); }
    catch (e) { toast.error(String(e)); void logUiError("action_failed",String(e)).catch(() => {}); }
    finally { setBusy(false); }
  }
  const number = (key: "scan_seconds" | "recent_hours" | "hourly_budget" | "fetch_seconds", text: string, min: number, max: number) => <div className="space-y-2"><Label htmlFor={key}>{text}</Label><Input id={key} type="number" min={min} max={max} disabled={active || busy} value={config[key]} onChange={e => change({ [key]: Number(e.target.value) })} /></div>;
  return <div className="mx-auto max-w-7xl space-y-5">
    <header><h1 className="text-xl font-semibold">推送监测</h1><p className="mt-1 text-sm text-muted-foreground">持续发现本机微信中的文章链接，解析后自动加入文章库。</p></header>
    {status.error && <p role="alert" className="rounded-lg border border-destructive p-3 text-sm">无法读取监测状态：{String(status.error)}</p>}
    {data && !data.supported && <p role="status" className="rounded-lg border p-3 text-sm">请在已登录微信的 Windows 电脑开启推送监测。此处可以查看已有记录。</p>}
    <Card><CardHeader><div className="flex flex-wrap items-center justify-between gap-4"><div><CardTitle className="flex items-center gap-2 text-base"><Radio className="size-4" aria-hidden="true" />后台发现新文章</CardTitle><CardDescription className="mt-2">与「主页识别」独立运行，无需为每次扫描打开公众号主页。仅能发现微信内存中存在的链接。</CardDescription></div><Button loading={busy} variant={active ? "outline" : "default"} disabled={!data?.supported || !!status.error} onClick={() => { void act(); }}>{active ? <Square className="mr-2 size-3" /> : null}{active ? "停止推送监测" : "开启推送监测"}</Button></div></CardHeader>
      <CardContent className="space-y-4"><p role="status" className={data?.run.phase === "failed" ? "text-sm text-destructive" : "text-sm font-medium"}>{pushPhase(data?.run)}</p>
        {data?.run.reason && <p className="text-sm text-muted-foreground">{data.run.reason}</p>}
        {!!data?.run.next_fetch_at && data.run.next_fetch_at > Date.now()/1000 && <p className="text-xs text-muted-foreground">网页解析不早于 {date(data.run.next_fetch_at)} 继续；内存扫描和待处理队列保留。</p>}
        <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">{[["新增入库",data?.imported ?? 0],["待处理链接",data?.pending ?? 0],["近一小时网页请求",`${data?.requests_hour ?? 0} / ${data?.run.config.hourly_budget ?? 3600}`],["微信进程",data?.run.processes ?? 0]].map(([name,value]) => <div key={name} className="rounded-lg bg-muted/40 p-3"><p className="text-xs text-muted-foreground">{name}</p><p className="mt-2 text-xl font-semibold tabular-nums">{value}</p></div>)}</div>
        <p className="text-xs text-muted-foreground">上次扫描：{date(data?.run.last_scan_at)} · {data?.run.rounds ?? 0} 轮 · 最近一轮读取 {((data?.run.read_bytes ?? 0)/1048576).toFixed(0)} MB。监测记录保留 30 天；文章保存在文章库。</p>
      </CardContent></Card>
    <Card><CardHeader><CardTitle className="text-base">监测范围与频率</CardTitle><CardDescription>开启时保存设置。首次开启也会处理内存中的近期文章，停止和重启保留待处理链接。</CardDescription></CardHeader><CardContent className="space-y-4">
      <div className="grid gap-4 md:grid-cols-2">{number("scan_seconds","每轮扫描后的间隔（秒）",5,300)}{number("recent_hours","发现时回溯文章时间窗（小时）",1,168)}{number("fetch_seconds","网页请求间隔（秒）",1,60)}{number("hourly_budget","每小时网页请求上限",1,3600)}</div>
      <fieldset disabled={active || busy} className="space-y-2"><legend className="mb-2 text-sm font-medium">公众号范围</legend><label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={!config.bizzes.length} onChange={e => change({ bizzes: e.target.checked ? [] : accounts.data?.[0] ? [accounts.data[0].biz] : [] })} />全部公众号（包括新发现的公众号）</label>
        {!!accounts.data?.length && <details><summary className="cursor-pointer text-sm text-muted-foreground">选择已登记公众号{config.bizzes.length ? `（已选 ${config.bizzes.length} 个）` : ""}</summary><div className="mt-3 grid max-h-48 gap-2 overflow-auto md:grid-cols-3">{accounts.data.map(account => <label key={account.biz} className="flex items-center gap-2 text-sm"><input type="checkbox" checked={config.bizzes.includes(account.biz)} onChange={e => change({ bizzes: e.target.checked ? [...config.bizzes,account.biz] : config.bizzes.filter(b => b !== account.biz) })} />{account.nickname || account.biz}</label>)}</div></details>}
      </fieldset><p className="text-xs text-muted-foreground">网页解析使用独立请求间隔，默认每秒最多一次。失败等待 1 秒后重试，每条链接最多请求三次；不影响列表采集和正文下载的冷却。正文仍从文章库单独下载。</p>
    </CardContent></Card>
    <Card><CardHeader><div className="flex items-center justify-between gap-3"><div><CardTitle className="text-base">最近发现</CardTitle><CardDescription className="mt-2">显示最近 100 条候选。发现时间是软件观察到链接的时间，不是微信推送送达时间；打开旧文章也可能产生记录。</CardDescription></div><Button variant="outline" onClick={() => onArticles("")}>查看文章库</Button></div></CardHeader><CardContent><Table><TableHeader><TableRow>{["发现时间","公众号 / 文章","发布时间","入库结果"].map(s => <TableHead key={s}>{s}</TableHead>)}</TableRow></TableHeader><TableBody>
      {data?.events.map(event => <TableRow key={event.id}><TableCell className="whitespace-nowrap text-xs">{date(event.first_seen_at)}</TableCell><TableCell><p className="text-xs text-muted-foreground">{event.nickname || "等待解析公众号"}</p>{event.article_id ? <button className="mt-1 text-left text-sm hover:underline" onClick={() => onArticles(event.biz)}>{event.title}</button> : <p className="mt-1 text-sm">{event.title || "文章链接已保存"}</p>}</TableCell><TableCell className="whitespace-nowrap text-xs">{date(event.published_at)}</TableCell><TableCell><p className="text-sm">{pushOutcome(event.outcome)}</p>{event.reason && <p className="mt-1 text-xs text-muted-foreground">{pushOutcome(event.reason)}</p>}</TableCell></TableRow>)}
      {!data?.events.length && <TableRow><TableCell colSpan={4} className="py-8 text-center text-muted-foreground">{status.isPending ? "正在读取…" : "暂无发现记录。开启后保持微信登录，等待新推送。"}</TableCell></TableRow>}
    </TableBody></Table></CardContent></Card>
  </div>;
}
