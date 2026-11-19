import { useEffect, useState } from "react";
import { ArrowUpRight, X } from "lucide-react";
import { ExternalLink } from "@/components/ExternalLink";
import { advertisementStorage, useAdvertisements } from "@/lib/use-advertisements";
import { ADVERTISEMENT_DISMISSAL_KEY, advertisementCooldownRemaining, readAdvertisementDismissal, saveAdvertisementDismissal } from "@/lib/advertisement-dismissal";
import type { AdvertisementConfig } from "@/lib/advertisements";

function CardContent({ config, onClose }: { config: AdvertisementConfig; onClose: () => void }) {
  const items = config.items.filter(item => item.enabled);
  const [index, setIndex] = useState(0);
  const [hovered, setHovered] = useState(false);
  const [focused, setFocused] = useState(false);
  const current = items[index % items.length];
  useEffect(() => {
    if (items.length < 2 || hovered || focused) return;
    const timer = setInterval(() => setIndex(value => (value + 1) % items.length), config.interval_seconds * 1000);
    return () => clearInterval(timer);
  }, [items.length, config.interval_seconds, hovered, focused]);
  if (!current) return null;
  return <aside aria-label="产品推荐" className="pointer-events-auto w-80 max-w-full rounded-xl border bg-background p-5 shadow-lg animate-in slide-in-from-right-full duration-500"
    onMouseEnter={() => setHovered(true)} onMouseLeave={() => setHovered(false)}
    onFocusCapture={() => setFocused(true)} onBlurCapture={event => { if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setFocused(false); }}>
    <div className="mb-3 flex items-center justify-between gap-3">
      <span className="text-xs text-muted-foreground">产品推荐 · 广告</span>
      <button type="button" aria-label="关闭广告，1 小时后再次展示" title="关闭，1 小时后再次展示" onClick={onClose}
        className="-mr-2 -mt-2 flex size-8 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">
        <X className="size-4" aria-hidden="true" />
      </button>
    </div>
    <p className="break-words text-sm font-semibold leading-relaxed">{current.compact_title}</p>
    <p className="mt-2 break-words text-sm leading-relaxed text-muted-foreground">{current.title}</p>
    <ExternalLink href={current.url} className="mt-4 inline-flex items-center gap-1 text-sm font-medium">了解更多<ArrowUpRight className="size-4" aria-hidden="true" /></ExternalLink>
  </aside>;
}

export function AdvertisementCard() {
  const config = useAdvertisements();
  const [dismissedAt, setDismissedAt] = useState(() => readAdvertisementDismissal(advertisementStorage()));
  const [coolingDown, setCoolingDown] = useState(() => advertisementCooldownRemaining(dismissedAt) > 0);
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const check = () => {
      clearTimeout(timer);
      const remaining = advertisementCooldownRemaining(dismissedAt);
      setCoolingDown(remaining > 0);
      if (remaining > 0) timer = setTimeout(check, remaining);
    };
    check();
    window.addEventListener("focus", check);
    document.addEventListener("visibilitychange", check);
    return () => { clearTimeout(timer); window.removeEventListener("focus", check); document.removeEventListener("visibilitychange", check); };
  }, [dismissedAt]);
  useEffect(() => {
    const sync = (event: StorageEvent) => {
      if (event.key === ADVERTISEMENT_DISMISSAL_KEY || event.key === null) {
        const timestamp = readAdvertisementDismissal(advertisementStorage());
        setDismissedAt(timestamp);
        setCoolingDown(advertisementCooldownRemaining(timestamp) > 0);
      }
    };
    window.addEventListener("storage", sync);
    return () => window.removeEventListener("storage", sync);
  }, []);
  if (coolingDown || !config.enabled || !config.items.some(item => item.enabled)) return null;
  return <div className="pointer-events-none absolute inset-0 z-30 flex items-end justify-end overflow-hidden p-4">
    <CardContent key={JSON.stringify(config)} config={config} onClose={() => {
      setCoolingDown(true);
      setDismissedAt(saveAdvertisementDismissal(advertisementStorage()));
    }} />
  </div>;
}
