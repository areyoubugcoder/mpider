/** 滑入广告的关闭时间独立于内容缓存，刷新配置或重启不会重置冷却。 */
export const ADVERTISEMENT_DISMISSAL_KEY = "mpider.advertisement-dismissed-at.v1";
export const ADVERTISEMENT_COOLDOWN_MS = 60 * 60 * 1000;
type Storage = Pick<globalThis.Storage, "getItem" | "setItem">;

export function readAdvertisementDismissal(storage?: Storage): number | null {
  try {
    const value = storage?.getItem(ADVERTISEMENT_DISMISSAL_KEY);
    if (!value) return null;
    const timestamp = Number(value);
    return Number.isSafeInteger(timestamp) && timestamp > 0 ? timestamp : null;
  } catch { return null; }
}

export function advertisementCooldownRemaining(dismissedAt: number | null, now = Date.now()): number {
  if (dismissedAt === null || !Number.isSafeInteger(dismissedAt) || dismissedAt <= 0) return 0;
  // 系统时钟回拨时最多等待一小时，避免异常时间导致长期隐藏。
  return Math.min(ADVERTISEMENT_COOLDOWN_MS, Math.max(0, dismissedAt + ADVERTISEMENT_COOLDOWN_MS - now));
}

export function saveAdvertisementDismissal(storage?: Storage, now = Date.now()): number {
  try { storage?.setItem(ADVERTISEMENT_DISMISSAL_KEY, String(now)); } catch { /* 内存状态仍维持本次运行的冷却。 */ }
  return now;
}
