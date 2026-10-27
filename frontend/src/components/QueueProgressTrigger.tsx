/** 整块可点击的任务进度；未知总数不显示虚构百分比。 */
import { forwardRef } from "react";
import type { ComponentPropsWithoutRef } from "react";
import type { QueueProgress } from "@/lib/workbench";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";

export const QueueProgressTrigger = forwardRef<HTMLButtonElement, ComponentPropsWithoutRef<"button"> & {
  queue: QueueProgress; label: string; hint: string;
}>(({ queue, label, hint, ...props }, ref) => {
  const value = queue.total == null ? `已采集 ${queue.completed} 篇 · ${queue.task_count} 个任务` : `${queue.completed}/${queue.total}`;
  return <TooltipProvider delayDuration={200}><Tooltip><TooltipTrigger asChild>
    <button {...props} ref={ref} type="button" aria-label={`${label}：${value}，${hint}`} className="flex min-h-7 min-w-0 max-w-40 cursor-pointer items-center gap-1.5 rounded px-1.5 text-xs hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring min-[960px]:max-w-52 min-[1280px]:max-w-56">
      <span className="min-w-0 truncate whitespace-nowrap tabular-nums">{label}: {value}</span>
      <span className="hidden h-1.5 w-10 shrink-0 overflow-hidden rounded-full bg-muted min-[960px]:inline-block min-[1280px]:w-14" role="progressbar" aria-label={`${label}进度`} aria-valuemin={0} aria-valuemax={queue.total == null ? undefined : Math.max(1,queue.total)} aria-valuenow={queue.total == null ? undefined : queue.completed} aria-valuetext={queue.total == null ? `${value}，总篇数待确定` : `已完成 ${value} 篇`}>
        <span className={`block h-full bg-primary ${queue.total == null ? "w-1/3 animate-pulse motion-reduce:animate-none" : ""}`} style={queue.total == null ? undefined : {width:`${queue.total ? Math.min(100,queue.completed/queue.total*100):0}%`}} />
      </span>
    </button>
  </TooltipTrigger><TooltipContent className="max-w-sm break-words"><p>{label}：{value}</p><p>{hint}</p></TooltipContent></Tooltip></TooltipProvider>;
});
QueueProgressTrigger.displayName = "QueueProgressTrigger";
