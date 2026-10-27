import { useState, type ReactNode } from "react";
import { CircleHelp } from "lucide-react";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

/**
 * 说明气泡：一个小问号图标，悬停 / 聚焦 / 点击弹出详细说明——把页面上的长解释性文案收进去，
 * 正文只留一句短话。内容区可悬停（Radix 默认），里面放跳转链接也能点到。
 */
export function InfoTip({
  children,
  className,
  side = "bottom",
}: {
  children: ReactNode;
  className?: string;
  side?: "top" | "bottom" | "left" | "right";
}) {
  // 点击也能开关（触屏 / 不想悬停时），悬停仍由 Radix 自己管。
  const [open, setOpen] = useState<boolean | undefined>(undefined);
  return (
    <TooltipProvider delayDuration={150}>
      <Tooltip open={open} onOpenChange={setOpen}>
        <TooltipTrigger asChild>
          <button
            type="button"
            aria-label="查看说明"
            onClick={() => setOpen((o) => !o)}
            className={cn(
              "inline-flex shrink-0 items-center align-middle text-muted-foreground hover:text-foreground focus-visible:outline-none",
              className,
            )}
          >
            <CircleHelp className="size-3.5" />
          </button>
        </TooltipTrigger>
        <TooltipContent side={side} className="max-w-[360px] text-xs">
          {children}
        </TooltipContent>
      </Tooltip>
    </TooltipProvider>
  );
}
