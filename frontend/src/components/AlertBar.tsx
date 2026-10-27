import { AlertTriangle, ShieldAlert, PauseCircle, MousePointerClick } from "lucide-react";
import { useApp } from "@/store";
import { Button } from "@/components/ui/button";
import { fmtLogDateTime } from "@/lib/tauri";
import type { View } from "@/views/types";

/**
 * 全局提醒横幅：挂在主内容区顶部，任何页面都能看到。
 *
 * 只显示后端记下的「最近一条未确认提醒」（预算达上限 / 微信号受限 / 历史抓取暂停 / 人工模式请打开文章），
 * 「知道了」调 alert_ack 清掉；「查看」跳到公众号列表看历史抓取进度。数据来自 store 的 2 秒轮询，
 * 新提醒到达时 store 已经弹过 toast，这里只负责常驻提示。
 *
 * `manual_open` 是「要你动手」而不是「出事了」：用警示黄而不是危险红，没有「查看」（无处可看），
 * 凭证到位 / 等待超时后端会自动收掉，用户不必点「知道了」。
 */
export function AlertBar({ onNav }: { onNav: (v: View) => void }) {
  const { alert, ackAlert } = useApp();
  if (!alert) return null;
  const manual = alert.kind === "manual_open";
  const Icon = manual
    ? MousePointerClick
    : alert.kind === "budget"
      ? AlertTriangle
      : alert.kind === "blocked"
        ? ShieldAlert
        : PauseCircle;
  const title = manual
    ? "请在微信里打开一篇文章"
    : alert.kind === "budget"
      ? "列表请求预算已达上限"
      : alert.kind === "blocked"
        ? "微信号被限制"
        : "历史抓取已暂停";
  const tone = manual
    ? "border-warning/50 bg-warning/10 text-warning-foreground"
    : "border-destructive/40 bg-destructive/10 text-destructive";
  const sub = manual ? "opacity-90" : "text-destructive/90";
  const meta = manual ? "opacity-70" : "text-destructive/70";
  return (
    <div
      role="alert"
      className={`mb-3 flex items-start gap-3 rounded-lg border px-4 py-2.5 text-xs ${tone}`}
    >
      <Icon className="mt-0.5 size-4 shrink-0" />
      <div className="min-w-0 flex-1">
        <div className="font-medium">{title}</div>
        <div className={`mt-0.5 break-words ${sub}`}>{alert.message}</div>
        <div className={`mt-0.5 text-[11px] ${meta}`}>{fmtLogDateTime(alert.at)}</div>
      </div>
      <div className="flex shrink-0 items-center gap-1">
        {!manual && (
          <Button
            variant="ghost"
            size="sm"
            className="text-destructive hover:text-destructive"
            onClick={() => onNav("accounts")}
          >
            查看
          </Button>
        )}
        {/* 「知道了」只是确认已读（可回滚），用中性按钮，不用红色吓人 */}
        <Button variant="outline" size="sm" onClick={() => void ackAlert()}>
          知道了
        </Button>
      </div>
    </div>
  );
}
