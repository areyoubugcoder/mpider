import * as React from "react";
import { ChevronDown } from "lucide-react";
import { cn } from "@/lib/utils";
import { Card, CardDescription } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";

/**
 * 可折叠区块（不依赖 @radix-ui/react-collapsible，自己用 state 实现）。
 *
 * - 标题行整行可点；右侧 chevron 旋转动画；
 * - 收起时标题行右侧显示 `summary`（关键值摘要，灰色小字）；展开时标题下显示 `description`；
 * - `dirty` 为真时标题旁加「已修改」徽章（表单区块用来标示有未保存的改动）；
 * - 受控（`open` + `onOpenChange`）/ 非受控（`defaultOpen`）皆可。
 *
 * 展开状态的持久化见下方 `useSectionOpenState`。
 */
export interface CollapsibleSectionProps {
  /** 标题。 */
  title: React.ReactNode;
  /** 一句说明：展开时显示在标题下方。 */
  description?: React.ReactNode;
  /** 标题旁徽章（始终显示，如「已安装 ✓」）。 */
  badge?: React.ReactNode;
  /** 收起时标题行右侧的关键值摘要。 */
  summary?: React.ReactNode;
  /** 有未保存的修改：标题旁显示「已修改」徽章。 */
  dirty?: boolean;
  /** 标题左侧图标。 */
  icon?: React.ReactNode;
  /** 受控：是否展开。 */
  open?: boolean;
  /** 非受控：初始是否展开（默认收起）。 */
  defaultOpen?: boolean;
  onOpenChange?: (open: boolean) => void;
  className?: string;
  /** 内容区额外样式（默认 `p-4 pt-0`）。 */
  contentClassName?: string;
  children: React.ReactNode;
}

export function CollapsibleSection({
  title,
  description,
  badge,
  summary,
  dirty = false,
  icon,
  open: openProp,
  defaultOpen = false,
  onOpenChange,
  className,
  contentClassName,
  children,
}: CollapsibleSectionProps) {
  const [uncontrolled, setUncontrolled] = React.useState(defaultOpen);
  const controlled = openProp !== undefined;
  const open = controlled ? openProp : uncontrolled;

  const toggle = () => {
    const next = !open;
    if (!controlled) setUncontrolled(next);
    onOpenChange?.(next);
  };

  return (
    <Card className={cn(dirty && "ring-1 ring-warning/40", className)}>
      <button
        type="button"
        onClick={toggle}
        aria-expanded={open}
        className={cn(
          "flex w-full items-start gap-2 rounded-xl p-4 text-left transition-colors hover:bg-muted/40",
          "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background",
          open && "pb-2",
        )}
      >
        {icon && (
          <span className="mt-0.5 shrink-0 text-foreground [&_svg]:size-4">{icon}</span>
        )}
        <span className="min-w-0 flex-1">
          <span className="flex flex-wrap items-center gap-2">
            <span className="text-sm font-semibold leading-tight">{title}</span>
            {badge}
            {dirty && <Badge variant="warning">已修改</Badge>}
          </span>
          {open && description && (
            <CardDescription className="mt-1 leading-relaxed">
              {description}
            </CardDescription>
          )}
        </span>
        {!open && summary && (
          <span className="hidden max-w-[50%] shrink truncate pt-0.5 text-right text-xs text-muted-foreground sm:block">
            {summary}
          </span>
        )}
        <ChevronDown
          className={cn(
            "mt-0.5 size-4 shrink-0 text-muted-foreground transition-transform duration-200",
            open && "rotate-180",
          )}
        />
      </button>
      {open && <div className={cn("p-4 pt-0", contentClassName)}>{children}</div>}
    </Card>
  );
}

/**
 * 一组区块的展开状态，持久化到 `localStorage[storageKey]`（JSON 对象 `{id: bool}`）。
 *
 * `defaults[id]`：该区块没有记住的状态时的默认值；`null` 表示由调用方在 `isOpen(id, fallback)` 里
 * 按运行时条件决定（如证书区块：已安装收起、未安装展开）。
 */
export function useSectionOpenState<Id extends string>(
  storageKey: string,
  defaults: Record<Id, boolean | null>,
) {
  const [state, setState] = React.useState<Partial<Record<Id, boolean>>>(() => {
    try {
      const raw = localStorage.getItem(storageKey);
      if (raw) {
        const parsed = JSON.parse(raw) as Record<string, unknown>;
        const out: Partial<Record<Id, boolean>> = {};
        for (const id of Object.keys(defaults) as Id[]) {
          if (typeof parsed[id] === "boolean") out[id] = parsed[id] as boolean;
        }
        return out;
      }
    } catch {
      /* 读不到 / 解析失败就用默认 */
    }
    return {};
  });

  React.useEffect(() => {
    try {
      localStorage.setItem(storageKey, JSON.stringify(state));
    } catch {
      /* 写不进（隐私模式等）不影响使用 */
    }
  }, [state, storageKey]);

  const isOpen = React.useCallback(
    (id: Id, fallback = false): boolean => state[id] ?? defaults[id] ?? fallback,
    [state, defaults],
  );
  const setOpen = React.useCallback(
    (id: Id, open: boolean) => setState((s) => ({ ...s, [id]: open })),
    [],
  );
  const setAll = React.useCallback(
    (open: boolean) => {
      const next: Partial<Record<Id, boolean>> = {};
      for (const id of Object.keys(defaults) as Id[]) next[id] = open;
      setState(next);
    },
    [defaults],
  );

  return { isOpen, setOpen, setAll };
}
