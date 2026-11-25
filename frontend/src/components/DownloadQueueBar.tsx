/** 底栏正文进度：与历史进度使用相同入口，直接进入对应任务标签。 */
import { QueueProgressTrigger } from "@/components/QueueProgressTrigger";
import { Download } from "lucide-react";
import type { Snapshot } from "@/lib/workbench";

export function DownloadQueueBar({ snapshot, onTasks, compact = false }: { snapshot?: Snapshot; onTasks: () => void; compact?: boolean }) {
  const queue = snapshot?.active_queues?.download;
  return queue?.task_count ? <QueueProgressTrigger queue={queue} label="文章下载" hint="点击查看文章下载任务" icon={Download} compact={compact} onClick={onTasks} /> : null;
}
