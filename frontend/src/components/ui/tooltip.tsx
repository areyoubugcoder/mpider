import * as React from "react";
import * as TooltipPrimitive from "@radix-ui/react-tooltip";
import { cn } from "@/lib/utils";

/**
 * 轻量 tooltip / popover 气泡（基于 @radix-ui/react-tooltip，已是依赖，无需额外安装）。
 * 用途：给按钮加**简短解释说明**——悬停或键盘聚焦即弹出，移开即收，不打断操作。
 *
 * 用法（shadcn 风格）：
 * ```tsx
 * <TooltipProvider delayDuration={200}>
 *   <Tooltip>
 *     <TooltipTrigger asChild><Button>框选种子</Button></TooltipTrigger>
 *     <TooltipContent>人工标定种子点位……</TooltipContent>
 *   </Tooltip>
 * </TooltipProvider>
 * ```
 */
export const TooltipProvider = TooltipPrimitive.Provider;
export const Tooltip = TooltipPrimitive.Root;
export const TooltipTrigger = TooltipPrimitive.Trigger;

export const TooltipContent = React.forwardRef<
  React.ElementRef<typeof TooltipPrimitive.Content>,
  React.ComponentPropsWithoutRef<typeof TooltipPrimitive.Content>
>(({ className, sideOffset = 6, ...props }, ref) => (
  <TooltipPrimitive.Portal>
    <TooltipPrimitive.Content
      ref={ref}
      sideOffset={sideOffset}
      className={cn(
        "z-50 max-w-[280px] rounded-md border border-border bg-popover px-3 py-2 text-xs leading-relaxed text-popover-foreground shadow-md",
        "animate-in fade-in-0 zoom-in-95 data-[state=closed]:animate-out data-[state=closed]:fade-out-0",
        className,
      )}
      {...props}
    />
  </TooltipPrimitive.Portal>
));
TooltipContent.displayName = "TooltipContent";
