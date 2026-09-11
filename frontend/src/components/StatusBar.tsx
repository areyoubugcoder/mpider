import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { BellRing } from "lucide-react";
import { useApp } from "@/store";
import { StatusDot, type DotTone } from "@/components/StatusDot";
import { historyLine } from "@/lib/history";
import { sweepStatus, type SweepInfo } from "@/lib/tauri";
import { cn } from "@/lib/utils";
import type { View } from "@/views/types";

/** 一个状态项：彩色圆点 + 标签 + 值。 */
function StatItem({
  tone,
  label,
  value,
  title,
}: {
  tone: DotTone;
  label: string;
  value: string;
  title: string;
}) {
  return (
    <span className="inline-flex items-center gap-1.5 whitespace-nowrap" title={title}>
      <StatusDot tone={tone} />
      {label} <b className="font-semibold text-foreground">{value}</b>
    </span>
  );
}

/** 巡检一行文案（状态栏用）：未开启 / 停止中 / 退避 / 暂停 / 停留 / 第 N 轮进度。 */
function sweepLine(on: boolean, stopping: boolean, s: SweepInfo | undefined, now: number): { text: string; tone: DotTone } {
  if (!on || !s || s.phase === "off") return { text: "未开启", tone: "off" };
  if (stopping) return { text: `停止中 · 等本批结束（${s.pass_done}/${s.pass_total}）`, tone: "warn" };
  if (s.cooldown_until && s.cooldown_until > now) {
    return { text: `退避中 · ${Math.ceil((s.cooldown_until - now) / 60)} 分后恢复`, tone: "err" };
  }
  if (s.paused_until && s.paused_until > now) {
    return { text: `暂停中 · ${Math.ceil((s.paused_until - now) / 60)} 分后继续`, tone: "warn" };
  }
  if (s.phase === "waiting") {
    return {
      text: s.next_pass_at ? `等待下一轮 · ${Math.max(1, Math.ceil((s.next_pass_at - now) / 60))} 分后` : "等待下一轮",
      tone: "ok",
    };
  }
  return { text: `第 ${s.passes_completed + 1} 轮 · ${s.pass_done}/${s.pass_total} 个号 · 新文章 ${s.pass_new_articles}`, tone: "ok" };
}

/**
 * 底部常驻状态栏。
 *
 * - 常驻项：证书 / 微信 —— 与任务无关的环境前提，随时可看。
 * - 任务项：巡检（开关 / 轮进度 / 停止中）与历史抓取进度，两者互斥、各占一格。
 * - 运行期项：全局代理 / 抓包代理 —— 两者只在一次抓取的运行窗口内临时开启
 *   （结束由 SystemProxyGuard/capture_stop 自动复位），平时显示没有意义，
 *   仅在任务活跃（前端点了运行，或后端抓包代理实际在跑——含托盘触发）时出现。
 *
 * 证书项读 store.certInstalled（单一数据源），其余读 store.system（6s/2s 轮询）。
 */
export function StatusBar({ onNav }: { onNav?: (v: View) => void }) {
  const { certInstalled, system, running, stopping, loopRunning, history, alert } = useApp();

  // 巡检进度：开关打开时 3 秒拉一次，关着时 15 秒（只为对齐「未开启」）。
  const sweep = useQuery({
    queryKey: ["sweep-status"],
    queryFn: sweepStatus,
    refetchInterval: running ? 3_000 : 15_000,
  });

  // 当前时刻（秒），每秒走一格，驱动「下一页 X 秒后」倒计时。
  const [now, setNow] = useState(() => Date.now() / 1000);
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now() / 1000), 1000);
    return () => clearInterval(t);
  }, []);

  // 历史抓取：有活动任务（running / paused）才显示。
  const job = history?.job ?? null;
  const histText = job ? historyLine(job, now) : null;
  const histTone: DotTone = !job
    ? "off"
    : job.status === "running"
      ? "ok"
      : job.paused_reason === "user"
        ? "off"
        : "warn";

  // 预算：当前微信号近 24 小时列表请求；≥80% 变黄，达到上限变红。
  const budget = history?.budget ?? 0;
  const used = history?.budget_used ?? 0;
  const ratio = budget > 0 ? used / budget : 0;
  const budgetTone: DotTone =
    budget <= 0 ? "off" : ratio >= 1 ? "err" : ratio >= 0.8 ? "warn" : "ok";
  const budgetValue = budget > 0 ? `${used}/${budget}` : `${used}/不限`;

  const sw = sweepLine(running, stopping, sweep.data, now);

  // 任务活跃 = 后端主循环在跑（巡检 / 历史），或抓包代理实际在跑。
  const taskActive = loopRunning || !!system?.capture_running;

  // 证书：单一数据源。null=检查中(off) / true=已装(ok) / false=未装(warn)。
  const certTone: DotTone =
    certInstalled === null ? "off" : certInstalled ? "ok" : "warn";
  const certValue =
    certInstalled === null ? "检查中" : certInstalled ? "已安装" : "未安装";

  const wechatTone: DotTone = system?.wechat_running ? "ok" : "off";
  const wechatValue = system
    ? system.wechat_running
      ? "运行中"
      : "未启动"
    : "…";

  const proxyTone: DotTone = system?.proxy_enabled ? "ok" : "off";
  const proxyValue = system?.proxy_enabled
    ? `就绪 ${system.proxy_endpoint ?? ""}`.trim()
    : "未开启";

  const captureTone: DotTone = system?.capture_running ? "ok" : "off";
  const captureValue = system?.capture_running
    ? (system.capture_addr ?? "运行中")
    : "启动中…";

  return (
    <footer className="flex h-9 shrink-0 select-none items-center gap-3 border-t border-border bg-sidebar px-4 text-xs text-sidebar-foreground">
      <div className="flex items-center gap-5 overflow-hidden">
        {alert && (
          <button
            type="button"
            className="inline-flex items-center text-destructive"
            title={`有未确认的提醒：${alert.message}`}
            onClick={() => onNav?.("accounts")}
          >
            <BellRing className="size-3.5 animate-pulse" />
          </button>
        )}
        <StatItem
          tone={certTone}
          label="证书"
          value={certValue}
          title="根证书是否已装进系统信任库"
        />
        <StatItem
          tone={wechatTone}
          label="微信"
          value={wechatValue}
          title="微信进程是否在运行（≈是否已打开/登录；精确登录态需 RPA）"
        />
        <StatItem
          tone={budgetTone}
          label="预算"
          value={budgetValue}
          title={`当前微信号近 24 小时列表请求 已用 / 预算${
            history ? `（为巡检保留 ${history.budget_reserve} 次）` : ""
          }`}
        />
        <button
          type="button"
          className={cn(
            "inline-flex min-w-0 items-center gap-1.5 truncate whitespace-nowrap hover:text-foreground",
            sw.tone === "warn" && "text-warning",
            sw.tone === "err" && "text-destructive",
          )}
          title="定时巡检状态，点击去控制面板"
          onClick={() => onNav?.("dashboard")}
        >
          <StatusDot tone={sw.tone} />
          <span className="truncate">
            巡检 <b className="font-semibold text-foreground">{sw.text}</b>
          </span>
        </button>
        {job && histText && (
          <button
            type="button"
            className={cn(
              "inline-flex min-w-0 items-center gap-1.5 truncate whitespace-nowrap hover:text-foreground",
              histTone === "warn" && "text-warning",
            )}
            title="历史文章抓取进度，点击查看公众号列表"
            onClick={() => onNav?.("accounts")}
          >
            <StatusDot tone={histTone} />
            <span className="truncate">
              历史：<b className="font-semibold text-foreground">{job.nickname ?? job.biz}</b> · {histText}
            </span>
          </button>
        )}
        {taskActive && (
          <>
            <span className="inline-flex items-center gap-1.5 whitespace-nowrap font-semibold text-foreground">
              <span className="inline-block size-2 shrink-0 animate-pulse rounded-full bg-primary" />
              任务运行中
            </span>
            <StatItem
              tone={captureTone}
              label="抓包代理"
              value={captureValue}
              title="本次运行临时启动的抓包代理（结束自动停止）"
            />
            <StatItem
              tone={proxyTone}
              label="全局代理"
              value={proxyValue}
              title="本次运行是否已把系统（全局）代理指向抓包代理（结束自动复位）"
            />
          </>
        )}
      </div>
    </footer>
  );
}
