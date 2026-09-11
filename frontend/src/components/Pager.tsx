import { useEffect, useMemo, useState } from "react";
import { ChevronLeft, ChevronRight } from "lucide-react";
import { Button } from "@/components/ui/button";

/**
 * 列表分页（上一页 / 下一页）——各页表格共用一套控件与文案。
 *
 * - [`Pager`]：分页条。`pageCount <= 1` 时不渲染（一页放得下就不占位）。
 * - [`usePaged`]：前端切片分页（数据已全量在手的表，如限流分析、微信号）。数据行数变了自动回到第 1 页。
 */
export function Pager({
  page,
  pageCount,
  total,
  unit,
  busy,
  onPage,
}: {
  /** 当前页（0 起）。 */
  page: number;
  pageCount: number;
  /** 总条数。 */
  total: number;
  /** 计数单位：条 / 个 / 篇。 */
  unit: string;
  busy?: boolean;
  onPage: (page: number) => void;
}) {
  if (pageCount <= 1) return null;
  return (
    <div className="flex items-center justify-between px-2 py-2">
      <span className="text-xs text-muted-foreground">
        第 {page + 1} / {pageCount} 页 · 共 {total} {unit}
      </span>
      <div className="flex items-center gap-1">
        <Button
          variant="ghost"
          size="sm"
          className="px-2"
          disabled={page === 0 || busy}
          onClick={() => onPage(Math.max(0, page - 1))}
        >
          <ChevronLeft />
          上一页
        </Button>
        <Button
          variant="ghost"
          size="sm"
          className="px-2"
          disabled={page + 1 >= pageCount || busy}
          onClick={() => onPage(page + 1)}
        >
          下一页
          <ChevronRight />
        </Button>
      </div>
    </div>
  );
}

/** 前端切片分页：返回当前页的行与分页状态。 */
export function usePaged<T>(rows: T[], size: number) {
  const [page, setPage] = useState(0);
  const pageCount = Math.max(1, Math.ceil(rows.length / size));
  // 行数变化（换窗口 / 刷新 / 删除）回到第 1 页，避免停在已不存在的页。
  useEffect(() => {
    setPage(0);
  }, [rows.length]);
  const safePage = Math.min(page, pageCount - 1);
  const slice = useMemo(() => rows.slice(safePage * size, safePage * size + size), [rows, safePage, size]);
  return { page: safePage, pageCount, slice, setPage, total: rows.length };
}
