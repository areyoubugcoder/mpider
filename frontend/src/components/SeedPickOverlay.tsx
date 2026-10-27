import { useCallback, useEffect, useRef, useState } from "react";
import { msg } from "@/lib/utils";
import {
  rpaPickCancel,
  rpaPickCommit,
  rpaSelfCheck,
  type ScreenRect,
} from "@/lib/tauri";

/** 拖拽中的框（CSS 像素，任意两角）。 */
interface Drag {
  x0: number;
  y0: number;
  x1: number;
  y1: number;
}

/** 框选至少要这么大（CSS 像素），否则当误触忽略。 */
const MIN_SIZE = 6;

/**
 * 框选层覆盖显示器的**物理原点**（左上角，绝对物理像素），由后端 `rpa_pick_begin` 经 URL 参数 `ox/oy`
 * 带来。单屏（文件助手在主屏）时为 (0,0)；多屏时文件助手所在屏未必从 (0,0) 起，故必须加上它才能
 * 把窗口内 clientX 换算回**绝对**物理坐标（与 mpider-core `GetWindowRect` 同一坐标系）。
 */
const params = new URLSearchParams(window.location.search);
const ORIGIN_X = Number(params.get("ox")) || 0;
const ORIGIN_Y = Number(params.get("oy")) || 0;

/**
 * CSS 像素 → 屏幕**绝对**物理像素：框选层铺满其所在显示器，窗口内 (clientX,clientY) 先乘 devicePixelRatio
 * 得到相对该显示器左上角的物理偏移，再加上显示器物理原点 `ORIGIN`。缩放 100%/125%/150% 都由
 * devicePixelRatio 反映，无需额外处理。
 */
function toPhysical(d: Drag): ScreenRect {
  const dpr = window.devicePixelRatio || 1;
  return [
    ORIGIN_X + Math.round(d.x0 * dpr),
    ORIGIN_Y + Math.round(d.y0 * dpr),
    ORIGIN_X + Math.round(d.x1 * dpr),
    ORIGIN_Y + Math.round(d.y1 * dpr),
  ];
}

function normalize(d: Drag) {
  return {
    left: Math.min(d.x0, d.x1),
    top: Math.min(d.y0, d.y1),
    width: Math.abs(d.x1 - d.x0),
    height: Math.abs(d.y1 - d.y0),
  };
}

/**
 * 种子框选层：由后端 `rpa_pick_begin` 建的全屏透明置顶窗口加载（`index.html?view=pick`）。
 *
 * 半透明压暗整屏，画出「文件传输助手」窗口的提示框；用户按住鼠标拖一个框住种子链接消息的矩形，
 * 松开即把**物理像素**矩形交给 `rpa_pick_commit`——成功时后端关掉本窗口并通知主窗口；
 * 失败（如框选中心不在文件助手内）在层内提示，可重框；Esc / 右键取消。
 */
export function SeedPickOverlay() {
  const [fhRect, setFhRect] = useState<ScreenRect | null>(null);
  const [drag, setDrag] = useState<Drag | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const dragRef = useRef<Drag | null>(null);

  // 取文件助手矩形画提示框（自检不置前窗口、不打扰框选）。
  useEffect(() => {
    rpaSelfCheck()
      .then((sc) => setFhRect(sc.fh_rect))
      .catch(() => setFhRect(null));
  }, []);

  const cancel = useCallback(() => {
    void rpaPickCancel().catch(() => window.close());
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") cancel();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [cancel]);

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (submitting) return;
    if (e.button === 2) {
      cancel();
      return;
    }
    if (e.button !== 0) return;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    const d = { x0: e.clientX, y0: e.clientY, x1: e.clientX, y1: e.clientY };
    dragRef.current = d;
    setDrag(d);
    setError(null);
  };

  const onPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const d = dragRef.current;
    if (!d) return;
    const next = { ...d, x1: e.clientX, y1: e.clientY };
    dragRef.current = next;
    setDrag(next);
  };

  const onPointerUp = async (e: React.PointerEvent<HTMLDivElement>) => {
    const d = dragRef.current;
    dragRef.current = null;
    if (!d || e.button !== 0) return;
    const final = { ...d, x1: e.clientX, y1: e.clientY };
    const n = normalize(final);
    if (n.width < MIN_SIZE || n.height < MIN_SIZE) {
      setDrag(null);
      return;
    }
    setDrag(final);
    setSubmitting(true);
    try {
      await rpaPickCommit(toPhysical(final));
      // 成功：后端会关掉本窗口；这里不再动状态。
    } catch (err) {
      setError(msg(err));
      setSubmitting(false);
      setDrag(null);
    }
  };

  const dpr = window.devicePixelRatio || 1;
  // fhRect 是绝对物理坐标；减去框选层显示器原点得到窗口内物理偏移，再除 dpr 回到 CSS 像素。
  const fh = fhRect
    ? {
        left: (fhRect[0] - ORIGIN_X) / dpr,
        top: (fhRect[1] - ORIGIN_Y) / dpr,
        width: (fhRect[2] - fhRect[0]) / dpr,
        height: (fhRect[3] - fhRect[1]) / dpr,
      }
    : null;
  const box = drag ? normalize(drag) : null;

  return (
    <div
      className="fixed inset-0 select-none overflow-hidden"
      style={{ cursor: "crosshair", background: "rgba(0,0,0,0.28)" }}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={(e) => void onPointerUp(e)}
      onContextMenu={(e) => e.preventDefault()}
    >
      {/* 文件助手窗口提示框 */}
      {fh && (
        <div
          className="pointer-events-none absolute rounded-sm"
          style={{
            left: fh.left,
            top: fh.top,
            width: fh.width,
            height: fh.height,
            boxShadow: "0 0 0 2px rgba(59,130,246,0.9)",
            background: "rgba(255,255,255,0.06)",
          }}
        >
          <span className="absolute -top-6 left-0 whitespace-nowrap rounded bg-blue-600 px-2 py-0.5 text-xs text-white">
            文件传输助手 —— 框住蓝色链接文字那一行
          </span>
        </div>
      )}
      {/* 拖拽中的框 */}
      {box && (
        <div
          className="pointer-events-none absolute"
          style={{
            left: box.left,
            top: box.top,
            width: box.width,
            height: box.height,
            border: "2px solid #f0abfc",
            background: "rgba(240,171,252,0.18)",
          }}
        />
      )}
      {/* 顶部指引条 */}
      <div className="pointer-events-none absolute left-1/2 top-4 -translate-x-1/2 rounded-md bg-black/75 px-4 py-2 text-xs text-white shadow-lg">
        {submitting
          ? "正在保存标定…"
          : "按住鼠标左键，框住「文件传输助手」里种子链接的蓝色文字那一行后松开（点击会在框内随机取点）；Esc 或右键取消"}
      </div>
      {error && (
        <div className="pointer-events-none absolute left-1/2 top-16 -translate-x-1/2 max-w-[80vw] rounded-md bg-amber-600/95 px-4 py-2 text-xs text-white shadow-lg">
          {error}（可重新框选）
        </div>
      )}
    </div>
  );
}
