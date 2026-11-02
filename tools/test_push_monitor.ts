import test from "node:test";
import assert from "node:assert/strict";
import { pushPhase, pushQueuePhase, type PushRun, type PushQueueRun } from "../frontend/src/lib/push-monitor.ts";

test("停止监测时仍显示独立的队列处理状态", () => {
  const queue: PushQueueRun = { id: 1, active: true, phase: "running", next_fetch_at: 0, reason: null };
  assert.equal(pushPhase({ active: false, phase: "stopped", queue } as PushRun), "已停止");
  assert.equal(pushQueuePhase(queue, 10, 1000), "正在处理");
  assert.equal(pushQueuePhase(queue, 0, 1000), "等待待处理链接");
});
test("解析限流和暂停不改变监测状态，队列原因独立显示", () => {
  const queue: PushQueueRun = { id: 2, active: true, phase: "running", next_fetch_at: 2000, reason: "预算已用完" };
  assert.equal(pushPhase({ active: true, phase: "running", queue } as PushRun), "监测中");
  assert.equal(pushQueuePhase(queue, 10, 1000), "等待请求额度或间隔");
  assert.equal(pushQueuePhase({ ...queue, active: false, phase: "paused" }, 10, 1000), "已暂停");
  assert.equal(pushQueuePhase({ ...queue, active: false, phase: "failed" }, 10, 1000), "处理异常");
});
