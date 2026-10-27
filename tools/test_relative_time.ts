/** 相对时间展示契约：更新时间跨分钟推进，未知与未来时刻不会伪装成已采集。 */
import test from "node:test";
import assert from "node:assert/strict";
import { relativeTime } from "../frontend/src/lib/relative-time.ts";
const now = 1800000000;
test("最近活动以易读的相对时间展示", () => {
  for (const [seconds, expected] of [[10,"刚刚"],[300,"5分钟前"],[7200,"2小时前"],[259200,"3天前"],[5184000,"2个月前"],[31536000,"1年前"]] as const) assert.equal(relativeTime(now-seconds, now),expected);
});
test("时钟推进后刚刚会自然变为分钟前", () => {
  assert.equal(relativeTime(now,now+59),"刚刚");
  assert.equal(relativeTime(now,now+60),"1分钟前");
});
test("没有记录或时间无效时保持未知", () => {
  for (const timestamp of [null,undefined,0,-1,NaN,Infinity]) assert.equal(relativeTime(timestamp,now),"—");
});
test("未来时间明确显示为未来，避免掩盖时钟偏差", () => {
  assert.equal(relativeTime(now+300,now),"5分钟后");
});
