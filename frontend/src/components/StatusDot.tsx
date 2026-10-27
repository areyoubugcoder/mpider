import { cn } from "@/lib/utils";

export type DotTone = "ok" | "warn" | "off" | "err";

/** 状态色走主题 token（明暗各自调校），不再硬编码；不发光。 */
const TONE: Record<DotTone, string> = {
  ok: "bg-success",
  warn: "bg-warning",
  off: "bg-muted-foreground/50",
  err: "bg-destructive",
};

/** 底部状态栏彩色圆点。 */
export function StatusDot({ tone }: { tone: DotTone }) {
  return (
    <i
      className={cn(
        "inline-block size-2 shrink-0 rounded-full",
        TONE[tone],
      )}
    />
  );
}
