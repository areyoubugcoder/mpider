/** 单行底栏左侧广告位：独立向上轮播，长文省略，悬停或聚焦浮层展示全文。 */
import { useEffect, useId, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import bundledConfig from "../../../config/advertisements.json";
import { ExternalLink } from "@/components/ExternalLink";
import { Popover, PopoverAnchor, PopoverContent } from "@/components/ui/popover";
import { ADVERTISEMENTS_REFRESH_MS, loadAdvertisementConfig, readAdvertisementCache, type AdvertisementConfig } from "@/lib/advertisements";

const TRANSITION_MS = 650;
function localStorageIfAvailable() {
  try { return window.localStorage; } catch { return undefined; }
}

function AdvertisementSlot({ config }: { config: AdvertisementConfig }) {
  const promotions = config.items.filter(item => item.enabled);
  const slides = promotions.length > 1 ? [...promotions, promotions[0]] : promotions;
  const [slide, setSlide] = useState(0);
  const [hovered, setHovered] = useState(false);
  const [focused, setFocused] = useState(false);
  const [open, setOpen] = useState(false);
  const hoveredRef = useRef(false);
  const anchor = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const closeTimer = useRef<ReturnType<typeof setTimeout>>();
  const detailsId = useId();
  const current = promotions[slide % promotions.length];
  const paused = hovered || focused || open;
  function clearCloseTimer() { if (closeTimer.current) clearTimeout(closeTimer.current); }
  function showDetails() { clearCloseTimer(); setOpen(true); }
  function scheduleClose() {
    clearCloseTimer();
    closeTimer.current = setTimeout(() => {
      const active = document.activeElement;
      if (!hoveredRef.current && !anchor.current?.contains(active) && !content.current?.contains(active)) setOpen(false);
    }, 180);
  }
  function enter() { hoveredRef.current = true; setHovered(true); showDetails(); }
  function leave() { hoveredRef.current = false; setHovered(false); scheduleClose(); }
  function focus() { setFocused(true); showDetails(); }
  function blur(event: React.FocusEvent) {
    const next = event.relatedTarget as Node | null;
    if (anchor.current?.contains(next) || content.current?.contains(next)) return;
    setFocused(false); scheduleClose();
  }
  useEffect(() => () => clearCloseTimer(), []);
  useEffect(() => {
    if (open) return;
    // 浮层被移除时不一定触发离开/失焦，按锚点实际状态恢复轮播。
    const overAnchor = !!anchor.current?.matches(":hover");
    hoveredRef.current = overAnchor;
    setHovered(overAnchor);
    setFocused(!!anchor.current?.contains(document.activeElement));
  }, [open]);
  useEffect(() => {
    if (!config.enabled || promotions.length <= 1) return;
    // 只在首尾副本复位时关闭过渡；广告动画独立于系统的减少动画设置。
    if (slide === promotions.length) {
      const reset = setTimeout(() => setSlide(0), TRANSITION_MS);
      return () => clearTimeout(reset);
    }
    if (paused) return;
    const timer = setTimeout(() => setSlide(value => value + 1), config.interval_seconds * 1000);
    return () => clearTimeout(timer);
  }, [slide, paused, promotions.length, config.interval_seconds, config.enabled]);
  if (!config.enabled || !current) return null;
  return <Popover open={open} onOpenChange={setOpen}>
    <PopoverAnchor asChild><div ref={anchor} className="w-full min-w-0" onMouseEnter={enter} onMouseLeave={leave} onFocusCapture={focus} onBlurCapture={blur}>
      <ExternalLink href={current.url} aria-label={current.title} aria-haspopup="dialog" aria-expanded={open} aria-controls={open ? detailsId : undefined} className="block h-6 w-full min-w-0 overflow-hidden leading-6">
        <div aria-hidden="true" style={{ transform: `translateY(-${slide * 1.5}rem)`, transition: slide ? `transform ${TRANSITION_MS}ms ease-in-out` : "none" }}>
          {slides.map(({ id, title }, index) => <div key={`${id}-${index}`} className="h-6 w-full truncate text-left">{title}</div>)}
        </div>
      </ExternalLink>
    </div></PopoverAnchor>
    <PopoverContent ref={content} id={detailsId} side="top" align="start" className="max-h-[min(24rem,70vh)] max-w-[calc(100vw-2rem)] space-y-2 overflow-y-auto" aria-label="完整广告信息" onMouseEnter={enter} onMouseLeave={leave} onFocusCapture={focus} onBlurCapture={blur} onOpenAutoFocus={event => event.preventDefault()} onCloseAutoFocus={event => event.preventDefault()}>
      <p className="break-words text-sm leading-relaxed">{current.title}</p>
      <p className="break-all text-xs text-muted-foreground">{current.url}</p>
    </PopoverContent>
  </Popover>;
}

export function ProjectLinks() {
  const config = useQuery({
    queryKey: ["advertisements"],
    queryFn: () => loadAdvertisementConfig({ fallback: bundledConfig as AdvertisementConfig, storage: localStorageIfAvailable(),
      url: import.meta.env.DEV ? import.meta.env.VITE_ADVERTISEMENTS_URL : undefined }),
    initialData: () => readAdvertisementCache(localStorageIfAvailable()) || bundledConfig as AdvertisementConfig,
    initialDataUpdatedAt: 0,
    staleTime: ADVERTISEMENTS_REFRESH_MS,
    refetchInterval: ADVERTISEMENTS_REFRESH_MS,
    refetchIntervalInBackground: true,
    refetchOnWindowFocus: false,
    retry: false,
  }).data;
  if (!config.enabled || !config.items.some(item => item.enabled)) return null;
  return <nav aria-label="产品广告" className="w-[clamp(11rem,26vw,23rem)] min-w-0 shrink-0 text-xs">
    <AdvertisementSlot key={JSON.stringify(config)} config={config} />
  </nav>;
}
