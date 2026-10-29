/** 导出生命周期：选目录不显示任务，实际写出才显示，结束后移除。 */
import test from "node:test";
import assert from "node:assert/strict";
import { createExportProgressStore, type ArticleExportProgress } from "../frontend/src/lib/export-progress.ts";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

test("无任务、等待目录和取消选择时不显示进度", async () => {
  const store = createExportProgressStore();
  const picker = deferred<null>();
  const pending = store.run(() => picker.promise);
  assert.equal(store.getSnapshot(), null);
  picker.resolve(null);
  assert.equal(await pending, null);
  assert.equal(store.getSnapshot(), null);
  await assert.rejects(store.run(async () => { throw new Error("校验失败"); }), /校验失败/);
  assert.equal(store.getSnapshot(), null);
});

test("逐篇更新进度，写完文章仍等待索引发布，完成后隐藏并忽略迟到消息", async () => {
  const store = createExportProgressStore();
  const completion = deferred<string>();
  let report!: (value: ArticleExportProgress) => void;
  let notifications = 0;
  const unsubscribe = store.subscribe(() => { notifications++; });
  const pending = store.run(onProgress => { report = onProgress; return completion.promise; });
  report({ completed: 0, total: 76 });
  assert.deepEqual(store.getSnapshot(), { task_count: 1, completed: 0, total: 76 });
  const stable = store.getSnapshot();
  assert.equal(store.getSnapshot(), stable);
  report({ completed: 30, total: 76 });
  assert.deepEqual(store.getSnapshot(), { task_count: 1, completed: 30, total: 76 });
  report({ completed: 76, total: 76 });
  assert.notEqual(store.getSnapshot(), null);
  completion.resolve("导出路径");
  assert.equal(await pending, "导出路径");
  assert.equal(store.getSnapshot(), null);
  report({ completed: 76, total: 76 });
  assert.equal(store.getSnapshot(), null);
  assert.equal(notifications, 4);
  unsubscribe();
});

test("多个导出独立累计，失败只移除对应任务，全部结束后隐藏", async () => {
  const store = createExportProgressStore();
  const first = deferred<void>();
  const second = deferred<void>();
  const pendingA = store.run(report => { report({ completed: 2, total: 10 }); return first.promise; });
  const pendingB = store.run(report => { report({ completed: 3, total: 20 }); return second.promise; });
  assert.deepEqual(store.getSnapshot(), { task_count: 2, completed: 5, total: 30 });
  const failed = assert.rejects(pendingA, /磁盘写入失败/);
  first.reject(new Error("磁盘写入失败"));
  await failed;
  assert.deepEqual(store.getSnapshot(), { task_count: 1, completed: 3, total: 20 });
  second.resolve();
  await pendingB;
  assert.equal(store.getSnapshot(), null);
});
