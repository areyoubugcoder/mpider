/** 原生采集：从当前微信主页自动识别公众号，展示只读目标和采集进度。 */
import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { useApp } from "@/store";
import {
  nativeStatus, nativeStart, nativeResume, nativeStop, nativeEnvironment,
  nativeSessionExport, ratelimitExport, revealInFolder, fmtLogDateTime,
  type NativeConfig,
} from "@/lib/tauri";
import { Card, CardHeader, CardTitle, CardDescription, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Table, TableBody, TableRow, TableHead, TableHeader, TableCell } from "@/components/ui/table";

const defaults: NativeConfig = {
  enabled: true, transport: "session", session_file: "", python_path: "python",
  window_title: "", expected_biz: null, min_gap_seconds: 90, max_gap_seconds: 120,
  daily_budget: 60, max_pages: 1, response_timeout_seconds: 45,
};
const labels: Record<string, string> = {
  running: "采集中", paused: "已暂停", done: "采集完成", cancelled: "已结束",
  failed: "失败", ok: "成功", pending: "等待响应", interrupted: "采样结束",
  timeout: "超时", invalid_payload: "校验未通过", error: "接口异常", rate_limited: "频率限制信号",
};
function load(): NativeConfig {
  try {
    // 历史配置只复用参数，新任务总是重新识别当前页面，不复用旧目标或会话。
    return { ...defaults, ...JSON.parse(localStorage.getItem("mpider-native-config") || "{}"),
      transport: "session", session_file: "", window_title: "", expected_biz: null };
  } catch { return defaults; }
}
export function NativeCollector() {
  const { cfg, system } = useApp();
  const queryClient = useQueryClient();
  const status = useQuery({ queryKey: ["native-status"], queryFn: nativeStatus, refetchInterval: 1000 });
  const [config, setConfig] = useState<NativeConfig>(load);
  const [busy, setBusy] = useState(false);
  const job = status.data?.active;
  const latestJob = job ?? status.data?.recent[0];
  const windows = system?.platform === "windows";
  const detectedName = job?.nickname || "";
  const current = job?.config ?? config;
  const phase = job?.status === "running" && !detectedName ? "正在识别当前公众号…"
    : job?.status === "running" && job.pages === 0 ? "公众号已识别，正在准备采集…"
    : latestJob ? labels[latestJob.status] || latestJob.status : "待开始";
  async function act(fn: () => Promise<unknown>) {
    setBusy(true);
    try { await fn(); await queryClient.invalidateQueries({ queryKey: ["native-status"] }); }
    catch (e) { toast.error(String(e)); }
    finally { setBusy(false); }
  }
  const number = (key: "min_gap_seconds" | "max_gap_seconds" | "daily_budget" | "max_pages", label: string, min: number) => (
    <div className="space-y-1.5">
      <Label htmlFor={key}>{label}</Label>
      <Input id={key} type="number" min={min} max={key === "max_pages" ? 500 : undefined}
        value={current[key]} disabled={busy || !!job}
        onChange={(e) => setConfig({ ...config, [key]: Number(e.target.value) })} />
    </div>
  );
  return (
    <div className="mx-auto max-w-6xl space-y-4">
      <div>
        <h1 className="text-xl font-semibold">原生采集</h1>
        <p className="mt-1 text-sm text-muted-foreground">在微信中打开公众号主页，点击开始后自动识别并采集文章。</p>
      </div>
      {!windows && <Alert><AlertDescription>请在已登录微信的 Windows 专机运行。此处可查看已有采集记录。</AlertDescription></Alert>}
      <Card>
        <CardHeader>
          <CardTitle className="text-base">采集当前公众号</CardTitle>
          <CardDescription>程序会刷新当前主页，读取公众号名称，再自动翻页并去重入库。</CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="grid gap-4 md:grid-cols-[1fr_180px]">
            <div className="space-y-1.5">
              <Label htmlFor="detected-account">当前公众号 · 自动识别</Label>
              <Input id="detected-account" readOnly aria-readonly="true" value={detectedName}
                placeholder={job?.status === "running" ? "正在从微信请求中识别…" : "开始后自动识别，无需填写"}
                className="bg-muted/40" />
            </div>
            {number("max_pages", "最多采集页数", 1)}
          </div>
          <div className="flex flex-wrap gap-2">
            {!job && <Button disabled={!windows || busy || !!status.data?.running || status.isPending || !!status.error}
              onClick={() => act(async () => {
                const next: NativeConfig = { ...config, transport: "session", window_title: "", expected_biz: null, session_file: "" };
                localStorage.setItem("mpider-native-config", JSON.stringify(next));
                await nativeStart(next, cfg);
              })}>{busy ? "正在启动…" : "识别并开始采集"}</Button>}
            {job?.status === "running" && <Button variant="outline" disabled={busy} onClick={() => act(() => nativeStop(job.id))}>暂停采集</Button>}
            {job?.status === "paused" && <Button disabled={busy || !windows || status.data?.running}
              onClick={() => act(() => nativeResume(job.id, cfg))}>继续采集</Button>}
            {job && <Button variant="ghost" disabled={busy} onClick={() => act(() => nativeStop(job.id, true))}>结束任务</Button>}
          </div>
          <p className="text-xs text-muted-foreground">识别时请保持微信已登录、桌面未锁屏；有多个公众号窗口时，请只保留要采集的主页。</p>
        </CardContent>
      </Card>
      {status.error && <Alert variant="destructive"><AlertDescription>无法读取任务状态，请检查后台日志后重试。</AlertDescription></Alert>}
      <Card aria-live="polite">
        <CardHeader>
          <CardTitle className="text-base">{phase}</CardTitle>
          <CardDescription>{!job && latestJob?.nickname ? `上次采集：${latestJob.nickname}` : "公众号识别后将显示在上方，文章会自动去重。"}</CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          <div className="grid grid-cols-3 gap-4 text-sm">
            <div className="space-y-1"><p className="text-muted-foreground">已处理页数</p><p className="text-xl font-semibold">{latestJob?.pages ?? 0}</p></div>
            <div className="space-y-1"><p className="text-muted-foreground">新增文章</p><p className="text-xl font-semibold">{latestJob?.new_articles ?? 0}</p></div>
            <div className="space-y-1"><p className="text-muted-foreground">今日请求</p><p className="text-xl font-semibold">{status.data?.calls_24h ?? 0}</p></div>
          </div>
          {job?.next_action_at && job.status === "running" && <p className="text-sm text-muted-foreground">下一次请求不早于 {fmtLogDateTime(job.next_action_at)}</p>}
          {job?.status === "paused" && <Alert variant="warning"><AlertDescription>
            {job.pause_reason === "manual" ? "采集已暂停。" : job.pause_reason === "restart" ? "应用重启，采集已暂停。" : `采集已暂停：${job.pause_reason || "请检查微信页面"}。`}
            {job.config.transport === "session" ? "保持原公众号主页打开，点击继续将重新识别并从检查点续跑。" : "保持原公众号主页打开，点击继续将从首页读取并去重。"}
          </AlertDescription></Alert>}
        </CardContent>
      </Card>
      <details className="rounded-xl border bg-card p-4">
        <summary className="cursor-pointer text-sm font-medium">高级设置</summary>
        <div className="mt-4 space-y-4">
          <div className="space-y-1.5"><Label htmlFor="python-path">Python 可执行文件</Label>
            <Input id="python-path" value={current.python_path} disabled={busy || !!job}
              onChange={(e) => setConfig({ ...config, python_path: e.target.value })} />
            <p className="text-xs text-muted-foreground">默认自动使用程序旁的运行环境。会话采集适配微信 4.1.15.56。</p>
          </div>
          <div className="grid gap-4 md:grid-cols-3">
            {number("daily_budget", "近 24 小时请求预算", 1)}
            {number("min_gap_seconds", "最小请求间隔（秒）", 0)}
            {number("max_gap_seconds", "最大请求间隔（秒）", 0)}
          </div>
          <Button variant="outline" disabled={busy || !windows} onClick={() => act(async () => {
            const env = await nativeEnvironment(current.python_path);
            const missing = [!env.python && "Python", !env.frida && "Frida", !env.cryptography && "cryptography"].filter(Boolean);
            if (missing.length) toast.error(`缺少 ${missing.join("、")}，请运行 tools/setup-native-runtime.ps1 准备环境`);
            else toast.success("采集环境依赖已就绪");
          })}>检查采集环境</Button>
        </div>
      </details>
      <details className="rounded-xl border bg-card p-4">
        <summary className="cursor-pointer text-sm font-medium">诊断与导出</summary>
        <div className="mt-4 space-y-4">
          <div className="flex flex-wrap gap-2">
            <Button variant="outline" disabled={busy || !windows} onClick={() => act(async () => {
              const path = await nativeSessionExport(current.python_path); await revealInFolder(path);
              toast.success("完整会话已导出，文件包含密钥，请妥善保存");
            })}>导出会话材料（含密钥）</Button>
            <Button variant="outline" disabled={busy} onClick={() => act(async () => {
              const file = await ratelimitExport(30 * 86400); await revealInFolder(file.path); toast.success("请求数据已导出");
            })}>导出请求记录</Button>
          </div>
          <p className="text-xs text-muted-foreground">最近 24 小时的请求明细，最多显示 1000 条。</p>
          <Table>
            <TableHeader>
              <TableRow>
                {[
                  "时间",
                  "任务",
                  "请求类型",
                  "结果",
                  "实际间隔",
                  "耗时",
                  "桥 / JSAPI / 业务码",
                  "文章 / 新增",
                  "游标",
                ].map((x) => (
                  <TableHead key={x}>{x}</TableHead>
                ))}
              </TableRow>
            </TableHeader>
            <TableBody>
              {status.data?.calls.map((c) => (
                <TableRow key={c.id} title={c.reason || undefined}>
                  <TableCell className="whitespace-nowrap font-mono text-xs">
                    {fmtLogDateTime(c.ts)}
                  </TableCell>
                  <TableCell>#{c.job_id}</TableCell>
                  <TableCell>
                    {c.cgi === 5814
                      ? `列表 · ${c.action === 0 ? "首页" : "翻页"}`
                      : "文章补充"}
                    <div className="text-xs text-muted-foreground">
                      CGI {c.cgi}
                      {c.transport === "mmtls" ? " · 独立请求" : " · 微信页面"}
                    </div>
                  </TableCell>
                  <TableCell>{labels[c.outcome] || c.outcome}</TableCell>
                  <TableCell>
                    {c.gap_ms == null
                      ? "—"
                      : `${(c.gap_ms / 1000).toFixed(1)}s`}
                  </TableCell>
                  <TableCell>
                    {c.latency_ms == null ? "—" : `${c.latency_ms}ms`}
                  </TableCell>
                  <TableCell className="font-mono">
                    {c.transport === "mmtls" ? (c.outcome === "ok" ? `认证通过 / ${c.inner_ret ?? "—"}` : `— / ${c.inner_ret ?? "—"}`) :
                    [c.outer_ret, c.jsapi_ret, c.inner_ret]
                      .map((x) => x ?? "—")
                      .join(" / ")}
                  </TableCell>
                  <TableCell>
                    {c.articles} / {c.new_articles}
                  </TableCell>
                  <TableCell>
                    {c.cursor_advanced == null
                      ? "—"
                      : c.cursor_advanced
                        ? "已推进"
                        : "未变化"}
                  </TableCell>
                </TableRow>
              ))}
              {!status.data?.calls.length && (
                <TableRow>
                  <TableCell
                    colSpan={9}
                    className="py-8 text-center text-muted-foreground"
                  >
                    暂无原生请求记录
                  </TableCell>
                </TableRow>
              )}
            </TableBody>
          </Table>
        </div>
      </details>
    </div>
  );
}
