/** 与正文下载共用底栏进度样式；仅在实际导出期间显示。 */
import { useState, useSyncExternalStore } from "react";
import { QueueProgressTrigger } from "@/components/QueueProgressTrigger";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Progress } from "@/components/ui/progress";
import { articleExportProgress, type ExportQueueProgress } from "@/lib/export-progress";

export function ExportQueueBar() {
  const queue = useSyncExternalStore(articleExportProgress.subscribe, articleExportProgress.getSnapshot);
  return queue ? <ExportQueueProgressView queue={queue} /> : null;
}

function ExportQueueProgressView({ queue }: { queue: ExportQueueProgress }) {
  const [open, setOpen] = useState(false);
  return <>
    <QueueProgressTrigger queue={queue} label="文章导出" hint="点击查看文章导出进度" onClick={() => setOpen(true)} />
    <Dialog open={open} onOpenChange={setOpen}><DialogContent><DialogHeader>
      <DialogTitle>文章导出进度</DialogTitle>
      <DialogDescription>正在写出 Markdown、HTML 和已保存的图片，切换页面不会中断导出。</DialogDescription>
    </DialogHeader><Progress aria-label="文章导出进度" value={queue.total ? queue.completed / queue.total * 100 : 0} />
      <p role="status" className="text-sm">已写出 {queue.completed} / {queue.total} 篇 · {queue.task_count} 个任务{queue.completed === queue.total ? "，正在生成索引和完成导出…" : ""}</p>
    </DialogContent></Dialog>
  </>;
}
