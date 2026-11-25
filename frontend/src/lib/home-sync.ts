/** 主页同步与恢复状态的产品语义；不以服务开启状态推断会话有效。 */
import type { Discovery, Source, Task } from "./workbench.ts";
export const recoveryIsWaiting = (task: Task) => task.state === "needs_action" && ["starting", "waiting_home", "preparing_session"].includes(task.progress.session_recovery?.phase || "");
export function needsSessionRecovery(task: Task, source?: Source) {
  if (!["history", "home"].includes(task.kind) || !["paused", "needs_action", "failed"].includes(task.state)) return false;
  if (recoveryIsWaiting(task)) return true;
  if (task.progress.blocked_session_id) return !source?.session_id || task.progress.blocked_session_id === source.session_id;
  return !!task.progress.session_recovery || !source?.session_id || /H5 Session Timeout|authentication_failed|会话认证失败/.test(task.reason || "");
}
export function homeSyncState(discovery?: Discovery | null) {
  const state = discovery?.state;
  return { text: state === "running" ? "已开启" : state === "starting" ? "连接中" : state === "stopping" ? "正在关闭" : state === "failed" ? "连接异常" : "已关闭", active: state === "starting" || state === "running" || state === "stopping", ready: state === "running" };
}
export function recoveryMessage(task: Task, discovery?: Discovery | null, source?: Source) {
  const r = task.progress.session_recovery;
  const target = source?.nickname?.trim() ? `「${source.nickname.trim()}」公众号的文章列表主页` : "该公众号的文章列表主页";
  if (!recoveryIsWaiting(task)) return needsSessionRecovery(task, source)
    ? `文章列表会话缺失或已失效。请点击“更新会话并继续”，等待主页识别就绪后，在专机微信中打开${target}；已打开时请刷新或重新进入。新会话保存后自动重试，原进度保留。`
    : task.reason || "会话需要更新，原任务进度已保留。";
  if (r?.phase === "starting" || discovery?.state === "starting") return `正在启动主页识别。识别就绪后，请在微信打开${target}。`;
  if (r?.phase === "preparing_session") return "已收到目标公众号首页，正在准备新会话。";
  return `识别已就绪，请在微信打开${target}；若主页已打开，请重新进入或刷新。收到新会话后将自动重试，无需再次点击。`;
}
