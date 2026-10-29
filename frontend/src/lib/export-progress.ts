/** 文章导出的窗口级进度；切换页面不丢失，选目录期间不显示任务。 */
export interface ArticleExportProgress { completed: number; total: number }
export interface ExportQueueProgress extends ArticleExportProgress { task_count: number }

export function createExportProgressStore() {
  const tasks = new Map<symbol, ArticleExportProgress>();
  const listeners = new Set<() => void>();
  let snapshot: ExportQueueProgress | null = null;
  function publish() {
    snapshot = tasks.size ? { task_count: tasks.size, completed: 0, total: 0 } : null;
    if (snapshot) for (const progress of tasks.values()) {
      snapshot.completed += progress.completed;
      snapshot.total += progress.total;
    }
    for (const listener of listeners) listener();
  }
  return {
    subscribe(listener: () => void) { listeners.add(listener); return () => { listeners.delete(listener); }; },
    getSnapshot: () => snapshot,
    async run<T>(execute: (onProgress: (progress: ArticleExportProgress) => void) => Promise<T>): Promise<T> {
      const id = Symbol();
      let settled = false;
      try {
        return await execute(progress => {
          if (settled) return;
          tasks.set(id, progress);
          publish();
        });
      } finally {
        settled = true;
        if (tasks.delete(id)) publish();
      }
    },
  };
}

export const articleExportProgress = createExportProgressStore();
