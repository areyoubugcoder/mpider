import { fmtLogTime, type HistoryJobView, type HistoryTarget } from "@/lib/tauri";

/**
 * 历史文章抓取的展示辅助（纯函数，状态栏与公众号列表共用，保证两处文案一致）。
 */

/** 秒数 → 「N 秒 / N 分 / N 小时 M 分」。 */
export function fmtSecs(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  if (s < 60) return `${s} 秒`;
  if (s < 3600) return `${Math.floor(s / 60)} 分`;
  const m = Math.floor((s % 3600) / 60);
  return m ? `${Math.floor(s / 3600)} 小时 ${m} 分` : `${Math.floor(s / 3600)} 小时`;
}

/** 该任务是否还「活着」（运行中或暂停，占着唯一名额）。 */
export function isActiveJob(job: HistoryJobView | null | undefined): job is HistoryJobView {
  return !!job && (job.status === "running" || job.status === "paused");
}

/** 一行状态文案：状态栏与列表共用。 */
export function historyLine(job: HistoryJobView, now: number): string {
  switch (job.status) {
    case "running": {
      const next = job.next_page_at ?? 0;
      const wait = next - now;
      const tail =
        wait > 1 ? `下一页 ${Math.ceil(wait)} 秒后` : job.pages === 0 ? "准备中" : "正在请求";
      return `第 ${job.pages} 页 · 已抓 ${job.matched} 篇 · ${tail}`;
    }
    case "paused":
      switch (job.paused_reason) {
        case "budget":
          return job.resume_at
            ? `预算暂停，${fmtLogTime(job.resume_at)} 恢复`
            : "预算暂停，额度腾出后继续";
        case "blocked":
          return "微信号受限，暂停";
        case "credential":
          return "等待凭证续期";
        case "cooldown":
          return job.resume_at ? `退避中，${fmtLogTime(job.resume_at)} 恢复` : "退避中，暂停";
        case "restart":
          return `应用重启后暂停 · 已抓 ${job.matched} 篇 · 点「继续」恢复`;
        default:
          return `已暂停 · 第 ${job.pages} 页 · 已抓 ${job.matched} 篇`;
      }
    case "done":
      return `已完成 ${job.matched} 篇`;
    case "cancelled":
      return `已取消 · 抓到 ${job.matched} 篇`;
    case "failed":
      return `失败：${job.last_error ?? "未知原因"}`;
  }
}

/** 进度百分比：目标有条数时按 matched/count，否则 null（不定进度）。 */
export function historyPercent(job: HistoryJobView): number | null {
  const c = job.target.count;
  if (!c || c <= 0) return null;
  return Math.min(100, Math.round((job.matched / c) * 100));
}

/** 目标摘要：「500 篇 · 2026-01-01 起 · 至 2026-06-30」，全空为「全部（翻到底）」。 */
export function targetSummary(t: HistoryTarget): string {
  const parts: string[] = [];
  if (t.count) parts.push(`${t.count} 篇`);
  if (t.since_ts) parts.push(`${dateOf(t.since_ts)} 起`);
  if (t.until_ts) parts.push(`至 ${dateOf(t.until_ts)}`);
  return parts.length ? parts.join(" · ") : "全部（翻到底）";
}

function dateOf(ts: number): string {
  const d = new Date(ts * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

/** `<input type="date">` 的 `YYYY-MM-DD` → 秒级时间戳：起始取当天 00:00:00，截止取 23:59:59（本地时区）。 */
export function dateToTs(v: string, endOfDay: boolean): number | null {
  if (!v) return null;
  const [y, m, d] = v.split("-").map(Number);
  if (!y || !m || !d) return null;
  const dt = endOfDay ? new Date(y, m - 1, d, 23, 59, 59) : new Date(y, m - 1, d, 0, 0, 0);
  return Math.floor(dt.getTime() / 1000);
}
