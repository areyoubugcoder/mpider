import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Check, ChevronDown, Copy, Link2, RefreshCw } from "lucide-react";
import { toast } from "sonner";
import {
  fmtLogDateTime,
  seedBootstrapUrl,
  seedServerApply,
  seedServerStatus,
} from "@/lib/tauri";
import { useApp } from "@/store";
import { cn, msg } from "@/lib/utils";
import { Card, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";

/**
 * 种子链接「首次设置」卡（可折叠）。
 *
 * RPA 不再自动往文件助手输入框敲键发送种子（那步最脆弱）；改为用户**首次**手动把这条
 * 固定入口链接复制、粘贴进微信「文件传输助手」并发送一次，再在自检卡「框选种子」标定它的位置。
 * 此后 RPA 只需按标定位置点击它拉起内置浏览器。链接指向 App 自己起的**本机种子入口服务**
 * （`http://<seed_host>:<seed_port>/`，应用启动即常驻），任务运行时页面自带跳转脚本、自动跳到本批真正要采的
 * 任务链接；没任务时只给一行说明页，随时可以点开验证链接没坏。
 *
 * 默认展开；标题行可点击收起。副标题按自检 `mark` 提示首次设置是否已完成。卡内有一行服务状态
 * （在跑 / 起不来的原因 + 最近一次访问），起不来可点「重试」。
 */
export function SeedSetup() {
  const { rpaCheck, cfg } = useApp();
  const [url, setUrl] = useState<string>("");
  const [copied, setCopied] = useState(false);
  const [open, setOpen] = useState(true);

  const marked = rpaCheck?.mark === "ok";

  // 常驻服务状态：5s 轮询；起不来（端口被占）给原因和「重试」。
  const status = useQuery({
    queryKey: ["seed-server-status"],
    queryFn: seedServerStatus,
    refetchInterval: 5000,
  });
  const [retrying, setRetrying] = useState(false);
  const retry = async () => {
    setRetrying(true);
    try {
      await seedServerApply(cfg);
      toast.success("种子入口服务已启动");
    } catch (e) {
      toast.error("种子入口服务仍起不来", { description: msg(e) });
    } finally {
      setRetrying(false);
      void status.refetch();
    }
  };
  const st = status.data;

  useEffect(() => {
    void seedBootstrapUrl(cfg.seed_host, cfg.seed_port)
      .then(setUrl)
      .catch(() => setUrl(""));
  }, [cfg.seed_host, cfg.seed_port]);

  const copy = async () => {
    if (!url) return;
    try {
      await navigator.clipboard.writeText(url);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1600);
    } catch (e) {
      // 复制失败不阻断：提示后用户可手动选中链接复制。
      toast.error("复制失败，请手动选中链接复制", { description: msg(e) });
    }
  };

  return (
    <Card className="mb-3">
      <button
        type="button"
        onClick={() => setOpen(!open)}
        aria-expanded={open}
        className="flex w-full items-center gap-2 p-4 text-left"
      >
        <Link2 className="size-4 shrink-0 text-foreground" />
        <span className="text-sm font-semibold">种子链接</span>
        <span className="text-xs text-muted-foreground">
          {marked ? "首次设置已完成" : "首次设置，只需一次"}
        </span>
        <ChevronDown
          className={cn(
            "ml-auto size-4 shrink-0 text-muted-foreground transition-transform",
            open && "rotate-180",
          )}
        />
      </button>
      {open && (
        <CardContent className="p-4 pt-0">
          <div className="flex items-center gap-2">
            <code className="flex-1 overflow-x-auto whitespace-nowrap rounded-md bg-muted px-3 py-2 text-xs">
              {url || "加载中…"}
            </code>
            <Button
              variant="secondary"
              size="sm"
              disabled={!url}
              onClick={() => void copy()}
            >
              {copied ? <Check className="text-emerald-600" /> : <Copy />}
              {copied ? "已复制" : "复制"}
            </Button>
          </div>
          {st && (
            <div className="mt-2 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs">
              <span
                className={cn(
                  "size-2 shrink-0 rounded-full",
                  st.running ? "bg-success" : "bg-destructive",
                )}
              />
              {st.running ? (
                <>
                  <span>服务运行中，监听 {st.addr}</span>
                  <span className="text-muted-foreground">
                    {st.last_hit_at
                      ? `· 最近打开 ${fmtLogDateTime(st.last_hit_at)}${
                          st.last_hit_wechat === false ? "（非微信浏览器）" : ""
                        }${st.last_hit_injected ? "，已跳转任务链接" : "，当时没有任务"}`
                      : "· 还没有被打开过"}
                    {st.pending > 0 ? ` · 待打开 ${st.pending} 条` : ""}
                  </span>
                </>
              ) : (
                <>
                  <span className="text-destructive">
                    服务未运行{st.last_error ? `：${st.last_error}` : ""}
                  </span>
                  <Button
                    variant="outline"
                    size="sm"
                    className="h-6 px-2 text-xs"
                    disabled={retrying}
                    onClick={() => void retry()}
                  >
                    <RefreshCw
                      className={cn("size-3", retrying && "animate-spin")}
                    />
                    {retrying ? "重试中…" : "重试"}
                  </Button>
                </>
              )}
            </div>
          )}
          <ol className="mt-3 list-decimal space-y-1 pl-5 text-xs text-muted-foreground">
            <li>
              复制上面的链接，粘贴进微信「文件传输助手」并发送一次，让它留在会话里。它指向本
              App
              自己起的种子入口服务（应用启动即常驻，没任务时点开只显示一行说明；地址
              / 端口在系统设置 「抓包与系统代理」里改，改了要重发并重新框选）。
            </li>
            <li>
              在「微信号管理」里对应的微信号（或下方「启动自检」，作用于当前激活号）点「框选种子」，框住这条链接消息标定位置；可用「测试点击」验证。标定按微信号各存一份。
            </li>
            <li>
              之后每批任务 RPA
              会按标定位置点它拉起浏览器并接力采集；窗口尺寸变了或消息被顶走要重新框选。
            </li>
            <li>
              mac 或未启用 RPA 时手动点开这条链接即可；页面由本机直出且禁缓存，不需要再硬刷新。
            </li>
          </ol>
        </CardContent>
      )}
    </Card>
  );
}
