import { useEffect, useMemo, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Download, Gauge, RotateCw, TriangleAlert } from "lucide-react";
import { toast } from "sonner";
import { useApp } from "@/store";
import {
  fmtLogDateTime,
  fmtLogTime,
  ratelimitExport,
  ratelimitStats,
  revealInFolder,
  SOURCE_LABELS,
  type CooldownLogRow,
  type FactorCounts,
  type PassSummary,
  type RateBucket,
  type RateLimitStats,
  type RunLogRow,
} from "@/lib/tauri";
import { cn } from "@/lib/utils";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Alert, AlertDescription } from "@/components/ui/alert";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Pager, usePaged } from "@/components/Pager";
import type { View } from "@/views/types";

/**
 * 限流分析：把后端 `ratelimit_stats` 的聚合结果按「先看结论 → 再看节奏 → 再看明细」呈现——
 * 顶部是当前退避状态与诊断结论，中间是关键指标卡 + 请求频率时间线 + 请求间隔分布，
 * 下面是巡检轮次 / 执行单元 / 退避记录三张表。图表是手写 SVG（无图表库），颜色走
 * `index.css` 的 `--viz-*` 语义变量（明暗两态各自校验过色觉安全）。
 */

/** 时间窗口选项（秒）。 */
const WINDOWS: { value: number; label: string }[] = [
  { value: 3600, label: "近 1 小时" },
  { value: 6 * 3600, label: "近 6 小时" },
  { value: 24 * 3600, label: "近 24 小时" },
  { value: 3 * 86400, label: "近 3 天" },
  { value: 7 * 86400, label: "近 7 天" },
];

/** 请求结果系列：堆叠顺序 = 图例顺序 = 色觉安全校验时的相邻顺序，不要随意调换。 */
const OUTCOMES: { key: keyof RateBucket; label: string; color: string }[] = [
  { key: "ok", label: "成功", color: "var(--viz-ok)" },
  { key: "expired", label: "凭证过期 (-3)", color: "var(--viz-expired)" },
  { key: "rate_limited", label: "限流 (429/网页)", color: "var(--viz-limited)" },
  { key: "blocked", label: "封禁 (-6)", color: "var(--viz-blocked)" },
  { key: "error", label: "其它错误", color: "var(--viz-error)" },
];

/** 秒 → 「N 小时 M 分」/「M 分 S 秒」/「S 秒」。 */
function fmtDur(secs: number): string {
  const s = Math.max(0, Math.round(secs));
  if (s >= 3600) return `${Math.floor(s / 3600)} 小时 ${Math.floor((s % 3600) / 60)} 分`;
  if (s >= 60) return `${Math.floor(s / 60)} 分 ${s % 60} 秒`;
  return `${s} 秒`;
}

/** 毫秒 → 「12.3s」/「1 分 05 秒」。 */
function fmtMs(ms: number): string {
  if (ms >= 60_000) {
    const m = Math.floor(ms / 60_000);
    const s = Math.round((ms % 60_000) / 1000);
    return `${m} 分 ${String(s).padStart(2, "0")} 秒`;
  }
  return `${(ms / 1000).toFixed(1)}s`;
}

/** 元素宽度（ResizeObserver）：SVG 图表按容器宽度重画，避免文字被 viewBox 拉伸。 */
function useWidth<T extends HTMLElement>(): [React.RefObject<T>, number] {
  const ref = useRef<T>(null);
  const [w, setW] = useState(0);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver((entries) => {
      for (const e of entries) setW(Math.floor(e.contentRect.width));
    });
    ro.observe(el);
    setW(el.clientWidth);
    return () => ro.disconnect();
  }, []);
  return [ref, w];
}

/** 指标卡：小标题 + 大数值 + 副文案（可带徽章）。 */
function Stat({
  label,
  value,
  sub,
  badge,
  tone,
}: {
  label: string;
  value: string;
  sub?: string;
  badge?: { text: string; variant: "success" | "warning" | "destructive" | "muted" };
  tone?: "warn" | "bad";
}) {
  return (
    <Card>
      <CardHeader className="p-3.5">
        <CardDescription className="text-xs">{label}</CardDescription>
        <div className="flex items-end justify-between gap-2">
          <CardTitle
            className={cn(
              "text-xl tabular-nums tracking-tight",
              tone === "bad" && "text-destructive",
              tone === "warn" && "text-warning",
            )}
          >
            {value}
          </CardTitle>
          {badge && <Badge variant={badge.variant}>{badge.text}</Badge>}
        </div>
        {sub && <p className="mt-0.5 text-[11.5px] leading-snug text-muted-foreground">{sub}</p>}
      </CardHeader>
    </Card>
  );
}

/** 请求频率时间线：每个时间桶一根堆叠柱（按结果分色），退避触发点在顶部打竖线标记。 */
function FrequencyChart({
  s,
  gateMinMs,
  gateMaxMs,
}: {
  s: RateLimitStats;
  gateMinMs: number;
  gateMaxMs: number;
}) {
  const [ref, width] = useWidth<HTMLDivElement>();
  const [hover, setHover] = useState<number | null>(null);
  const H = 220;
  const pad = { l: 40, r: 12, t: 18, b: 28 };
  const buckets = s.buckets;
  const n = buckets.length;
  const innerW = Math.max(0, width - pad.l - pad.r);
  const innerH = H - pad.t - pad.b;
  const maxCalls = Math.max(1, ...buckets.map((b) => b.calls));
  // 理论上限参考线：按配置的平均间隔，一个桶内最多能发多少次。
  const avgGap = Math.max(1, (gateMinMs + Math.max(gateMinMs, gateMaxMs)) / 2);
  const theoreticalMax = Math.floor((s.bucket_secs * 1000) / avgGap);
  const yMax = Math.max(maxCalls, Math.min(theoreticalMax, maxCalls * 1.6)) * 1.1;
  const y = (v: number) => pad.t + innerH - (v / yMax) * innerH;
  const slot = n > 0 ? innerW / n : 0;
  const barW = Math.max(2, slot - 2);
  const ticks = [0, 0.5, 1].map((f) => Math.round(yMax * f));
  const fmtBucket = (ts: number) =>
    s.bucket_secs >= 6 * 3600 ? fmtLogDateTime(ts).slice(5, 16) : fmtLogTime(ts).slice(0, 5);
  // 横轴标签：最多放 8 个，均匀抽样。
  const labelEvery = Math.max(1, Math.ceil(n / 8));

  return (
    <div ref={ref} className="relative w-full">
      {width > 0 && (
        <svg width={width} height={H} role="img" aria-label="列表请求频率时间线">
          {ticks.map((t) => (
            <g key={t}>
              <line
                x1={pad.l}
                x2={width - pad.r}
                y1={y(t)}
                y2={y(t)}
                stroke="hsl(var(--viz-grid))"
                strokeWidth={1}
              />
              <text
                x={pad.l - 6}
                y={y(t) + 4}
                textAnchor="end"
                className="fill-muted-foreground"
                fontSize={10}
              >
                {t}
              </text>
            </g>
          ))}
          {theoreticalMax > 0 && theoreticalMax <= yMax && (
            <g>
              <line
                x1={pad.l}
                x2={width - pad.r}
                y1={y(theoreticalMax)}
                y2={y(theoreticalMax)}
                stroke="hsl(var(--muted-foreground))"
                strokeDasharray="4 3"
                strokeWidth={1}
              />
              <text
                x={width - pad.r}
                y={y(theoreticalMax) - 4}
                textAnchor="end"
                className="fill-muted-foreground"
                fontSize={10}
              >
                按配置间隔的理论上限 ≈ {theoreticalMax} 次
              </text>
            </g>
          )}
          {buckets.map((b, i) => {
            const x = pad.l + i * slot + (slot - barW) / 2;
            let acc = 0;
            return (
              <g key={b.ts}>
                {OUTCOMES.map((o) => {
                  const v = Number(b[o.key]) || 0;
                  if (v <= 0) return null;
                  const y1 = y(acc + v);
                  const y0 = y(acc);
                  acc += v;
                  return (
                    <rect
                      key={o.key}
                      x={x}
                      y={y1}
                      width={barW}
                      height={Math.max(1, y0 - y1 - 1)}
                      rx={2}
                      fill={o.color}
                      opacity={hover === null || hover === i ? 1 : 0.55}
                    />
                  );
                })}
                {/* 命中区比柱子宽：整个桶列都可 hover */}
                <rect
                  x={pad.l + i * slot}
                  y={pad.t}
                  width={Math.max(1, slot)}
                  height={innerH}
                  fill="transparent"
                  onMouseEnter={() => setHover(i)}
                  onMouseLeave={() => setHover(null)}
                />
                {i % labelEvery === 0 && (
                  <text
                    x={pad.l + i * slot + slot / 2}
                    y={H - 8}
                    textAnchor="middle"
                    className="fill-muted-foreground"
                    fontSize={10}
                  >
                    {fmtBucket(b.ts)}
                  </text>
                )}
              </g>
            );
          })}
          {/* 退避触发点：顶部竖线标记（ip 橙 / account 红） */}
          {s.cooldowns
            .filter((c) => c.kind !== "clear")
            .map((c) => {
              const first = buckets[0]?.ts ?? c.ts;
              const idx = (c.ts - first) / s.bucket_secs;
              if (idx < 0 || idx > n) return null;
              const x = pad.l + idx * slot;
              return (
                <g key={c.id}>
                  <line
                    x1={x}
                    x2={x}
                    y1={pad.t - 6}
                    y2={pad.t + innerH}
                    stroke={c.kind === "account" ? "var(--viz-blocked)" : "var(--viz-limited)"}
                    strokeWidth={1.5}
                    strokeDasharray="3 2"
                  >
                    <title>
                      {fmtLogDateTime(c.ts)} {c.kind === "account" ? "账号级封禁" : "限流退避"}
                      {`（第 ${c.level} 级，${fmtDur(c.secs)}）：${c.reason}`}
                    </title>
                  </line>
                </g>
              );
            })}
          <line
            x1={pad.l}
            x2={width - pad.r}
            y1={pad.t + innerH}
            y2={pad.t + innerH}
            stroke="hsl(var(--border))"
          />
        </svg>
      )}
      {hover !== null && buckets[hover] && (
        <div
          className="pointer-events-none absolute top-2 z-10 rounded-md border bg-popover px-2.5 py-1.5 text-xs shadow-md"
          style={{
            left: Math.min(
              Math.max(0, pad.l + hover * slot - 60),
              Math.max(0, width - 190),
            ),
          }}
        >
          <div className="font-medium">
            {fmtLogDateTime(buckets[hover].ts)} 起 {fmtDur(s.bucket_secs)}
          </div>
          <div className="mt-0.5 tabular-nums">
            共 {buckets[hover].calls} 次
            {OUTCOMES.filter((o) => Number(buckets[hover][o.key]) > 0).map((o) => (
              <span key={o.key} className="ml-2 inline-flex items-center gap-1">
                <span className="inline-block size-2 rounded-sm" style={{ background: o.color }} />
                {o.label} {Number(buckets[hover][o.key])}
              </span>
            ))}
          </div>
          {buckets[hover].articles > 0 && (
            <div className="text-muted-foreground">
              解析 {buckets[hover].articles} 篇，新增 {buckets[hover].new_articles} 篇
            </div>
          )}
        </div>
      )}
      <div className="mt-1.5 flex flex-wrap gap-x-4 gap-y-1 text-[11.5px] text-muted-foreground">
        {OUTCOMES.map((o) => (
          <span key={o.key} className="inline-flex items-center gap-1.5">
            <span className="inline-block size-2.5 rounded-sm" style={{ background: o.color }} />
            {o.label}
          </span>
        ))}
        <span className="inline-flex items-center gap-1.5">
          <span
            className="inline-block h-3 w-0 border-l-2 border-dashed"
            style={{ borderColor: "var(--viz-limited)" }}
          />
          退避触发
        </span>
      </div>
    </div>
  );
}

/** 请求间隔分布：直方图；低于配置下限的格用限流色标出。 */
function GapChart({ s, gateMinMs, gateMaxMs }: { s: RateLimitStats; gateMinMs: number; gateMaxMs: number }) {
  const [ref, width] = useWidth<HTMLDivElement>();
  const H = 180;
  const pad = { l: 36, r: 12, t: 14, b: 26 };
  const bins = s.gaps.histogram;
  const innerW = Math.max(0, width - pad.l - pad.r);
  const innerH = H - pad.t - pad.b;
  const max = Math.max(1, ...bins.map((b) => b.count));
  const slot = bins.length > 0 ? innerW / bins.length : 0;
  const barW = Math.max(4, slot * 0.62);
  const y = (v: number) => pad.t + innerH - (v / (max * 1.15)) * innerH;
  // 「这一格整体低于闸门下限」：格上限 ≤ 下限。
  const belowGate = (upto: number) => upto <= gateMinMs;
  const inGate = (upto: number, prev: number) => prev >= gateMinMs && upto <= Math.max(gateMinMs, gateMaxMs);
  return (
    <div ref={ref} className="w-full">
      {width > 0 && (
        <svg width={width} height={H} role="img" aria-label="相邻两次列表请求的间隔分布">
          {[0, 0.5, 1].map((f) => {
            const v = Math.round(max * f);
            return (
              <g key={f}>
                <line
                  x1={pad.l}
                  x2={width - pad.r}
                  y1={y(v)}
                  y2={y(v)}
                  stroke="hsl(var(--viz-grid))"
                />
                <text x={pad.l - 6} y={y(v) + 4} textAnchor="end" fontSize={10} className="fill-muted-foreground">
                  {v}
                </text>
              </g>
            );
          })}
          {bins.map((b, i) => {
            const prev = i > 0 ? bins[i - 1].upto_ms : 0;
            const x = pad.l + i * slot + (slot - barW) / 2;
            const color = belowGate(b.upto_ms)
              ? "var(--viz-limited)"
              : inGate(b.upto_ms, prev)
                ? "var(--viz-ok)"
                : "var(--viz-bar)";
            return (
              <g key={b.label}>
                <rect x={x} y={y(b.count)} width={barW} height={Math.max(0, pad.t + innerH - y(b.count))} rx={3} fill={color}>
                  <title>{`${b.label}：${b.count} 次`}</title>
                </rect>
                {b.count > 0 && (
                  <text
                    x={x + barW / 2}
                    y={y(b.count) - 4}
                    textAnchor="middle"
                    fontSize={10}
                    className="fill-foreground tabular-nums"
                  >
                    {b.count}
                  </text>
                )}
                <text
                  x={x + barW / 2}
                  y={H - 8}
                  textAnchor="middle"
                  fontSize={10}
                  className="fill-muted-foreground"
                >
                  {b.label}
                </text>
              </g>
            );
          })}
          <line x1={pad.l} x2={width - pad.r} y1={pad.t + innerH} y2={pad.t + innerH} stroke="hsl(var(--border))" />
        </svg>
      )}
      <div className="mt-1.5 flex flex-wrap gap-x-4 gap-y-1 text-[11.5px] text-muted-foreground">
        <span className="inline-flex items-center gap-1.5">
          <span className="inline-block size-2.5 rounded-sm" style={{ background: "var(--viz-limited)" }} />
          低于闸门下限（过密）
        </span>
        <span className="inline-flex items-center gap-1.5">
          <span className="inline-block size-2.5 rounded-sm" style={{ background: "var(--viz-ok)" }} />
          在闸门区间内
        </span>
        <span className="inline-flex items-center gap-1.5">
          <span className="inline-block size-2.5 rounded-sm" style={{ background: "var(--viz-bar)" }} />
          高于区间（翻页等待 / 批次间隔 / 空闲）
        </span>
      </div>
    </div>
  );
}

/** 一句话诊断：按规则从统计里提炼出「问题 + 建议」，让用户不用读图也知道该调什么。 */
function diagnose(
  s: RateLimitStats,
  cfg: { gateMin: number; gateMax: number; batchSize: number; budget: number },
): { level: "ok" | "warn" | "bad"; text: string }[] {
  const out: { level: "ok" | "warn" | "bad"; text: string }[] = [];
  const t = s.totals;
  const now = s.generated_at;
  if (s.cooldown_now.until && s.cooldown_now.until > now) {
    out.push({
      level: "bad",
      text: `当前处于整机退避（第 ${s.cooldown_now.level} 级），${fmtDur(s.cooldown_now.until - now)}后恢复：${s.cooldown_now.reason}`,
    });
  }
  if (t.blocked > 0) {
    out.push({
      level: "bad",
      text: `出现 ${t.blocked} 次账号级限制（ret=-6）：微信号已被识别为异常，与凭证无关，换 key 无用。请保持退避至少 1 天，期间不要手动解除、不要再发列表请求；恢复后把每日预算与每批号数调低再观察。`,
    });
  }
  if (t.rate_limited > 0) {
    out.push({
      level: "warn",
      text: `窗口内 ${t.rate_limited} 次限流信号（429 / 5xx / 接口回网页），触发 IP 级退避 ${s.cooldown_hits} 次。建议：调大「列表请求间隔」（当前 ${fmtMs(cfg.gateMin)}–${fmtMs(cfg.gateMax)}）、减小「巡检每批号数」（当前 ${cfg.batchSize}）。`,
    });
  }
  const tooFast = s.gaps.recent_ms.filter((g) => g < cfg.gateMin).length;
  if (tooFast > 0) {
    out.push({
      level: "warn",
      text: `有 ${tooFast} 次请求间隔低于配置下限 ${fmtMs(cfg.gateMin)}（最短 ${fmtMs(s.gaps.min_ms)}）——闸门未按预期生效或有绕过闸门的请求路径，请结合日志核对。`,
    });
  }
  if (cfg.budget > 0 && s.uin_calls_24h >= cfg.budget * 0.8) {
    out.push({
      level: s.uin_calls_24h >= cfg.budget ? "warn" : "ok",
      text: `当前微信号近 24 小时列表请求 ${s.uin_calls_24h} / ${cfg.budget}（${Math.round((s.uin_calls_24h / cfg.budget) * 100)}%）${s.uin_calls_24h >= cfg.budget ? "，预算已用完，巡检与历史抓取都暂停，等最早一次请求滑出 24 小时窗口" : "，接近每账号预算。测试中账号在累计约 206–224 次时触发账号级限制（ret=-6），不建议调高预算"}。`,
    });
  }
  const verifyBatches = s.passes.reduce((a, p) => a + p.verify_batches, 0);
  if (verifyBatches > 0) {
    out.push({
      level: "warn",
      text: `${verifyBatches} 个巡检批次接力命中人机验证页。单个验证页多为坏链假阳性（已自动换链接重接力），连续 2 批才判限流；若频繁出现请调大翻页停留。`,
    });
  }
  const envFails = s.runs.filter((r) => r.env_failure).length;
  if (envFails > 0) {
    out.push({
      level: "warn",
      text: `${envFails} 个执行单元发生环境故障（接力期间没有任何文章请求）：多为 RPA 点不开种子 / 文件助手被遮挡 / 代理无流量，与限流无关，请到控制面板自检。`,
    });
  }
  if (t.expired > 0 && t.calls > 0 && t.expired / t.calls > 0.3) {
    out.push({
      level: "warn",
      text: `凭证过期（ret=-3）占请求的 ${Math.round((t.expired / t.calls) * 100)}%：一批的接力 + 采集超出了 key 的 30 分钟有效期，建议减小每批号数。`,
    });
  }
  if (out.length === 0) {
    const theo = Math.floor(3_600_000 / Math.max(1, (cfg.gateMin + Math.max(cfg.gateMin, cfg.gateMax)) / 2));
    out.push({
      level: "ok",
      text:
        t.calls === 0
          ? "窗口内没有列表请求记录。开始巡检 / 抓取历史后这里会按请求逐条统计。"
          : `窗口内无限流、无封禁、无过密请求：节奏正常。当前配置间隔 ${fmtMs(cfg.gateMin)}–${fmtMs(cfg.gateMax)}，理论上限约 ${theo} 次 / 小时；实际峰值 ${s.peak_bucket_calls} 次 / ${fmtDur(s.bucket_secs)}。`,
    });
  }
  return out;
}

function RunFlags({ r }: { r: RunLogRow }) {
  return (
    <span className="inline-flex flex-wrap gap-1">
      {r.blocked && <Badge variant="destructive">封禁</Badge>}
      {r.rate_limited && !r.blocked && <Badge variant="destructive">限流</Badge>}
      {r.verify_hit && <Badge variant="warning">验证页</Badge>}
      {r.env_failure && <Badge variant="warning">环境故障</Badge>}
      {r.truncated && !r.rate_limited && !r.env_failure && <Badge variant="muted">未跑完</Badge>}
      {!r.blocked && !r.rate_limited && !r.verify_hit && !r.env_failure && !r.truncated && (
        <Badge variant="success">干净</Badge>
      )}
    </span>
  );
}

/** 分析页各表每页行数。 */
const TABLE_PAGE_SIZE = 20;

function PassesTable({ passes }: { passes: PassSummary[] }) {
  const pg = usePaged(passes, TABLE_PAGE_SIZE);
  if (passes.length === 0) {
    return <p className="p-4 text-xs text-muted-foreground">窗口内没有巡检轮次记录。</p>;
  }
  return (
    <>
    <Table className="min-w-[960px]">
      <TableHeader>
        <TableRow>
          <TableHead>轮次</TableHead>
          <TableHead>开始</TableHead>
          <TableHead>用时</TableHead>
          <TableHead className="text-right">批次</TableHead>
          <TableHead className="text-right">号数</TableHead>
          <TableHead className="text-right">成功 / 失败</TableHead>
          <TableHead className="text-right">重试 / 未轮到</TableHead>
          <TableHead className="text-right">新文章</TableHead>
          <TableHead className="text-right">列表请求</TableHead>
          <TableHead className="text-right">每号请求</TableHead>
          <TableHead>异常批次</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {pg.slice.map((p) => (
          <TableRow key={p.pass_no}>
            <TableCell className="whitespace-nowrap font-medium">
              第 {p.pass_no} 轮 {p.in_progress && <Badge variant="success">进行中</Badge>}
            </TableCell>
            <TableCell className="whitespace-nowrap tabular-nums">{fmtLogDateTime(p.started_at)}</TableCell>
            <TableCell className="whitespace-nowrap tabular-nums">{fmtDur(p.finished_at - p.started_at)}</TableCell>
            <TableCell className="text-right tabular-nums">{p.batches}</TableCell>
            <TableCell className="text-right tabular-nums">{p.accounts}</TableCell>
            <TableCell className="text-right tabular-nums">
              {p.ok} / <span className={p.failed > 0 ? "text-destructive" : ""}>{p.failed}</span>
            </TableCell>
            <TableCell className="text-right tabular-nums">
              {p.retry} / {p.deferred}
            </TableCell>
            <TableCell className="text-right tabular-nums">{p.new_articles}</TableCell>
            <TableCell className="text-right tabular-nums">{p.list_calls}</TableCell>
            <TableCell className="text-right tabular-nums">
              {p.accounts > 0 ? (p.list_calls / p.accounts).toFixed(1) : "–"}
            </TableCell>
            <TableCell className="space-x-1 whitespace-nowrap">
              {p.blocked_batches > 0 && <Badge variant="destructive">封禁 {p.blocked_batches}</Badge>}
              {p.rate_limited_batches > 0 && <Badge variant="destructive">限流 {p.rate_limited_batches}</Badge>}
              {p.verify_batches > 0 && <Badge variant="warning">验证页 {p.verify_batches}</Badge>}
              {p.env_failures > 0 && <Badge variant="warning">环境故障 {p.env_failures}</Badge>}
              {p.blocked_batches + p.rate_limited_batches + p.verify_batches + p.env_failures === 0 && (
                <Badge variant="success">无</Badge>
              )}
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
    <Pager page={pg.page} pageCount={pg.pageCount} total={pg.total} unit="轮" onPage={pg.setPage} />
    </>
  );
}

function RunsTable({ runs }: { runs: RunLogRow[] }) {
  const pg = usePaged(runs, TABLE_PAGE_SIZE);
  if (runs.length === 0) {
    return <p className="p-4 text-xs text-muted-foreground">窗口内没有执行记录。</p>;
  }
  return (
    <>
    <Table className="min-w-[960px]">
      <TableHeader>
        <TableRow>
          <TableHead>类型</TableHead>
          <TableHead>开始</TableHead>
          <TableHead>用时</TableHead>
          <TableHead className="text-right">号数</TableHead>
          <TableHead className="text-right">成功 / 失败</TableHead>
          <TableHead className="text-right">新文章</TableHead>
          <TableHead className="text-right">列表请求</TableHead>
          <TableHead>结果</TableHead>
          <TableHead>备注</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {pg.slice.map((r) => (
          <TableRow key={r.id}>
            <TableCell className="whitespace-nowrap">
              <Badge variant="muted">
                {r.kind === "sweep" ? `巡检${r.pass_no ? ` #${r.pass_no}` : ""}` : "手动任务（旧）"}
              </Badge>
            </TableCell>
            <TableCell className="whitespace-nowrap tabular-nums">{fmtLogDateTime(r.started_at)}</TableCell>
            <TableCell className="whitespace-nowrap tabular-nums">{fmtDur(r.finished_at - r.started_at)}</TableCell>
            <TableCell className="text-right tabular-nums">{r.accounts}</TableCell>
            <TableCell className="whitespace-nowrap text-right tabular-nums">
              {r.ok} / <span className={r.failed > 0 ? "text-destructive" : ""}>{r.failed}</span>
              {r.kind === "sweep" && (r.retry > 0 || r.deferred > 0) && (
                <span className="text-muted-foreground">（重试 {r.retry}，未轮到 {r.deferred}）</span>
              )}
            </TableCell>
            <TableCell className="text-right tabular-nums">{r.new_articles}</TableCell>
            <TableCell className="text-right tabular-nums">{r.list_calls}</TableCell>
            <TableCell className="whitespace-nowrap">
              <RunFlags r={r} />
            </TableCell>
            <TableCell className="max-w-[360px] truncate text-muted-foreground" title={r.note ?? ""}>
              {r.note ?? ""}
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
    <Pager page={pg.page} pageCount={pg.pageCount} total={pg.total} unit="条" onPage={pg.setPage} />
    </>
  );
}

function CooldownsTable({ rows }: { rows: CooldownLogRow[] }) {
  const pg = usePaged(rows, TABLE_PAGE_SIZE);
  if (rows.length === 0) {
    return <p className="p-4 text-xs text-muted-foreground">窗口内没有触发过整机退避。</p>;
  }
  return (
    <>
    <Table className="min-w-[720px]">
      <TableHeader>
        <TableRow>
          <TableHead>时间</TableHead>
          <TableHead>类型</TableHead>
          <TableHead className="text-right">等级</TableHead>
          <TableHead className="text-right">退避时长</TableHead>
          <TableHead>原因</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {pg.slice.map((c) => (
          <TableRow key={c.id}>
            <TableCell className="whitespace-nowrap tabular-nums">{fmtLogDateTime(c.ts)}</TableCell>
            <TableCell className="whitespace-nowrap">
              {c.kind === "account" ? (
                <Badge variant="destructive">账号级封禁</Badge>
              ) : c.kind === "ip" ? (
                <Badge variant="warning">IP 级限流</Badge>
              ) : (
                <Badge variant="muted">手动解除</Badge>
              )}
            </TableCell>
            <TableCell className="text-right tabular-nums">{c.kind === "clear" ? "–" : c.level}</TableCell>
            <TableCell className="whitespace-nowrap text-right tabular-nums">{c.kind === "clear" ? "–" : fmtDur(c.secs)}</TableCell>
            <TableCell className="max-w-[420px] truncate text-muted-foreground" title={c.reason}>
              {c.reason}
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
    <Pager page={pg.page} pageCount={pg.pageCount} total={pg.total} unit="条" onPage={pg.setPage} />
    </>
  );
}

/**
 * 按影响因素（出口代理 / 微信号）的对照表：一行一组，看限流 / 封禁是不是集中在某个代理或某个号上。
 * 老数据没有因素列的归到「直连 / 未知」。
 */
function FactorTable({
  title,
  description,
  keyLabel,
  rows,
  formatKey,
}: {
  title: string;
  description: string;
  keyLabel: string;
  rows: FactorCounts[];
  formatKey?: (k: string) => string;
}) {
  const pg = usePaged(rows, TABLE_PAGE_SIZE);
  if (rows.length === 0) return null;
  return (
    <Card className="mb-3">
      <CardHeader className="p-4 pb-2">
        <CardTitle className="text-sm">{title}</CardTitle>
        <CardDescription>{description}</CardDescription>
      </CardHeader>
      <CardContent className="p-0 pb-2">
        <Table className="min-w-[760px]">
          <TableHeader>
            <TableRow>
              <TableHead>{keyLabel}</TableHead>
              <TableHead className="text-right">请求</TableHead>
              <TableHead className="text-right">成功率</TableHead>
              <TableHead className="text-right">凭证过期</TableHead>
              <TableHead className="text-right">限流</TableHead>
              <TableHead className="text-right">封禁</TableHead>
              <TableHead className="text-right">其它错误</TableHead>
              <TableHead className="text-right">平均耗时</TableHead>
              <TableHead>首次 / 最近</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {pg.slice.map((x) => {
              const rate = x.calls > 0 ? Math.round((x.ok / x.calls) * 100) : 0;
              return (
                <TableRow key={x.key}>
                  <TableCell className="whitespace-nowrap font-medium">{formatKey ? formatKey(x.key) : x.key}</TableCell>
                  <TableCell className="text-right tabular-nums">{x.calls}</TableCell>
                  <TableCell className={cn("text-right tabular-nums", rate < 60 && "text-destructive")}>{rate}%</TableCell>
                  <TableCell className="text-right tabular-nums">{x.expired}</TableCell>
                  <TableCell className={cn("text-right tabular-nums", x.rate_limited > 0 && "text-destructive")}>{x.rate_limited}</TableCell>
                  <TableCell className={cn("text-right tabular-nums", x.blocked > 0 && "text-destructive")}>{x.blocked}</TableCell>
                  <TableCell className="text-right tabular-nums">{x.error}</TableCell>
                  <TableCell className="text-right tabular-nums">{fmtMs(x.avg_latency_ms)}</TableCell>
                  <TableCell className="whitespace-nowrap text-xs text-muted-foreground">
                    {fmtLogDateTime(x.first_ts)} / {fmtLogDateTime(x.last_ts)}
                  </TableCell>
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
        <Pager page={pg.page} pageCount={pg.pageCount} total={pg.total} unit="组" onPage={pg.setPage} />
      </CardContent>
    </Card>
  );
}

export function RateLimit({ onNav }: { onNav: (v: View) => void }) {
  const { cfg } = useApp();
  const [windowSecs, setWindowSecs] = useState<number>(24 * 3600);
  const q = useQuery({
    queryKey: ["ratelimit-stats", windowSecs],
    queryFn: () => ratelimitStats(windowSecs),
    refetchInterval: 30_000,
  });
  const s = q.data;
  const [exporting, setExporting] = useState(false);
  const onExport = async () => {
    setExporting(true);
    try {
      // 导出固定取最近 30 天（留档保留期），不受页面窗口影响，便于离线整段分析。
      const r = await ratelimitExport(30 * 86_400);
      toast.success("原始数据已导出", {
        description: `${r.path}（请求 ${r.list_calls} 条 / 执行单元 ${r.run_logs} 条 / 退避 ${r.cooldown_logs} 条）`,
        action: { label: "打开所在文件夹", onClick: () => void revealInFolder(r.path) },
        duration: 10_000,
      });
    } catch (e) {
      toast.error("导出失败", { description: String(e) });
    } finally {
      setExporting(false);
    }
  };
  const gateMin = Math.max(0, cfg.list_gap_min_ms);
  const gateMax = Math.max(gateMin, cfg.list_gap_max_ms);
  const findings = useMemo(
    () =>
      s
        ? diagnose(s, {
            gateMin,
            gateMax,
            batchSize: cfg.sweep_batch_size,
            budget: cfg.list_daily_budget,
          })
        : [],
    [s, gateMin, gateMax, cfg.sweep_batch_size, cfg.list_daily_budget],
  );
  const cooling = !!s?.cooldown_now.until && s.cooldown_now.until > s.generated_at;
  const okRate = s && s.totals.calls > 0 ? Math.round((s.totals.ok / s.totals.calls) * 100) : null;
  const tooFast = s ? s.gaps.recent_ms.filter((g) => g < gateMin).length : 0;

  return (
    <div>
      <div className="mb-3 flex flex-wrap items-end justify-between gap-3">
        <div>
          <h1 className="text-xl font-semibold tracking-tight">限流分析</h1>
          <p className="mt-0.5 text-xs text-muted-foreground">
            列表接口（getmsg）的请求节奏、巡检轮次与整机退避——微信按微信号计频，看的是整机节奏
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Select value={String(windowSecs)} onValueChange={(v) => setWindowSecs(Number(v))}>
            <SelectTrigger className="h-8 w-[130px] text-xs">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {WINDOWS.map((w) => (
                <SelectItem key={w.value} value={String(w.value)}>
                  {w.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Button variant="outline" size="sm" loading={q.isFetching} onClick={() => void q.refetch()}>
            {!q.isFetching && <RotateCw />}
            刷新
          </Button>
          <Button
            variant="outline"
            size="sm"
            loading={exporting}
            title="把近 30 天的 list_call_log（含代理 / 微信号 / ret / 间隔等因素列）、run_log、cooldown_log 导出为 JSON，供离线分析"
            onClick={() => void onExport()}
          >
            {!exporting && <Download />}
            导出原始数据
          </Button>
        </div>
      </div>

      {q.isError && (
        <Alert variant="destructive" className="mb-3">
          <TriangleAlert />
          <AlertDescription>读取统计失败：{String(q.error)}</AlertDescription>
        </Alert>
      )}

      {/* 诊断结论：最重要的信息放最上面 */}
      {s && (
        <Card className={cn("mb-3", cooling && "border-destructive/50")}>
          <div className="flex items-start gap-3 p-4">
            <Gauge className={cn("mt-0.5 size-5 shrink-0", cooling ? "text-destructive" : "text-muted-foreground")} />
            <div className="min-w-0 flex-1">
              <div className="flex flex-wrap items-center gap-2 text-sm font-semibold">
                诊断结论
                {cooling ? (
                  <Badge variant="destructive">退避中 · 第 {s.cooldown_now.level} 级</Badge>
                ) : findings.some((f) => f.level === "bad") ? (
                  <Badge variant="destructive">有封禁</Badge>
                ) : findings.some((f) => f.level === "warn") ? (
                  <Badge variant="warning">需关注</Badge>
                ) : (
                  <Badge variant="success">正常</Badge>
                )}
              </div>
              <ul className="mt-1.5 space-y-1 text-xs">
                {findings.map((f, i) => (
                  <li key={i} className="flex gap-2">
                    <span
                      className={cn(
                        "mt-1.5 inline-block size-2 shrink-0 rounded-full",
                        f.level === "bad" && "bg-destructive",
                        f.level === "warn" && "bg-warning",
                        f.level === "ok" && "bg-success",
                      )}
                    />
                    <span className={cn(f.level === "bad" && "text-destructive")}>{f.text}</span>
                  </li>
                ))}
              </ul>
              <p className="mt-2 text-xs text-muted-foreground">
                节流参数在
                <button
                  type="button"
                  onClick={() => onNav("settings")}
                  className="text-link underline underline-offset-2 hover:text-link/80"
                >
                  「系统设置 → 列表采集与节流」
                </button>
                调整；退避可在
                <button
                  type="button"
                  onClick={() => onNav("dashboard")}
                  className="text-link underline underline-offset-2 hover:text-link/80"
                >
                  「控制面板」
                </button>
                手动解除（仅确认是坏链假阳性时）。
              </p>
            </div>
          </div>
        </Card>
      )}

      {/* 关键指标 */}
      <div className="mb-3 grid grid-cols-2 gap-2.5 lg:grid-cols-3 xl:grid-cols-6">
        <Stat
          label="当前号近 24 小时列表请求"
          value={s ? `${s.uin_calls_24h}${cfg.list_daily_budget > 0 ? ` / ${cfg.list_daily_budget}` : ""}` : "–"}
          sub={
            s
              ? `今日 ${s.today_calls} 次 · ${cfg.list_daily_budget > 0 ? "达到预算后巡检与历史抓取都暂停" : "每号预算未设（0 = 不限）"}`
              : undefined
          }
          tone={s && cfg.list_daily_budget > 0 && s.uin_calls_24h >= cfg.list_daily_budget ? "warn" : undefined}
        />
        <Stat
          label="窗口内请求"
          value={s ? String(s.totals.calls) : "–"}
          sub={
            s
              ? `${s.by_source.map((x) => `${SOURCE_LABELS[x.source] ?? x.source} ${x.calls}`).join(" · ") || "无"}`
              : undefined
          }
          badge={okRate !== null ? { text: `成功 ${okRate}%`, variant: okRate >= 90 ? "success" : okRate >= 60 ? "warning" : "destructive" } : undefined}
        />
        <Stat
          label="限流 / 封禁信号"
          value={s ? `${s.totals.rate_limited} / ${s.totals.blocked}` : "–"}
          sub={s ? `触发退避 ${s.cooldown_hits + s.block_hits} 次，累计 ${fmtDur(s.cooldown_total_secs)}` : undefined}
          tone={s && s.totals.blocked > 0 ? "bad" : s && s.totals.rate_limited > 0 ? "warn" : undefined}
        />
        <Stat
          label="请求间隔中位数"
          value={s && s.gaps.samples > 0 ? fmtMs(s.gaps.p50_ms) : "–"}
          sub={`配置闸门 ${fmtMs(gateMin)}–${fmtMs(gateMax)}；最短 ${s && s.gaps.samples > 0 ? fmtMs(s.gaps.min_ms) : "–"}`}
          badge={s && s.gaps.samples > 0 ? (tooFast > 0 ? { text: `过密 ${tooFast} 次`, variant: "destructive" } : { text: "符合闸门", variant: "success" }) : undefined}
        />
        <Stat
          label="请求峰值"
          value={s ? `${s.peak_bucket_calls} 次` : "–"}
          sub={s ? `单个 ${fmtDur(s.bucket_secs)} 桶内最多；往返耗时中位 ${s.latency.p50_ms} ms` : undefined}
        />
        <Stat
          label="执行单元"
          value={s ? `${s.runs_manual + s.runs_sweep}` : "–"}
          sub={s ? `巡检批次 ${s.runs_sweep} · 轮次 ${s.passes.length}${s.runs_manual > 0 ? ` · 旧版手动任务 ${s.runs_manual}` : ""}` : undefined}
        />
      </div>

      {/* 图表：频率时间线 + 间隔分布 */}
      <div className="mb-3 grid grid-cols-1 gap-3 xl:grid-cols-5">
        <Card className="xl:col-span-3">
          <CardHeader className="p-4 pb-2">
            <CardTitle className="text-sm">列表请求频率</CardTitle>
            <CardDescription>
              每 {s ? fmtDur(s.bucket_secs) : "…"} 一桶，按结果堆叠；虚线为按配置间隔推算的单桶理论上限，竖线为退避触发点
            </CardDescription>
          </CardHeader>
          <CardContent className="p-4 pt-1">
            {s ? <FrequencyChart s={s} gateMinMs={gateMin} gateMaxMs={gateMax} /> : <div className="h-[220px]" />}
          </CardContent>
        </Card>
        <Card className="xl:col-span-2">
          <CardHeader className="p-4 pb-2">
            <CardTitle className="text-sm">请求间隔分布</CardTitle>
            <CardDescription>
              相邻两次 getmsg 的间隔（跨号 / 跨页 / 跨来源）；
              {s && s.gaps.samples > 0
                ? `共 ${s.gaps.samples} 个样本，均值 ${fmtMs(s.gaps.avg_ms)}，最长 ${fmtMs(s.gaps.max_ms)}`
                : "暂无样本"}
            </CardDescription>
          </CardHeader>
          <CardContent className="p-4 pt-1">
            {s ? <GapChart s={s} gateMinMs={gateMin} gateMaxMs={gateMax} /> : <div className="h-[180px]" />}
          </CardContent>
        </Card>
      </div>

      {/* 按来源 / 结果的明细表（图表的表格视图） */}
      {s && s.by_source.length > 0 && (
        <Card className="mb-3">
          <CardHeader className="p-4 pb-2">
            <CardTitle className="text-sm">按来源统计</CardTitle>
            <CardDescription>同一微信号下三条路径共用一道闸门与一份每日预算</CardDescription>
          </CardHeader>
          <CardContent className="p-0 pb-2">
            <Table className="min-w-[640px]">
              <TableHeader>
                <TableRow>
                  <TableHead>来源</TableHead>
                  <TableHead className="text-right">请求</TableHead>
                  <TableHead className="text-right">成功</TableHead>
                  <TableHead className="text-right">凭证过期</TableHead>
                  <TableHead className="text-right">限流</TableHead>
                  <TableHead className="text-right">封禁</TableHead>
                  <TableHead className="text-right">其它错误</TableHead>
                  <TableHead className="text-right">解析 / 新增篇数</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {[...s.by_source, { ...s.totals, source: "__total" }].map((x) => (
                  <TableRow key={x.source} className={x.source === "__total" ? "font-medium" : ""}>
                    <TableCell>{x.source === "__total" ? "合计" : (SOURCE_LABELS[x.source] ?? x.source)}</TableCell>
                    <TableCell className="text-right tabular-nums">{x.calls}</TableCell>
                    <TableCell className="text-right tabular-nums">{x.ok}</TableCell>
                    <TableCell className="text-right tabular-nums">{x.expired}</TableCell>
                    <TableCell className={cn("text-right tabular-nums", x.rate_limited > 0 && "text-destructive")}>{x.rate_limited}</TableCell>
                    <TableCell className={cn("text-right tabular-nums", x.blocked > 0 && "text-destructive")}>{x.blocked}</TableCell>
                    <TableCell className="text-right tabular-nums">{x.error}</TableCell>
                    <TableCell className="text-right tabular-nums">
                      {x.articles} / {x.new_articles}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </CardContent>
        </Card>
      )}

      {/* 对照维度：微信号——看封禁是否只跟着某个号 */}
      {s && (
        <FactorTable
          title="按微信号统计"
          description="按凭证 uin 的短哈希分组（不存 uin 原文）；换号登录后会出现新的一行，看封禁是否只跟着某个号"
          keyLabel="微信号"
          rows={s.by_account}
          formatKey={(k) => (k === "未知" ? "未知（老数据）" : `#${k.slice(0, 8)}`)}
        />
      )}

      <Card className="mb-3">
        <CardHeader className="p-4 pb-2">
          <CardTitle className="text-sm">巡检轮次</CardTitle>
          <CardDescription>一轮 = 全库公众号各查一遍；「每号请求」明显大于 1 说明翻页多或凭证过期重采多</CardDescription>
        </CardHeader>
        <CardContent className="p-0 pb-2">{s ? <PassesTable passes={s.passes} /> : null}</CardContent>
      </Card>

      <Card className="mb-3">
        <CardHeader className="p-4 pb-2">
          <CardTitle className="text-sm">最近执行单元</CardTitle>
          <CardDescription>每个巡检批次算一个单元（最近 80 条）；任务执行频率看开始时间的间隔</CardDescription>
        </CardHeader>
        <CardContent className="p-0 pb-2">{s ? <RunsTable runs={s.runs} /> : null}</CardContent>
      </Card>

      <Card className="mb-3">
        <CardHeader className="p-4 pb-2">
          <CardTitle className="text-sm">整机退避记录</CardTitle>
          <CardDescription>IP 级 5 分钟起翻倍封顶 1 小时；账号级 6 小时起翻倍封顶 24 小时；干净跑完一批等级归零</CardDescription>
        </CardHeader>
        <CardContent className="p-0 pb-2">{s ? <CooldownsTable rows={s.cooldowns} /> : null}</CardContent>
      </Card>
    </div>
  );
}
