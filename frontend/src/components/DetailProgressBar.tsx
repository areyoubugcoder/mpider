import { useApp } from "@/store";
import { Progress } from "@/components/ui/progress";
import { cn } from "@/lib/utils";

/**
 * 补采文章详情的全局进度条：任务运行中（或刚结束还有进度可看时）显示
 * 「n/total + 成功/失败计数 + 最近一行进度」。挂在控制面板与「公众号文章」页，
 * 读同一份 store.detailProgress —— 无论从哪个入口触发，两处都能看到进度。
 */
export function DetailProgressBar({ className }: { className?: string }) {
  const { detailRunning, detailProgress } = useApp();
  if (!detailRunning && !detailProgress) return null;
  const p = detailProgress;
  const pct = p && p.total > 0 ? Math.round((p.completed / p.total) * 100) : 0;
  return (
    <div className={cn("rounded-lg bg-card p-3 shadow-sm", className)}>
      <div className="mb-2 flex items-center justify-between text-xs">
        <span className="font-medium">
          {detailRunning ? "正在抓取文章详情…" : "详情抓取已结束"}
        </span>
        <span className="text-muted-foreground">
          {p ? `${p.completed}/${p.total} · 成功 ${p.done} · 失败 ${p.failed}` : "准备中…"}
        </span>
      </div>
      <Progress value={pct} className={cn("h-1.5", detailRunning && "animate-pulse")} />
      {p?.line && (
        <div className="mt-2 truncate font-mono text-[11px] text-muted-foreground" title={p.line}>
          {p.line}
        </div>
      )}
    </div>
  );
}
