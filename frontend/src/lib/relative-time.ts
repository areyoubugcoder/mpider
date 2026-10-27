/** 公众号最近活动时间：按传入时钟展示相对时间，保留未知与未来时间的真实语义。 */
export function relativeTime(timestamp: number | null | undefined, now: number): string {
  if (!timestamp || !Number.isFinite(timestamp) || timestamp < 0 || !Number.isFinite(now)) return "—";
  const elapsed = now - timestamp;
  const distance = Math.abs(elapsed);
  if (distance < 60) return elapsed >= 0 ? "刚刚" : "即将";
  const suffix = elapsed >= 0 ? "前" : "后";
  for (const [seconds, unit] of [[365 * 86400, "年"], [30 * 86400, "个月"], [86400, "天"], [3600, "小时"], [60, "分钟"]] as const) {
    if (distance >= seconds) return `${Math.floor(distance / seconds)}${unit}${suffix}`;
  }
  return "—";
}
