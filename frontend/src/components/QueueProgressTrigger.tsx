/** 整块可点击的任务进度；未知总数不显示虚构百分比。 */
import { forwardRef } from "react";
import type { ComponentPropsWithoutRef } from "react";
import type { LucideIcon } from "lucide-react";
import type { QueueProgress } from "@/lib/workbench";
import { cn } from "@/lib/utils";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";

export const QueueProgressTrigger = forwardRef<HTMLButtonElement, ComponentPropsWithoutRef<"button"> & {
  queue: QueueProgress; label: string; hint: string; compact?: boolean; icon?: LucideIcon;
}>(({ queue, label, hint, compact = false, icon: Icon, className, ...props }, ref) => {
  const value = queue.total == null ? `已采集 ${queue.completed} 篇 · ${queue.task_count} 个任务` : `${queue.completed}/${queue.total}`;
  const percent = queue.total ? Math.max(0, Math.min(100, queue.completed / queue.total * 100)) : 0;
  const progressProps = {
    role: "progressbar" as const,
    "aria-label": `${label}进度`,
    "aria-valuemin": 0,
    "aria-valuemax": queue.total == null ? undefined : Math.max(1, queue.total),
    "aria-valuenow": queue.total == null ? undefined : Math.min(queue.completed, queue.total),
    "aria-valuetext": queue.total == null ? `${value}，总篇数待确定` : `已完成 ${value} 篇`,
  };
  return <TooltipProvider delayDuration={200}><Tooltip><TooltipTrigger asChild>
    <button {...props} ref={ref} type="button" aria-label={`${label}：${value}，${hint}`} className={cn("flex shrink-0 cursor-pointer items-center justify-center text-xs hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring", compact ? "size-8 rounded-full" : "min-h-7 min-w-0 max-w-40 gap-1.5 rounded px-1.5 min-[960px]:max-w-52 min-[1280px]:max-w-56", className)}>
      {compact ? <span {...progressProps} className="relative flex size-7 items-center justify-center">
        <svg aria-hidden="true" viewBox="0 0 32 32" className="absolute inset-0 size-7 -rotate-90 fill-none" strokeWidth="3">
          <circle cx="16" cy="16" r="13" className="stroke-muted" />
          <circle cx="16" cy="16" r="13" pathLength="100" strokeLinecap="round" strokeDasharray={`${queue.total == null ? 25 : percent} 100`} className={cn("stroke-primary", queue.total == null && "animate-pulse motion-reduce:animate-none")} />
        </svg>
        {Icon && <Icon aria-hidden="true" className="size-3" />}
      </span> : <>
      <span className="min-w-0 truncate whitespace-nowrap tabular-nums">{label}: {value}</span>
      <span {...progressProps} className="hidden h-1.5 w-10 shrink-0 overflow-hidden rounded-full bg-muted min-[960px]:inline-block min-[1280px]:w-14">
        <span className={`block h-full bg-primary ${queue.total == null ? "w-1/3 animate-pulse motion-reduce:animate-none" : ""}`} style={queue.total == null ? undefined : {width:`${percent}%`}} />
      </span>
      </>}
    </button>
  </TooltipTrigger><TooltipContent side="top" className="max-w-sm break-words"><p>{label}：{value}{queue.total != null && ` · ${queue.task_count} 个任务`}</p><p>{hint}</p></TooltipContent></Tooltip></TooltipProvider>;
});
QueueProgressTrigger.displayName = "QueueProgressTrigger";
