/**
 * Domain-warped fbm sampled into a monospace glyph ramp.
 * Same kind of fluid ASCII field as the undivisible.dev background
 * (flow, then glyphs) — original field, no shapes, no page chrome.
 */

const RAMP = " .'`^,:;Il!i><~+_-?][}{1)(|/tfjrxnuvczXYUJCLQ0OZmwqpdbkhao*#MW&8%B@$";

function hash(ix: number, iy: number): number {
  const s = Math.sin(ix * 127.1 + iy * 311.7) * 43758.5453123;
  return s - Math.floor(s);
}

function noise(x: number, y: number): number {
  const x0 = Math.floor(x);
  const y0 = Math.floor(y);
  const fx = x - x0;
  const fy = y - y0;
  const ux = fx * fx * (3 - 2 * fx);
  const uy = fy * fy * (3 - 2 * fy);
  const a = hash(x0, y0);
  const b = hash(x0 + 1, y0);
  const c = hash(x0, y0 + 1);
  const d = hash(x0 + 1, y0 + 1);
  return a + (b - a) * ux + (c - a) * uy + (a - b - c + d) * ux * uy;
}

function fbm(x: number, y: number): number {
  let value = 0;
  let amplitude = 0.5;
  let px = x;
  let py = y;
  for (let i = 0; i < 4; i++) {
    value += amplitude * noise(px, py);
    px = px * 2.03 + 1.7;
    py = py * 2.03 + 9.2;
    amplitude *= 0.5;
  }
  return value;
}

/** Density in 0..1 at a normalized plane coordinate. Exported for tests. */
export function sampleFluid(
  x: number,
  y: number,
  time: number,
  mouseX = 0.5,
  mouseY = 0.5,
): number {
  const t = time * 0.085;
  const q1 = fbm(x + t * 0.55, y - t * 0.32);
  const q2 = fbm(x + 5.2 - t * 0.28, y + 1.3 + t * 0.22);
  const dx = x - mouseX * 3.2;
  const dy = y - mouseY * 2.2;
  const dist = Math.hypot(dx, dy);
  const pull = Math.exp(-dist * 0.85) * 0.7;
  const r1 = fbm(x + q1 * 1.7 + dx * pull + t * 0.25, y + q2 * 1.7 + dy * pull);
  const r2 = fbm(x + q2 * 1.35 - t * 0.18, y + q1 * 1.35 + t * 0.12);
  const density = fbm(x + r1 * 1.55, y + r2 * 1.55);
  return Math.min(1, Math.max(0, density));
}

export function startFluid(canvas: HTMLCanvasElement, reduced: () => boolean): () => void {
  const ctx = canvas.getContext("2d", { alpha: true });
  if (!ctx) return () => {};

  let width = 0;
  let height = 0;
  let cols = 0;
  let rows = 0;
  let running = true;
  let frame = 0;
  let mouseX = 0.72;
  let mouseY = 0.28;
  const started = performance.now();

  const resize = () => {
    width = canvas.clientWidth;
    height = canvas.clientHeight;
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    canvas.width = Math.max(1, Math.floor(width * dpr));
    canvas.height = Math.max(1, Math.floor(height * dpr));
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    const cell = width < 800 ? 15 : 13;
    cols = Math.max(8, Math.floor(width / (cell * 0.62)));
    rows = Math.max(8, Math.floor(height / cell));
  };

  const paint = (time: number) => {
    ctx.clearRect(0, 0, width, height);
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    const cellW = width / cols;
    const cellH = height / rows;
    ctx.font = `${Math.max(11, cellH * 0.78)}px "Chivo Mono", ui-monospace, monospace`;
    const aspect = width / Math.max(height, 1);
    for (let row = 0; row < rows; row++) {
      for (let col = 0; col < cols; col++) {
        const nx = (col / cols) * aspect * 2.4;
        const ny = (row / rows) * 2.2;
        const density = sampleFluid(nx, ny, time, mouseX, mouseY);
        const index = Math.min(RAMP.length - 1, Math.floor(density * (RAMP.length - 1)));
        const glyph = RAMP[index] ?? " ";
        if (glyph === " ") continue;
        const light = 118 + density * 132;
        const cool = 8 + density * 18;
        ctx.fillStyle = `rgb(${light - 6}, ${light}, ${light + cool})`;
        ctx.globalAlpha = 0.38 + density * 0.62;
        ctx.fillText(glyph, (col + 0.5) * cellW, (row + 0.5) * cellH);
      }
    }
    ctx.globalAlpha = 1;
  };

  const onMove = (event: PointerEvent) => {
    mouseX = event.clientX / Math.max(width, 1);
    mouseY = event.clientY / Math.max(height, 1);
  };

  resize();
  paint((performance.now() - started) / 1000);
  window.addEventListener("pointermove", onMove, { passive: true });
  const onResize = () => {
    resize();
    paint((performance.now() - started) / 1000);
  };
  window.addEventListener("resize", onResize);
  const observer = new ResizeObserver(onResize);
  observer.observe(canvas);

  const tick = (now: number) => {
    if (!running) return;
    frame = requestAnimationFrame(tick);
    if (reduced() || document.hidden) return;
    paint((now - started) / 1000);
  };
  frame = requestAnimationFrame(tick);

  return () => {
    running = false;
    cancelAnimationFrame(frame);
    window.removeEventListener("pointermove", onMove);
    window.removeEventListener("resize", onResize);
    observer.disconnect();
  };
}
