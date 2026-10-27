/** 广告位配置契约与远程读取：只接受文字和 HTTPS 链接，异常回退最近有效缓存或内置内容。 */
export interface Advertisement {
  id: string;
  enabled: boolean;
  title: string;
  compact_title: string;
  url: string;
}
export interface AdvertisementConfig {
  version: 1;
  enabled: boolean;
  interval_seconds: number;
  items: Advertisement[];
}
export const ADVERTISEMENTS_URL = "https://raw.githubusercontent.com/areyoubugcoder/mpider/main/config/advertisements.json";
export const ADVERTISEMENTS_REFRESH_MS = 30 * 60 * 1000;
export const ADVERTISEMENTS_CACHE_KEY = "mpider.advertisements.v1";
const CACHE_MAX_AGE_MS = 7 * 24 * 60 * 60 * 1000;
const MAX_CONFIG_BYTES = 32 * 1024;
type Storage = Pick<globalThis.Storage, "getItem" | "setItem">;
const record = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);
const text = (value: unknown, max: number): value is string => typeof value === "string" && value.trim().length > 0 && value.length <= max;

export function parseAdvertisementConfig(value: unknown): AdvertisementConfig | null {
  if (!record(value) || value.version !== 1 || typeof value.enabled !== "boolean" ||
    typeof value.interval_seconds !== "number" || !Number.isInteger(value.interval_seconds) || value.interval_seconds < 5 || value.interval_seconds > 60 ||
    !Array.isArray(value.items) || value.items.length > 12) return null;
  const ids = new Set<string>();
  const items: Advertisement[] = [];
  for (const item of value.items) {
    if (!record(item) || !text(item.id, 64) || ids.has(item.id) || typeof item.enabled !== "boolean" ||
      !text(item.title, 160) || !text(item.compact_title, 80) || !text(item.url, 2048)) return null;
    try {
      const url = new URL(item.url);
      if (url.protocol !== "https:" || url.username || url.password) return null;
    } catch { return null; }
    ids.add(item.id);
    items.push({ id: item.id, enabled: item.enabled, title: item.title.trim(), compact_title: item.compact_title.trim(), url: item.url });
  }
  return { version: 1, enabled: value.enabled, interval_seconds: value.interval_seconds, items };
}

export function readAdvertisementCache(storage?: Storage, now = Date.now()): AdvertisementConfig | null {
  try {
    const cached: unknown = JSON.parse(storage?.getItem(ADVERTISEMENTS_CACHE_KEY) || "null");
    if (!record(cached) || typeof cached.saved_at !== "number" || !Number.isFinite(cached.saved_at) ||
      cached.saved_at > now || now - cached.saved_at > CACHE_MAX_AGE_MS) return null;
    return parseAdvertisementConfig(cached.config);
  } catch { return null; }
}

export async function loadAdvertisementConfig({ fallback, storage, url = ADVERTISEMENTS_URL, timeoutMs = 5000 }: {
  fallback: AdvertisementConfig;
  storage?: Storage;
  url?: string;
  timeoutMs?: number;
}): Promise<AdvertisementConfig> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const response = await fetch(url, { signal: controller.signal, credentials: "omit", referrerPolicy: "no-referrer", cache: "no-cache" });
    if (!response.ok || Number(response.headers.get("content-length")) > MAX_CONFIG_BYTES) throw new Error("广告配置暂不可用");
    const body = await response.text();
    if (new TextEncoder().encode(body).byteLength > MAX_CONFIG_BYTES) throw new Error("广告配置过大");
    const config = parseAdvertisementConfig(JSON.parse(body));
    if (!config) throw new Error("广告配置格式无效");
    try { storage?.setItem(ADVERTISEMENTS_CACHE_KEY, JSON.stringify({ saved_at: Date.now(), config })); } catch { /* 缓存不可写不影响有效内容展示。 */ }
    return config;
  } catch { return readAdvertisementCache(storage) || fallback; }
  finally { clearTimeout(timer); }
}
