import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import {
  Check,
  Copy,
  DownloadCloud,
  ExternalLink,
  Eye,
  Link2,
  RefreshCw,
  Trash2,
  X,
} from "lucide-react";
import { toast } from "sonner";
import { Pager } from "@/components/Pager";
import { useApp } from "@/store";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { DetailProgressBar } from "@/components/DetailProgressBar";
import {
  deleteArticle,
  getArticleDetail,
  listAccounts,
  listArticles,
  openExternal,
  type ArticleDetail,
  type ArticleItem,
} from "@/lib/tauri";
import { msg } from "@/lib/utils";

function fmtTime(epoch: number | null): string {
  return epoch ? new Date(epoch * 1000).toLocaleString() : "—";
}

/** 用系统默认浏览器打开链接；失败（如没有默认浏览器 / 命令被拒）toast 提示而不是静默。 */
function openLink(url: string) {
  void openExternal(url).catch((e) => toast.error("打开链接失败", { description: msg(e) }));
}

/** 复制到剪贴板；成功回调用于行内「已复制」反馈，失败 toast 提示。 */
function copyText(text: string, onDone: () => void) {
  void navigator.clipboard
    .writeText(text)
    .then(onDone)
    .catch((e) => toast.error("复制失败", { description: msg(e) }));
}

/** 全部公众号选项的哨兵值（radix Select 不允许空字符串 value）。 */
const ALL = "__all__";

/** 每页条数。 */
const PAGE_SIZE = 50;

/** 正文 Markdown 渲染（react-markdown + GFM）：
 *  - 链接一律经系统默认浏览器打开（WebView 内 target=_blank 无效）；
 *  - 微信图床图片需去 Referer 才能加载。 */
function ArticleMarkdown({ md }: { md: string }) {
  return (
    <div className="prose prose-sm dark:prose-invert max-w-none prose-img:mx-auto prose-img:rounded-lg">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          a: ({ href, children }) => (
            <a
              href={href}
              onClick={(e) => {
                e.preventDefault();
                if (href) openLink(href);
              }}
            >
              {children}
            </a>
          ),
          img: ({ src, alt }) => (
            <img src={src} alt={alt ?? ""} referrerPolicy="no-referrer" loading="lazy" />
          ),
        }}
      >
        {md}
      </ReactMarkdown>
    </div>
  );
}

/** Markdown 阅读弹窗：渲染呈现正文 + 一键复制原文 + 跳原文。 */
function DetailDialog({
  detail,
  onClose,
}: {
  detail: ArticleDetail | null;
  onClose: () => void;
}) {
  const [copied, setCopied] = useState(false);
  const [linkCopied, setLinkCopied] = useState(false);
  const md = detail?.content_md ?? "(无正文)";
  return (
    <Dialog open={detail !== null} onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="flex max-h-[85vh] max-w-3xl flex-col">
        <DialogHeader className="pr-8">
          <DialogTitle className="text-sm leading-snug">
            {detail?.title ?? "(无标题)"}
          </DialogTitle>
          <DialogDescription className="flex flex-wrap items-center gap-x-2 text-xs">
            <span>{detail?.author ?? "—"}</span>
            <span>·</span>
            <span>{fmtTime(detail?.published_at ?? null)}</span>
            {detail?.content_url && (
              <button
                type="button"
                onClick={() => openLink(detail.content_url!)}
                className="inline-flex items-center gap-1 text-link hover:underline"
              >
                原文 <ExternalLink className="size-3" />
              </button>
            )}
          </DialogDescription>
        </DialogHeader>
        <div className="flex items-center justify-end gap-2">
          {detail?.content_url && (
            <Button
              variant="outline"
              size="sm"
              onClick={() =>
                copyText(detail.content_url!, () => {
                  setLinkCopied(true);
                  setTimeout(() => setLinkCopied(false), 1500);
                })
              }
            >
              {linkCopied ? <Check /> : <Link2 />}
              {linkCopied ? "已复制" : "复制链接"}
            </Button>
          )}
          <Button
            variant="outline"
            size="sm"
            onClick={() =>
              copyText(md, () => {
                setCopied(true);
                setTimeout(() => setCopied(false), 1500);
              })
            }
          >
            <Copy />
            {copied ? "已复制" : "复制 Markdown"}
          </Button>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto rounded-lg bg-muted/40 p-5">
          <ArticleMarkdown md={md} />
        </div>
      </DialogContent>
    </Dialog>
  );
}

export function Articles() {
  const { articlesBiz, setArticlesBiz, runDetail, detailRunning, detailError, refreshCounts } =
    useApp();
  const qc = useQueryClient();
  const [detail, setDetail] = useState<ArticleDetail | null>(null);
  // 待确认物理删除的文章（null = 关闭确认框）。
  const [pendingDelete, setPendingDelete] = useState<ArticleItem | null>(null);
  // 行内「复制链接」的已复制反馈（记录行 id，1.5s 后消失）。
  const [copiedId, setCopiedId] = useState<number | null>(null);
  const copyLink = (id: number, url: string) =>
    copyText(url, () => {
      setCopiedId(id);
      setTimeout(() => setCopiedId((cur) => (cur === id ? null : cur)), 1500);
    });

  // 翻页：切换公众号过滤时回到第一页。
  const [page, setPage] = useState(0);
  const changeBiz = (biz: string | null) => {
    setArticlesBiz(biz);
    setPage(0);
  };

  const accounts = useQuery({ queryKey: ["accounts"], queryFn: () => listAccounts() });
  const articles = useQuery({
    queryKey: ["articles", articlesBiz, page],
    queryFn: () => listArticles(articlesBiz ?? undefined, PAGE_SIZE, page * PAGE_SIZE),
  });

  const invalidateLists = () => {
    void qc.invalidateQueries({ queryKey: ["articles"] });
    void qc.invalidateQueries({ queryKey: ["accounts"] });
  };

  /** 批量补详情（全部 / 当前过滤的号）。 */
  const fetchBatch = useMutation({
    mutationFn: () => runDetail(articlesBiz ?? undefined),
    onSettled: invalidateLists,
  });

  /** 单篇补详情。 */
  const fetchOne = useMutation({
    mutationFn: (id: number) => runDetail(undefined, id),
    onSettled: invalidateLists,
  });

  /** 查看已采详情（读库 + 打开弹窗）。 */
  const viewDetail = useMutation({
    mutationFn: (id: number) => getArticleDetail(id),
    onSuccess: (d) => setDetail(d),
    onError: (e) => toast.error("读取详情失败", { description: msg(e) }),
  });

  /** 物理删除单篇（确认后执行；行彻底移除，不可恢复）。失败留在确认框里显示，便于重试。 */
  const removeArticle = useMutation({
    mutationFn: (id: number) => deleteArticle(id),
    onSuccess: () => {
      toast.success(`已删除文章「${pendingDelete?.title ?? "(无标题)"}」`, {
        description: "物理删除，不可恢复",
      });
      setPendingDelete(null);
      invalidateLists();
      void refreshCounts();
    },
    onError: (e) => toast.error("删除文章失败", { description: msg(e) }),
  });

  const rows = articles.data?.items ?? [];
  const total = articles.data?.total ?? 0;
  const pageCount = Math.max(1, Math.ceil(total / PAGE_SIZE));
  const doneCount = rows.filter((r) => r.detail_done === 1).length;

  return (
    <div>
      <div className="mb-3">
        <h1 className="text-xl font-semibold tracking-tight">公众号文章</h1>
        <p className="mt-0.5 text-xs text-muted-foreground">
          已采集的文章列表，查看或补抓 Markdown 正文
        </p>
      </div>

      {/* 补采进度（运行时显示） */}
      <DetailProgressBar className="mb-3" />
      {detailError && (
        <div className="mb-3 text-xs text-destructive">{detailError}</div>
      )}

      <Card>
        <CardHeader className="flex-row flex-wrap items-center justify-between gap-2 space-y-0 p-4 pb-2">
          <div>
            <CardTitle className="text-sm">文章</CardTitle>
            <CardDescription className="mt-1">
              {articles.isSuccess
                ? `共 ${total} 篇（本页已采详情 ${doneCount} 篇）`
                : "加载中…"}
            </CardDescription>
          </div>
          <div className="flex items-center gap-2">
            {/* 公众号过滤：Radix Select 原样使用；「清除」是触发器右侧的独立按钮，不塞进触发器里 */}
            <div className="flex items-center gap-1">
              <Select
                value={articlesBiz ?? ALL}
                onValueChange={(v) => changeBiz(v === ALL ? null : v)}
              >
                <SelectTrigger
                  className={"h-8 w-44 text-xs" + (articlesBiz ? " font-semibold" : "")}
                >
                  <SelectValue placeholder="全部公众号" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value={ALL}>全部公众号</SelectItem>
                  {(accounts.data?.items ?? []).map((a) => (
                    <SelectItem key={a.biz} value={a.biz}>
                      {a.nickname ?? a.biz}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {articlesBiz && (
                <Button
                  variant="ghost"
                  size="sm"
                  className="px-2 text-muted-foreground"
                  title="清除过滤"
                  onClick={() => changeBiz(null)}
                >
                  <X />
                </Button>
              )}
            </div>
            <Button
              variant="outline"
              size="sm"
              disabled={detailRunning && !fetchBatch.isPending}
              loading={fetchBatch.isPending}
              onClick={() => fetchBatch.mutate()}
            >
              {!fetchBatch.isPending && <DownloadCloud />}
              {articlesBiz ? "抓取本号详情" : "抓取全部详情"}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              loading={articles.isFetching}
              onClick={() => void articles.refetch()}
            >
              {!articles.isFetching && <RefreshCw />}
              刷新
            </Button>
          </div>
        </CardHeader>
        <CardContent className="p-2 pt-0">
          {articles.isError ? (
            <div className="p-4 text-sm text-destructive">
              list_articles 失败：{msg(articles.error)}
            </div>
          ) : articles.isSuccess && rows.length === 0 ? (
            <div className="p-4 text-sm text-muted-foreground">
              {articlesBiz
                ? "该公众号还没有文章。"
                : "还没有文章。先在控制面板「运行一次」采集列表。"}
            </div>
          ) : (
            <Table className="min-w-[760px]">
              <TableHeader>
                <TableRow>
                  <TableHead>标题</TableHead>
                  <TableHead className="w-36">所属公众号</TableHead>
                  <TableHead className="w-28">作者</TableHead>
                  <TableHead className="w-40">发布时间</TableHead>
                  <TableHead className="w-24">操作</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {articles.isPending ? (
                  <TableRow>
                    <TableCell colSpan={5} className="text-muted-foreground">
                      加载中…
                    </TableCell>
                  </TableRow>
                ) : (
                  rows.map((a) => (
                    <TableRow key={a.id}>
                      <TableCell className="max-w-0">
                        <div className="flex items-center gap-2">
                          {a.content_url ? (
                            <button
                              type="button"
                              onClick={() => openLink(a.content_url!)}
                              title={`${a.title ?? ""}（在浏览器打开原文）`}
                              className="group/title flex min-w-0 items-center gap-1 text-left font-medium hover:text-link hover:underline"
                            >
                              <span className="truncate">
                                {a.title ?? (
                                  <span className="text-muted-foreground">(无标题)</span>
                                )}
                              </span>
                              <ExternalLink className="size-3 shrink-0 opacity-0 transition-opacity group-hover/title:opacity-60" />
                            </button>
                          ) : (
                            <span className="truncate font-medium" title={a.title ?? ""}>
                              {a.title ?? (
                                <span className="text-muted-foreground">(无标题)</span>
                              )}
                            </span>
                          )}
                          {a.detail_done === 1 && (
                            <Badge variant="success" className="shrink-0">
                              已采
                            </Badge>
                          )}
                          {a.detail_done === -1 && (
                            <Badge
                              variant="muted"
                              className="shrink-0"
                              title={a.detail_error ?? "微信提示该内容无法查看"}
                            >
                              不可用
                            </Badge>
                          )}
                        </div>
                      </TableCell>
                      <TableCell>
                        <span>
                          {a.account ?? (
                            <code className="rounded bg-muted px-1 py-0.5 text-xs">
                              {a.biz}
                            </code>
                          )}
                        </span>
                      </TableCell>
                      <TableCell>{a.author ?? "—"}</TableCell>
                      <TableCell className="text-xs text-muted-foreground">
                        {fmtTime(a.published_at)}
                      </TableCell>
                      <TableCell>
                        <div className="flex items-center gap-0.5">
                          {a.detail_done === 1 ? (
                            <Button
                              variant="ghost"
                              size="sm"
                              className="px-2 text-link"
                              title="查看详情（Markdown）"
                              loading={viewDetail.isPending && viewDetail.variables === a.id}
                              onClick={() => viewDetail.mutate(a.id)}
                            >
                              {!(viewDetail.isPending && viewDetail.variables === a.id) && (
                                <Eye />
                              )}
                            </Button>
                          ) : a.detail_done === 0 ? (
                            <Button
                              variant="ghost"
                              size="sm"
                              className="px-2 text-link"
                              disabled={
                                (detailRunning && !fetchOne.isPending) || !a.content_url
                              }
                              loading={fetchOne.isPending && fetchOne.variables === a.id}
                              title={
                                a.content_url
                                  ? a.detail_error
                                    ? `抓取详情（上次失败：${a.detail_error}）`
                                    : "抓取详情（Markdown 正文）"
                                  : "该文章没有链接，无法补详情"
                              }
                              onClick={() => fetchOne.mutate(a.id)}
                            >
                              {!(fetchOne.isPending && fetchOne.variables === a.id) && (
                                <DownloadCloud />
                              )}
                            </Button>
                          ) : null /* 不可用（-1）：不提供抓取入口 */}
                          {a.content_url && (
                            <Button
                              variant="ghost"
                              size="sm"
                              className="px-2"
                              title="复制原文链接"
                              onClick={() => copyLink(a.id, a.content_url!)}
                            >
                              {copiedId === a.id ? (
                                <Check className="text-success" />
                              ) : (
                                <Link2 />
                              )}
                            </Button>
                          )}
                          <Button
                            variant="ghost"
                            size="sm"
                            className="px-2 text-muted-foreground hover:text-destructive"
                            title="删除该文章（物理删除，不可恢复）"
                            onClick={() => setPendingDelete(a)}
                          >
                            <Trash2 />
                          </Button>
                        </div>
                      </TableCell>
                    </TableRow>
                  ))
                )}
              </TableBody>
            </Table>
          )}
          {articles.isSuccess && (
            <Pager page={page} pageCount={pageCount} total={total} unit="篇" busy={articles.isFetching} onPage={setPage} />
          )}
        </CardContent>
      </Card>

      <DetailDialog detail={detail} onClose={() => setDetail(null)} />

      {/* 物理删除确认：与公众号软删除不同，这是彻底移除，不可恢复 */}
      <Dialog
        open={pendingDelete !== null}
        onOpenChange={(open) => !open && setPendingDelete(null)}
      >
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle className="text-sm">
              删除文章「{pendingDelete?.title ?? "(无标题)"}」？
            </DialogTitle>
            <DialogDescription className="leading-relaxed">
              这是<b className="text-foreground">物理删除</b>——文章及其评论将从数据库中彻底移除，
              <b className="text-foreground">不可恢复</b>。之后若重新采集到这篇文章，会当作新文章再次入库。
            </DialogDescription>
          </DialogHeader>
          {removeArticle.isError && (
            <div className="text-xs text-destructive">
              删除失败：{msg(removeArticle.error)}
            </div>
          )}
          <DialogFooter>
            <Button variant="outline" size="sm" onClick={() => setPendingDelete(null)}>
              取消
            </Button>
            <Button
              variant="destructive"
              size="sm"
              loading={removeArticle.isPending}
              onClick={() => {
                if (pendingDelete) removeArticle.mutate(pendingDelete.id);
              }}
            >
              {!removeArticle.isPending && <Trash2 />}
              确认删除
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
