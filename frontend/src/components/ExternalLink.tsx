/** 公开链接统一交给系统浏览器；网页预览沿用原生链接行为。 */
import type { ComponentProps } from "react";
import { isTauri } from "@tauri-apps/api/core";
import { toast } from "sonner";
import { openExternal } from "@/lib/tauri";
import { cn } from "@/lib/utils";

export function ExternalLink({ className, onClick, ...props }: ComponentProps<"a"> & { href: string }) {
  return <a {...props} target="_blank" rel="noopener noreferrer"
    className={cn("rounded-sm text-link hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background", className)}
    onClick={event => {
      onClick?.(event);
      if (event.defaultPrevented || !isTauri()) return;
      event.preventDefault();
      void openExternal(props.href).catch(() => toast.error("无法打开浏览器，请复制链接后手动打开。", { description: props.href, duration: 10000 }));
    }} />;
}
