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
export function recoveryMessage(task: Task, discovery?: Discovery | null) {
  const r = task.progress.session_recovery;
  if (!recoveryIsWaiting(task)) return task.reason || "会话需要更新，原任务进度已保留。";
  if (r?.phase === "starting" || discovery?.state === "starting") return "正在启动主页识别。识别就绪后，请在微信打开该公众号主页。";
  if (r?.phase === "preparing_session") return "已收到目标公众号首页，正在准备新会话。";
  return "识别已就绪，请在微信打开该公众号主页；若主页已打开，请重新进入或刷新。收到新会话后将自动继续，无需再次点击。";
}
