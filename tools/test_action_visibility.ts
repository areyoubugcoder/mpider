/** 操作列回归：会话恢复、任务状态和正文完整性决定唯一的主操作。 */
import test from "node:test";
import assert from "node:assert/strict";
import { canCancelTask, sourceBodyAction, taskPrimaryAction } from "../frontend/src/lib/action-visibility.ts";
import type { Source, Task } from "../frontend/src/lib/workbench.ts";

const source = { session_id: "new-session" } as Source;
const task = (extra: Partial<Task> = {}): Task => ({ id: 1, kind: "history", biz: "fake", state: "paused", target: {}, progress: { completed: 0, pages: 0, new_articles: 0, skipped: 0, partial: 0, failed: 0, rebuilds: 0, rebuilding: false, finished_ids: [] }, reason: null, next_action_at: 0, created_at: 0, updated_at: 0, result_path: null, ...extra });

test("任务状态只产生一个主操作；已结束和取消不再显示继续或取消", () => {
  for (const state of ["running", "queued"]) assert.equal(taskPrimaryAction(task({ state }), source), "pause");
  for (const state of ["paused", "needs_action", "failed"]) assert.equal(taskPrimaryAction(task({ state }), source), "resume");
  for (const state of ["done", "cancelled"]) {
    assert.equal(taskPrimaryAction(task({ state }), source), null);
    assert.equal(canCancelTask(task({ state })), false);
  }
});
test("过期会话显示恢复；新会话已保存则显示继续；等待恢复时只允许撤销自动继续", () => {
  const blocked = task({ progress: { ...task().progress, blocked_session_id: "old-session" } });
  assert.equal(taskPrimaryAction(blocked, { session_id: "old-session" } as Source), "recover");
  assert.equal(taskPrimaryAction(blocked, source), "resume");
  const waiting = task({ state: "needs_action", progress: { ...blocked.progress, session_recovery: { discovery_id: 1, requested_at: 0, previous_session_id: "old-session", phase: "waiting_home" } } });
  assert.equal(taskPrimaryAction(waiting, source), "stop_recovery");
});
test("下载结束仍缺正文或图片时提供重试；完整下载与取消的任务不提供重试", () => {
  const partial = task({ kind: "archive", state: "done", progress: { ...task().progress, partial: 1 } });
  assert.equal(taskPrimaryAction(partial), "retry_download");
  assert.equal(taskPrimaryAction({ ...partial, state: "cancelled" }), null);
  assert.equal(taskPrimaryAction({ ...partial, progress: task().progress }), null);
});
test("整号仅在全部非空正文齐备时允许导出；缺正文提供下载，未知状态不猜测", () => {
  assert.equal(sourceBodyAction(10, 0), "download");
  assert.equal(sourceBodyAction(10, 9), "download");
  assert.equal(sourceBodyAction(10, 10), "export");
  for (const count of [undefined, -1, 11, NaN, 1.5]) assert.equal(sourceBodyAction(10, count), null);
  assert.equal(sourceBodyAction(0, 0), null);
});
