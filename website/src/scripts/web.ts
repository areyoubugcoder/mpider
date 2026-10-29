// 装饰性蛛网：轻微晃动、鼠标波纹、离屏暂停与减少动态效果支持。
type WebPoint = { x: number; y: number; pin: number };
type Ripple = { x: number; y: number; strength: number; at: number };
(() => {
  const candidate = document.getElementById('mpider-landing');
  if (!candidate) return;
  const root = candidate;
  const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)');
  const theme = window.matchMedia('(prefers-color-scheme: dark)');
  const TAU = Math.PI * 2;

  for (const page of root.querySelectorAll<HTMLElement>('.kp-shell')) {
    const canvas = document.createElement('canvas');
    canvas.className = 'kp-web-canvas';
    canvas.setAttribute('aria-hidden', 'true');
    page.prepend(canvas);
    const context = canvas.getContext('2d');
    if (!context) continue;
    const ctx = context;

    let width = 0;
    let height = 0;
    let visible = false;
    let frameId = 0;
    let lastPaint = 0;
    let ink = '#1c293c';
    let points: WebPoint[] = [];
    let spokes: number[][] = [];
    let arcs: [number, number][] = [];
    let ripples: Ripple[] = [];
    let pointerSample: { x: number; y: number; at: number } | null = null;
    let lastRipple = 0;

    function buildWeb() {
      const center = { x: width * .5, y: Math.min(height * .48, 360) };
      const ringsByRay: number[][] = [];
      points = [{ ...center, pin: .55 }];
      spokes = [];
      arcs = [];
      for (let ray = 0; ray < 18; ray++) {
        const angle = ray * TAU / 18 - .19;
        const dx = Math.cos(angle);
        const dy = Math.sin(angle);
        const bx = Math.abs(dx) < .0001 ? Infinity : (dx > 0 ? width - center.x : -center.x) / dx;
        const by = Math.abs(dy) < .0001 ? Infinity : (dy > 0 ? height - center.y : -center.y) / dy;
        const boundary = Math.min(bx, by);
        const spoke = [0];
        const ringMap = [];
        for (let radius = 34; radius < boundary - 10; radius += 56) {
          const index = points.length;
          points.push({ x: center.x + dx * radius, y: center.y + dy * radius, pin: 1 });
          spoke.push(index);
          ringMap.push(index);
        }
        spoke.push(points.length);
        points.push({ x: center.x + dx * boundary, y: center.y + dy * boundary, pin: 0 });
        spokes.push(spoke);
        ringsByRay.push(ringMap);
      }
      for (let ray = 0; ray < 18; ray++) {
        const next = (ray + 1) % 18;
        for (let ring = 0; ring < Math.min(ringsByRay[ray].length, ringsByRay[next].length); ring++) {
          arcs.push([ringsByRay[ray][ring], ringsByRay[next][ring]]);
        }
      }
    }

    function ripple(x: number, y: number, strength: number, now = performance.now()) {
      if (reducedMotion.matches) return;
      ripples.push({ x, y, strength: Math.max(2, Math.min(13, strength)), at: now });
      ripples = ripples.slice(-12);
    }

    function displacement(point: WebPoint, now: number) {
      if (reducedMotion.matches || point.pin === 0) return point;
      const seconds = now / 1000;
      let dx = Math.sin(seconds * .87 + point.y * .009 + point.x * .003) * 1.05 + Math.sin(seconds * .43 + point.x * .009) * .35;
      let dy = Math.sin(seconds * .74 + point.x * .008 - point.y * .002) * .9;
      for (const wave of ripples) {
        const age = (now - wave.at) / 1000;
        const rx = point.x - wave.x;
        const ry = point.y - wave.y;
        const distance = Math.hypot(rx, ry);
        const front = distance - age * 290;
        const envelope = Math.exp(-Math.pow(front / 78, 2)) * Math.exp(-age * .95);
        const amount = Math.sin(front * .048) * wave.strength * envelope;
        dx += rx / Math.max(1, distance) * amount;
        dy += ry / Math.max(1, distance) * amount;
      }
      return { x: point.x + dx * point.pin, y: point.y + dy * point.pin };
    }

    function draw(now = performance.now()) {
      if (!width || !height || !points.length) return;
      ctx.clearRect(0, 0, width, height);
      ripples = ripples.filter(wave => now - wave.at < 3400);
      const moved = points.map(point => displacement(point, now));
      const center = moved[0];
      ctx.strokeStyle = ink;
      ctx.lineWidth = .75;
      ctx.globalAlpha = theme.matches ? .13 : .11;
      ctx.beginPath();
      for (const spoke of spokes) {
        ctx.moveTo(moved[spoke[0]].x, moved[spoke[0]].y);
        for (let i = 1; i < spoke.length; i++) ctx.lineTo(moved[spoke[i]].x, moved[spoke[i]].y);
      }
      for (const [aIndex, bIndex] of arcs) {
        const a = moved[aIndex];
        const b = moved[bIndex];
        const mx = (a.x + b.x) / 2;
        const my = (a.y + b.y) / 2;
        ctx.moveTo(a.x, a.y);
        ctx.quadraticCurveTo(mx + (center.x - mx) * .075, my + (center.y - my) * .075, b.x, b.y);
      }
      ctx.stroke();
      ctx.globalAlpha = 1;
    }

    function frame(now: number) {
      frameId = 0;
      if (!root.isConnected || !visible || document.hidden || reducedMotion.matches) return;
      if (now - lastPaint >= 28) {
        draw(now);
        lastPaint = now;
      }
      frameId = requestAnimationFrame(frame);
    }

    function start() {
      if (!visible || document.hidden) return;
      if (reducedMotion.matches) { draw(); return; }
      if (!frameId) frameId = requestAnimationFrame(frame);
    }

    function stop() {
      if (frameId) cancelAnimationFrame(frameId);
      frameId = 0;
    }

    function resize() {
      if (!page.getClientRects().length) return;
      const nextWidth = page.clientWidth;
      const nextHeight = page.clientHeight;
      if (!nextWidth || !nextHeight || (nextWidth === width && nextHeight === height)) return;
      width = nextWidth;
      height = nextHeight;
      const dpr = Math.min(2, window.devicePixelRatio || 1);
      canvas.width = Math.round(width * dpr);
      canvas.height = Math.round(height * dpr);
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      buildWeb();
      draw();
    }

    function palette() {
      ink = getComputedStyle(page).color;
      draw();
    }

    page.addEventListener('pointermove', event => {
      if (event.pointerType === 'touch' || reducedMotion.matches) return;
      const bounds = page.getBoundingClientRect();
      const point = { x: event.clientX - bounds.left, y: event.clientY - bounds.top, at: performance.now() };
      if (pointerSample) {
        const moved = Math.hypot(point.x - pointerSample.x, point.y - pointerSample.y);
        const elapsed = Math.max(8, point.at - pointerSample.at);
        if (moved > .5 && point.at - lastRipple > 65) {
          ripple(point.x, point.y, 3 + Math.min(9, moved / elapsed * 5), point.at);
          lastRipple = point.at;
        }
      }
      pointerSample = point;
    }, { passive: true });
    page.addEventListener('pointerleave', () => { pointerSample = null; });
    page.addEventListener('pointerdown', event => {
      const bounds = page.getBoundingClientRect();
      ripple(event.clientX - bounds.left, event.clientY - bounds.top, 7);
    }, { passive: true });

    const resizeObserver = new ResizeObserver(() => { resize(); start(); });
    resizeObserver.observe(page);
    const visibilityObserver = new IntersectionObserver(entries => {
      visible = entries[0].isIntersecting;
      if (visible) { resize(); start(); } else stop();
    });
    visibilityObserver.observe(page);
    const styleObserver = new MutationObserver(palette);
    styleObserver.observe(page, { attributes: true, attributeFilter: ['style', 'class', 'data-tone'] });
    document.addEventListener('visibilitychange', () => {
      if (document.hidden) stop(); else start();
    });
    theme.addEventListener('change', palette);
    reducedMotion.addEventListener('change', () => {
      ripples = [];
      stop();
      draw();
      start();
    });
    palette();
    resize();
  }
})();
