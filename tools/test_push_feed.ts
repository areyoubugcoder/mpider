import test from "node:test";
import assert from "node:assert/strict";
import { recentPushEvents } from "../frontend/src/lib/push-feed.ts";
import type { PushEvent } from "../frontend/src/lib/push-monitor.ts";
const event = (id: number, first_seen_at: number, title = "测试") => ({ id, first_seen_at, title } as PushEvent);
test("最近发现只展示一小时内的数据，旧记录刚完成也不重新出现", () => {
  assert.deepEqual(recentPushEvents([{ items: [event(1, 6400), event(2, 6399), event(3, 10001)] }], 10000).map(e => e.id), [1]);
  assert.equal(recentPushEvents([{ items: [event(1, 6400)] }], 10001).length, 0);
});
test("加载更多与新数据刷新重叠时按身份去重，保留最新页状态和稳定顺序", () => {
  const result = recentPushEvents([{ items: [event(4, 9990), event(3, 9980, "新状态")] }, { items: [event(3, 9980, "旧状态"), event(2, 9980)] }], 10000);
  assert.deepEqual(result.map(e => e.id), [4, 3, 2]);
  assert.equal(result[1].title, "新状态");
});
