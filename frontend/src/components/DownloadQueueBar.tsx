/** 底栏正文进度：与历史进度使用相同入口，直接进入对应任务标签。 */
import { QueueProgressTrigger } from "@/components/QueueProgressTrigger";
import type { Snapshot } from "@/lib/workbench";

export function DownloadQueueBar({ snapshot, onTasks }: { snapshot?: Snapshot; onTasks: () => void }) {
  const queue = snapshot?.active_queues?.download;
  return queue?.task_count ? <QueueProgressTrigger queue={queue} label="正文下载" hint="点击查看正文下载任务" onClick={onTasks} /> : null;
}
