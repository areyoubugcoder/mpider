import { useEffect, useState, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { DownloadCloud, Play, RadioTower, Square, TriangleAlert } from "lucide-react";
import { toast } from "sonner";
import { useApp } from "@/store";
import {
  clearCooldown,
  fmtLogDateTime,
  seedServerStatus,
  sweepRestartPass,
  sweepStatus,
  wxActive,
} from "@/lib/tauri";
import { isActiveJob } from "@/lib/history";
import { InfoTip } from "@/components/InfoTip";
import { msg } from "@/lib/utils";
import {
  Card,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { DetailProgressBar } from "@/components/DetailProgressBar";
import { RpaSelfCheck } from "@/components/RpaSelfCheck";
import { SeedSetup } from "@/components/SeedSetup";
import type { View } from "@/views/types";

/** 统计卡（对齐 shadcn dashboard-01 的 SectionCards：描述 + 大数值 + 徽章）。 */
/**
 * 人工模式（mac / 关闭 RPA）的「待命页」状态：微信内置浏览器是否停在待命页上等任务。
 * 在线 = 后续批次全自动；未打开 = 下一批开始时会提醒打开一篇文章（打开一篇后本批跑完就会自动停到待命页）。
 * 只在不能自动点种子的平台显示；文案只说现状与要做的一件事，不解释原理。
 */
function StandbyCard() {
  const status = useQuery({
    queryKey: ["seed-server-status"],
    queryFn: seedServerStatus,
    refetchInterval: 5000,
  });
  const st = status.data;
  const alive = !!st?.resident_alive;
  const ago =
    st?.resident_last_poll_at != null
      ? Math.max(0, Math.round(Date.now() / 1000 - st.resident_last_poll_at))
      : null;
  return (
    <Card className="mb-3">
      <CardHeader className="p-3.5">
        <div className="flex items-center justify-between gap-2">
          <div>
            <CardDescription className="text-xs">微信内置浏览器待命页</CardDescription>
            <CardTitle className="text-base">
              {st == null ? "…" : alive ? "在线" : "未打开"}
              {alive && ago != null && (
                <span className="ml-2 text-xs font-normal text-muted-foreground tabular-nums">
                  {ago} 秒前有心跳
                </span>
              )}
            </CardTitle>
          </div>
          <Badge variant={alive ? "success" : "muted"}>
            {alive ? "后续批次自动接力" : "下一批会提醒你打开一篇文章"}
          </Badge>
        </div>
        <p className="mt-1 text-xs text-muted-foreground">
          {alive
            ? "别关闭微信里那个待命窗口，采集任务会自动在里面打开。"
            : "开始巡检后按提醒在微信里打开一篇公众号文章即可；本批跑完浏览器会自动停在待命页，之后不用再管。"}
        </p>
      </CardHeader>
    </Card>
  );
}

function StatCard({
  label,
  value,
  badge,
  hint,
}: {
  label: string;
  value: string;
  badge?: { text: string; variant: "success" | "warning" | "muted" };
  hint?: string;
}) {
  return (
    <Card title={hint}>
      <CardHeader className="p-3.5">
        <CardDescription className="text-xs">{label}</CardDescription>
        <div className="flex items-end justify-between gap-2">
          <CardTitle className="text-xl tabular-nums tracking-tight">
            {value}
          </CardTitle>
          {badge && <Badge variant={badge.variant}>{badge.text}</Badge>}
        </div>
      </CardHeader>
    </Card>
  );
}

/** 秒数 → 「N 小时 M 分」/「M 分」。 */
function fmtDur(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  if (s >= 3600) return `${Math.floor(s / 3600)} 小时 ${Math.floor((s % 3600) / 60)} 分`;
  if (s >= 60) return `${Math.floor(s / 60)} 分`;
  return `${s} 秒`;
}

/**
 * 巡检卡（控制面板主操作区）：左侧「开始 / 停止巡检」大按钮，中间一行状态（未开启 / 第 N 轮进度 / 停留 /
 * 暂停 / 退避 / 停止中）+ 明细（参与号数、轮数、当前号预算）+ 进度条，右侧「立即开始新一轮」「解除退避」与
 * 调用方传入的次级操作（补详情）。进度每 5 秒拉一次 sweep_status。
 */
function SweepCard({
  running,
  stopping,
  pollBusy,
  activeHistory,
  reportEnabled,
  onStart,
  onStop,
  onNav,
  actions,
  footer,
}: {
  /** 巡检开关打开（含停止中）。 */
  running: boolean;
  /** 已点「停止巡检」、等当前批次收尾。 */
  stopping: boolean;
  /** 启停请求在途。 */
  pollBusy: boolean;
  /** 活动的历史抓取任务（进行中 / 暂停）：有则不能开始巡检。 */
  activeHistory: { biz: string; nickname: string | null; status: string } | null;
  reportEnabled: boolean;
  onStart: () => void;
  onStop: () => void;
  onNav: (v: View) => void;
  /** 右侧次级操作（补详情按钮）。 */
  actions?: ReactNode;
  /** 卡片底部（补详情进度条 / 错误）。 */
  footer?: ReactNode;
}) {
  const qc = useQueryClient();
  const q = useQuery({
    queryKey: ["sweep-status"],
    queryFn: sweepStatus,
    refetchInterval: 5_000,
  });
  const [now, setNow] = useState(() => Date.now() / 1000);
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now() / 1000), 5_000);
    return () => clearInterval(t);
  }, []);
  const lift = useMutation({
    mutationFn: clearCooldown,
    onSuccess: () => toast.success("已解除限流退避", { description: "主循环立即恢复领任务" }),
    onError: (e) => toast.error("解除退避失败", { description: msg(e) }),
    onSettled: () => void qc.invalidateQueries({ queryKey: ["sweep-status"] }),
  });
  const restart = useMutation({
    mutationFn: sweepRestartPass,
    onSuccess: () =>
      toast.success("已请求开始新一轮巡检", {
        description: running
          ? "跳过停留 / 暂停，所有号重新排队，下一批起生效"
          : "巡检没在跑：下次「开始巡检」即从新一轮开始",
      }),
    onError: (e) => toast.error("开始新一轮失败", { description: msg(e) }),
    onSettled: () => {
      void qc.invalidateQueries({ queryKey: ["sweep-status"] });
      void qc.invalidateQueries({ queryKey: ["accounts"] });
    },
  });
  const s = q.data;
  const cooling = !!s?.cooldown_until && s.cooldown_until > now;

  let headline: string;
  let badge: { text: string; variant: "success" | "warning" | "muted" | "destructive" };
  if (!running || !s || s.phase === "off") {
    headline = "按批更新全库公众号的最新文章";
    badge = { text: "未开启", variant: "muted" };
  } else if (stopping) {
    headline = `正在停止：等当前批次结束（第 ${s.passes_completed + 1} 轮 ${s.pass_done}/${s.pass_total} 个号）`;
    badge = { text: "停止中", variant: "warning" };
  } else if (cooling) {
    headline = `整机限流退避中，${fmtDur((s.cooldown_until ?? now) - now)}后恢复`;
    badge = { text: `退避 ${s.cooldown_level} 级`, variant: "destructive" };
  } else if (s.paused_until && s.paused_until > now) {
    headline = `巡检暂停中，${fmtDur(s.paused_until - now)}后继续：${s.paused_reason ?? ""}`;
    badge = { text: "已暂停", variant: "warning" };
  } else if (s.phase === "waiting") {
    headline = s.next_pass_at
      ? `本轮已完成，${fmtDur(s.next_pass_at - now)}后开始下一轮（${fmtLogDateTime(s.next_pass_at)}）`
      : "等待下一轮";
    badge = { text: "停留中", variant: "success" };
  } else {
    headline = `第 ${s.passes_completed + 1} 轮巡检中：${s.pass_done}/${s.pass_total} 个号，新文章 ${s.pass_new_articles} 篇`;
    badge = { text: "巡检中", variant: "success" };
  }
  const pct =
    s && running && s.phase === "running" && s.pass_total > 0
      ? Math.min(100, Math.round((s.pass_done / s.pass_total) * 100))
      : null;

  return (
    <Card className="mb-3">
      <div className="flex flex-wrap items-center gap-4 p-5">
        <Button
          size="lg"
          variant={running ? "destructive" : "default"}
          loading={pollBusy}
          disabled={(!running && !!activeHistory) || (running && stopping)}
          onClick={running ? onStop : onStart}
          className="h-12 px-7 text-base font-semibold shadow-sm [&_svg]:size-5"
          title={
            running
              ? stopping
                ? "已发出停止信号，等当前批次结束"
                : "发停止信号；正在处理的批次会先跑完再停止"
              : activeHistory
                ? `『${activeHistory.nickname ?? activeHistory.biz}』的历史抓取未结束，先在公众号列表取消它再开始巡检`
                : "后台长驻：按批打开各号最新一篇换凭证、采最新文章列表，一轮跑完停留后再来"
          }
        >
          {!pollBusy && (running ? <Square /> : <Play />)}
          {pollBusy
            ? running
              ? "停止中…"
              : "启动中…"
            : running
              ? stopping
                ? "停止中…"
                : "停止巡检"
              : "开始巡检"}
        </Button>
        <div className="min-w-[260px] flex-1">
          <div className="flex flex-wrap items-center gap-2 text-sm font-semibold">
            <RadioTower className="size-4 text-muted-foreground" />
            定时巡检
            <Badge variant={badge.variant}>{badge.text}</Badge>
            {!running && activeHistory && (
              <button
                type="button"
                onClick={() => onNav("accounts")}
                className="text-xs font-normal text-[#e0a53a] underline-offset-2 hover:underline"
                title="巡检与历史抓取只能跑一个；点击去公众号列表"
              >
                历史抓取{activeHistory.status === "running" ? "进行中" : "已暂停"}，不能开巡检
              </button>
            )}
          </div>
          <p className="mt-0.5 flex items-center gap-1 text-xs text-muted-foreground">
            <span className="min-w-0 truncate">{headline}</span>
            <InfoTip>
              开始后按批打开各号最新一篇文章换凭证，再直连采最新文章列表入库
              {reportEnabled && "，每号采完当场上报"}
              ；一轮跑完停留一段时间再来，「停止巡检」在当前批次结束后生效。巡检与历史抓取同一时刻只能跑一个。
            </InfoTip>
          </p>
          {s && (
            <p className="mt-0.5 flex items-center gap-1 text-xs text-muted-foreground">
              <span className="min-w-0 truncate">
                参与 {s.total_enabled} 个号
                {s.passes_completed > 0 && ` · 已完成 ${s.passes_completed} 轮`}
                {` · 近 24 小时 ${s.list_calls_24h}${s.list_daily_budget > 0 ? ` / ${s.list_daily_budget}` : " 次"}`}
              </span>
              <InfoTip>
                「参与」= 未被排除的公众号数；「近 24 小时」= 当前微信号的列表请求已用 / 预算，达到预算巡检自动暂停。
                批次大小、轮间停留、预算在
                <button
                  type="button"
                  onClick={() => onNav("settings")}
                  className="mx-0.5 text-link underline underline-offset-2 hover:text-link/80"
                >
                  「系统设置」
                </button>
                调；逐条进度看
                <button
                  type="button"
                  onClick={() => onNav("logs")}
                  className="mx-0.5 text-link underline underline-offset-2 hover:text-link/80"
                >
                  「日志」
                </button>
                。
              </InfoTip>
            </p>
          )}
          {cooling && s?.cooldown_reason && (
            <p className="mt-0.5 text-xs text-destructive">原因：{s.cooldown_reason}</p>
          )}
          {!cooling && running && s?.last_error && s.phase !== "off" && (
            <p className="mt-0.5 text-xs text-muted-foreground">{s.last_error}</p>
          )}
          {pct !== null && (
            <div className="mt-2 h-1.5 w-full overflow-hidden rounded bg-muted">
              <div className="h-full bg-primary transition-all" style={{ width: `${pct}%` }} />
            </div>
          )}
        </div>
        <div className="flex w-full flex-wrap items-center justify-end gap-2 xl:w-auto">
          {cooling && (
            <Button
              variant="outline"
              size="sm"
              loading={lift.isPending}
              title="确认是坏链假阳性、并非真限流时，手动解除退避立即恢复领任务"
              onClick={() => lift.mutate()}
            >
              解除退避
            </Button>
          )}
          <Button
            variant="outline"
            size="sm"
            loading={restart.isPending}
            disabled={stopping}
            title="跳过轮间停留 / 环境故障暂停，所有号（含本轮已记失败的）重新排队；巡检没在跑时下次开始即开新一轮"
            onClick={() => restart.mutate()}
          >
            立即开始新一轮
          </Button>
          {actions}
        </div>
      </div>
      {footer}
    </Card>
  );
}

export function Dashboard({ onNav }: { onNav: (v: View) => void }) {
  const {
    health,
    certInstalled,
    system,
    counts,
    refreshHealth,
    refreshCert,
    refreshCounts,
    running,
    stopping,
    pollBusy,
    reportError,
    startSweep,
    stopSweep,
    history,
    detailRunning,
    detailError,
    runDetail,
    cfg,
    autoClick,
  } = useApp();

  // 进入控制面板即实时复查（告警条/状态卡自愈）。
  useEffect(() => {
    void refreshHealth();
    void refreshCert();
    void refreshCounts();
  }, [refreshHealth, refreshCert, refreshCounts]);

  const qc = useQueryClient();

  /** 全局补详情：结束后失效列表缓存。 */
  const detailAll = useMutation({
    mutationFn: () => runDetail(),
    onSettled: () => void qc.invalidateQueries(),
  });

  const pendingCount = counts?.pending ?? 0;

  // 历史抓取与巡检互斥：有活动的历史任务（进行中 / 暂停）时不能开始巡检。
  const activeHistory = isActiveJob(history?.job) ? history!.job : null;

  // 当前激活的微信号（采集只用它；没有就引导去微信号管理新增）。
  const active = useQuery({
    queryKey: ["wx-active"],
    queryFn: wxActive,
    refetchInterval: 5_000,
  });
  const wx = active.data ?? null;
  const wxStatusText = !wx
    ? null
    : wx.status === "blocked" && wx.blocked_until
      ? `受限至 ${fmtLogDateTime(wx.blocked_until)}`
      : wx.status === "budget"
        ? `预算已用完（${wx.budget_used_24h} / ${wx.budget}）`
        : `正常 · 近 24 小时 ${wx.budget_used_24h}${wx.budget > 0 ? ` / ${wx.budget}` : ""}`;

  return (
    <div>
      <div className="mb-3 flex flex-wrap items-end justify-between gap-2">
        <div>
          <h1 className="text-xl font-semibold tracking-tight">控制面板</h1>
          <p className="mt-0.5 text-xs text-muted-foreground">
            开始 / 停止巡检、查看环境状态与前置自检
          </p>
        </div>
        <button
          type="button"
          onClick={() => onNav("wxaccounts")}
          className="flex items-center gap-2 rounded-md border bg-card px-3 py-1.5 text-left text-xs hover:bg-accent"
          title="采集只用当前激活的微信号；点击进入微信号管理"
        >
          <span className="text-muted-foreground">当前微信号</span>
          {wx ? (
            <>
              <span className="font-medium">{wx.alias}</span>
              <Badge
                variant={
                  wx.status === "blocked" ? "destructive" : wx.status === "budget" ? "warning" : "success"
                }
              >
                {wxStatusText}
              </Badge>
              {autoClick && wx.calibrated !== "ok" && (
                <Badge variant="warning">{wx.calibrated === "none" ? "未标定" : "标定失效"}</Badge>
              )}
            </>
          ) : active.isLoading ? (
            <span className="text-muted-foreground">…</span>
          ) : (
            <span className="text-muted-foreground">尚未登记（抓到凭证后自动登记）</span>
          )}
        </button>
      </div>

      {/* 证书未装告警条：仅 certInstalled === false 时显示（单一数据源） */}
      {certInstalled === false && (
        <Alert variant="warning" className="mb-4">
          <TriangleAlert />
          <AlertDescription className="flex items-center gap-3">
            <span>还没安装证书，直接抓取会失败。</span>
            <button
              type="button"
              onClick={() => onNav("settings")}
              className="font-semibold underline underline-offset-2"
            >
              去系统设置一键安装 →
            </button>
          </AlertDescription>
        </Alert>
      )}

      {/* 主操作区：开始/停止巡检 + 巡检进度 / 退避是本页最重的内容，放最上；补详情作为次级操作靠右。 */}
      <SweepCard
        running={running}
        stopping={stopping}
        pollBusy={pollBusy}
        activeHistory={activeHistory}
        reportEnabled={cfg.report_enabled}
        onStart={() => void startSweep()}
        onStop={() => void stopSweep()}
        onNav={onNav}
        actions={
          <Button
            variant="outline"
            size="sm"
            loading={detailRunning}
            disabled={pendingCount === 0}
            title={
              pendingCount > 0
                ? `并发抓取全部 ${pendingCount} 篇缺详情的文章正文（Markdown）`
                : "没有待补详情的文章"
            }
            onClick={() => detailAll.mutate()}
          >
            {!detailRunning && <DownloadCloud />}
            {detailRunning
              ? "抓取详情中…"
              : `抓取全部详情${pendingCount > 0 ? `（待补 ${pendingCount}）` : ""}`}
          </Button>
        }
        footer={
          <>
            {/* 补详情进度 / 错误：仅有内容时才占位 */}
            <DetailProgressBar className="mx-5 mb-4" />
            {(detailError || reportError) && (
              <div className="space-y-1 px-5 pb-4 text-xs text-destructive">
                {detailError && <div>{detailError}</div>}
                {reportError && <div>{reportError}</div>}
              </div>
            )}
          </>
        }
      />

      {/* 状态区（SectionCards） */}
      <div className="mb-3 grid grid-cols-2 gap-2.5 lg:grid-cols-4">
        <StatCard
          label="证书"
          value={
            certInstalled === null
              ? "检查中"
              : certInstalled
                ? "已安装"
                : "未安装"
          }
          badge={
            certInstalled === null
              ? { text: "…", variant: "muted" }
              : certInstalled
                ? { text: "就绪", variant: "success" }
                : { text: "需安装", variant: "warning" }
          }
          hint="根证书是否已装进系统信任库"
        />
        <StatCard
          label="微信"
          value={system ? (system.wechat_running ? "运行中" : "未启动") : "…"}
          badge={
            system?.wechat_running
              ? { text: "就绪", variant: "success" }
              : { text: "待启动", variant: "muted" }
          }
          hint="微信进程是否在运行"
        />
        <StatCard
          label="公众号"
          value={health ? String(health.account_count) : "–"}
          hint="已采集到的公众号数量"
        />
        <StatCard
          label="文章详情"
          value={counts ? `${counts.detail_done}/${counts.total}` : "–"}
          badge={
            counts
              ? pendingCount > 0
                ? { text: `待补 ${pendingCount}`, variant: "warning" }
                : { text: "已齐", variant: "success" }
              : undefined
          }
          hint="已采正文 / 文章总数"
        />
      </div>

      {/* 种子链接首次设置 + 微信自动化自检：只有能自动点种子的平台（Windows）才有这两件事；
          人工模式（mac）运行时用全局提醒告诉用户打开一篇文章即可，这里整体不显示，不增加学习成本 */}
      {autoClick ? (
        <>
          <SeedSetup />
          <RpaSelfCheck />
        </>
      ) : (
        <StandbyCard />
      )}
    </div>
  );
}
