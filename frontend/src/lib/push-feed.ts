import type { PushEvent } from "./push-monitor";

/** 新记录插入和分页刷新可能短暂重叠；按身份去重并持续移除已过期记录。 */
export function recentPushEvents(pages: readonly { items: PushEvent[] }[], now: number): PushEvent[] {
  const events = new Map<number, PushEvent>();
  for (const page of pages) for (const event of page.items) {
    if (event.first_seen_at >= now - 3600 && event.first_seen_at <= now && !events.has(event.id)) events.set(event.id, event);
  }
  return [...events.values()].sort((a, b) => b.first_seen_at - a.first_seen_at || b.id - a.id);
}
