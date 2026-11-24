import { useState } from "react";
import { useInfiniteQuery } from "@tanstack/react-query";
import { ThumbsUp } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { articleComments, fetchArticleComments, type ArticleComment } from "@/lib/workbench";

const REPLIES_PER_PAGE = 3;

function CommentThread({ comment, depth = 0 }: { comment: ArticleComment; depth?: number }) {
  const [visibleReplies, setVisibleReplies] = useState(REPLIES_PER_PAGE);
  const name = comment.author.trim() || "微信用户";
  const initial = Array.from(name)[0];
  const remaining = comment.replies.length - visibleReplies;
  return <li className={depth ? "py-2.5" : "border-b border-border py-4 last:border-0"}>
    <article className="flex min-w-0 gap-2.5">
      <div aria-hidden="true" className={"grid shrink-0 place-items-center rounded-md bg-accent text-accent-foreground " + (depth ? "size-6 text-[10px]" : "size-8 text-xs")}>{initial}</div>
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-x-2 gap-y-0.5 text-xs text-muted-foreground">
          <span className="font-medium text-foreground break-all">{name}</span>
          {comment.is_author && <span className="rounded bg-primary/15 px-1 text-foreground">作者</span>}
          {comment.created_at != null && comment.created_at > 0 && <time>{new Date(comment.created_at * 1000).toLocaleDateString("zh-CN")}</time>}
          {comment.likes != null && <span className="ml-auto inline-flex items-center gap-1 tabular-nums" aria-label={`获赞 ${comment.likes}`}><ThumbsUp aria-hidden="true" className="size-3" />{comment.likes}</span>}
        </div>
        <p className="mt-1.5 whitespace-pre-wrap break-words text-sm leading-relaxed">
          {comment.reply_to && <span className="text-muted-foreground">回复 {comment.reply_to} </span>}
          {comment.content}
        </p>
      </div>
    </article>
    {comment.replies.length > 0 && <div className={depth < 3 ? "ml-3 mt-2 border-l border-border pl-2.5 sm:ml-5 sm:pl-3.5" : "mt-2 border-t border-border"}>
      <ol aria-label={`${name}的回复`} className="divide-y divide-border/60">
        {comment.replies.slice(0, visibleReplies).map(reply => <CommentThread key={reply.id} comment={reply} depth={depth + 1} />)}
      </ol>
      {remaining > 0 && <button type="button" className="py-1.5 text-xs text-link hover:underline" onClick={() => setVisibleReplies(current => current + REPLIES_PER_PAGE)}>
        加载更多回复 · 还剩 {remaining} 条
      </button>}
    </div>}
  </li>;
}

export function ArticleComments({
  article,
  onClose,
}: {
  article: { id: number; title: string; count: number | null };
  onClose: () => void;
}) {
  const query = useInfiniteQuery({
    queryKey: ["article-comments", article.id],
    initialPageParam: null as number | null,
    queryFn: ({ pageParam }) => articleComments(article.id, pageParam),
    getNextPageParam: page => page.next_cursor ?? undefined,
  });
  const pages = query.data?.pages;
  const newest = pages?.[pages.length - 1];
  const cached = newest?.cached_count ?? 0;
  const reported = newest?.comment_count ?? article.count;
  const comments = query.data?.pages.flatMap(page => page.items) ?? [];
  const incomplete = newest != null && reported != null && cached < reported;
  const [fetchingRemote, setFetchingRemote] = useState(false);
  const [remoteError, setRemoteError] = useState<string | null>(null);
  const canFetchRemote = newest?.can_fetch_more === true;
  async function loadMore() {
    setRemoteError(null);
    if (query.hasNextPage) {
      await query.fetchNextPage();
      return;
    }
    setFetchingRemote(true);
    try {
      await fetchArticleComments(article.id);
      const updated = await query.refetch({ throwOnError: true });
      const fetched = updated.data?.pages;
      if (comments.length > 0 && fetched?.[fetched.length - 1]?.next_cursor != null) {
        await query.fetchNextPage();
      }
    } catch (error) {
      setRemoteError(String(error));
    } finally {
      setFetchingRemote(false);
    }
  }


  return <Dialog open onOpenChange={open => { if (!open) onClose(); }}>
    <DialogContent className="flex max-h-[85vh] max-w-2xl flex-col gap-3">
      <DialogHeader className="shrink-0 pr-4">
        <DialogTitle>文章留言{reported != null && ` · ${reported}`}</DialogTitle>
        <DialogDescription className="line-clamp-2">{article.title}</DialogDescription>
      </DialogHeader>
      <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain pr-1">
        {query.isPending && <p role="status" className="py-8 text-center text-sm text-muted-foreground">正在读取留言…</p>}
        {query.isError && !query.data && <div className="space-y-3 py-8 text-center">
          <p role="alert" className="text-sm text-destructive">留言读取失败。{String(query.error)}</p>
          <Button size="sm" variant="outline" onClick={() => void query.refetch()}>重试</Button>
        </div>}
        {newest && comments.length === 0 && <div className="space-y-3 py-8 text-center">
          <p className="text-sm text-muted-foreground">{reported === 0 ? "这篇文章暂无留言。" : "本机还没有保存这篇文章的留言正文。"}</p>
        </div>}
        {comments.length > 0 && <ol aria-label="文章留言">{comments.map(comment => <CommentThread key={comment.id} comment={comment} />)}</ol>}
        {remoteError && <p role="alert" className="py-2 text-center text-sm text-destructive">{remoteError}</p>}
        {(query.hasNextPage || canFetchRemote) && <div className="py-3 text-center">
          <Button size="sm" variant="outline" disabled={query.isFetchingNextPage || fetchingRemote} onClick={() => void loadMore()}>
            {query.isFetchingNextPage || fetchingRemote ? "正在加载…" : remoteError || query.isFetchNextPageError ? "重试" : comments.length ? "加载更多" : reported === 0 ? "刷新留言" : "获取留言"}
          </Button>
        </div>}
        {comments.length > 0 && !query.hasNextPage && !canFetchRemote && <p role="status" className="py-3 text-center text-xs text-muted-foreground">
          {incomplete ? `已显示本机保存的 ${cached} 条留言，微信显示 ${reported} 条。` : `已显示全部 ${cached} 条已保存留言。`}
        </p>}
      </div>
    </DialogContent>
  </Dialog>;
}
