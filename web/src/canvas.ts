// The canvas board: a 1024x1024 backing canvas the tiles are blitted into,
// drawn to the visible canvas with zoom/pan, a pixel-snap hover cursor, and
// click-to-place (a click is a pointer release that never travelled far).

import { CANVAS_SIZE, EMPTY_PIXEL, TILE_SIZE } from "./state";

/// The classic 2017 r/place palette; index order matters (these were the
/// only contract colors in the 16-color era, so indices 0..15 are frozen).
export const CLASSIC_PALETTE = [
  "#ffffff",
  "#e4e4e4",
  "#888888",
  "#222222",
  "#ffa7d1",
  "#e50000",
  "#e59500",
  "#a06a42",
  "#e5d900",
  "#94e044",
  "#02be01",
  "#00d3dd",
  "#0083c7",
  "#0000ea",
  "#cf6ee4",
  "#820080",
];

/// Extended-palette layout (indices are contract colors; 0xff stays the
/// EMPTY_PIXEL sentinel, so 255 entries is the ceiling):
///   0..15    classic palette above (frozen),
///   16..38   23-step gray ramp, light to dark,
///   39..254  24 hues x 9 (saturation, lightness) variants, grouped by hue.
export const GRAY_RAMP_START = 16;
export const GRAY_RAMP_STEPS = 23;
export const HUE_START = GRAY_RAMP_START + GRAY_RAMP_STEPS;
export const HUE_COUNT = 24;
/// Per-hue (saturation%, lightness%) variants: five vivid lightness steps,
/// three muted, one desaturated.
export const HUE_VARIANTS: [number, number][] = [
  [100, 82],
  [100, 66],
  [100, 50],
  [100, 34],
  [100, 20],
  [60, 70],
  [60, 50],
  [60, 30],
  [25, 50],
];

function hslHex(h: number, s: number, l: number): string {
  const sat = s / 100;
  const light = l / 100;
  const channel = (n: number) => {
    const k = (n + h / 30) % 12;
    const a = sat * Math.min(light, 1 - light);
    const value = light - a * Math.max(-1, Math.min(k - 3, 9 - k, 1));
    return Math.round(value * 255)
      .toString(16)
      .padStart(2, "0");
  };
  return `#${channel(0)}${channel(8)}${channel(4)}`;
}

export const PALETTE = [
  ...CLASSIC_PALETTE,
  ...Array.from({ length: GRAY_RAMP_STEPS }, (_, i) => hslHex(0, 0, 96 - i * 4)),
  ...Array.from({ length: HUE_COUNT }, (_, h) =>
    HUE_VARIANTS.map(([s, l]) => hslHex(h * (360 / HUE_COUNT), s, l)),
  ).flat(),
];

/// Empty pixels render as white, like an unpainted r/place board.
const EMPTY_RGBA: [number, number, number] = [255, 255, 255];

const MIN_ZOOM = 0.2;
const MAX_ZOOM = 40;
const CLICK_SLOP_PX = 4;
/// Fingers wobble more than mice; a tap must still read as a click.
const TOUCH_CLICK_SLOP_PX = 12;
/// How long a remote placement's highlight ring stays visible.
const RECENT_ARRIVAL_MS = 1200;

export function paletteRgb(index: number): [number, number, number] {
  if (index === EMPTY_PIXEL || index >= PALETTE.length) return EMPTY_RGBA;
  const hex = PALETTE[index];
  return [
    parseInt(hex.slice(1, 3), 16),
    parseInt(hex.slice(3, 5), 16),
    parseInt(hex.slice(5, 7), 16),
  ];
}

export class CanvasView {
  private backing = document.createElement("canvas");
  private backingCtx: CanvasRenderingContext2D;
  private ctx: CanvasRenderingContext2D;
  private zoom = 1;
  private panX = 0;
  private panY = 0;
  private hover: { x: number; y: number } | null = null;
  /// Active pointers (client coords) keyed by pointerId: one pans or places,
  /// two pinch-zoom around their midpoint.
  private pointers = new Map<number, { x: number; y: number }>();
  /// Where the current gesture's first pointer went down, for click slop.
  private gestureStart: { x: number; y: number } | null = null;
  /// True once the gesture panned, pinched, or ever held a second pointer;
  /// releasing a moved gesture never places a pixel.
  private gestureMoved = false;
  private drawQueued = false;
  /// Remote placements still showing their fading highlight ring. Overlay
  /// only: the backing store never holds non-palette colors.
  private recent: { x: number; y: number; until: number }[] = [];
  /// Template overlay drawn above the backing board, below the hover cursor.
  private overlayImage: {
    x: number;
    y: number;
    opacity: number;
    canvas: HTMLCanvasElement;
  } | null = null;
  /// While true, board clicks and drags reposition the template overlay
  /// (via onDragPlace) instead of panning or placing pixels.
  private dragPlace = false;

  constructor(
    private readonly canvas: HTMLCanvasElement,
    private readonly onPlace: (globalX: number, globalY: number) => void,
    private readonly onHover: (pixel: { x: number; y: number } | null) => void = () => {},
    private readonly onDragPlace: (globalX: number, globalY: number) => void = () => {},
  ) {
    this.backing.width = CANVAS_SIZE;
    this.backing.height = CANVAS_SIZE;
    this.backingCtx = this.backing.getContext("2d")!;
    this.backingCtx.fillStyle = `rgb(${EMPTY_RGBA.join(",")})`;
    this.backingCtx.fillRect(0, 0, CANVAS_SIZE, CANVAS_SIZE);
    this.ctx = canvas.getContext("2d")!;

    new ResizeObserver(() => this.resize()).observe(canvas.parentElement!);
    this.resize();
    this.fit();
    this.bindEvents();
  }

  /// Blit one tile's derived palette indices into its quadrant.
  renderTile(tileX: number, tileY: number, pixels: Uint8Array): void {
    const image = this.backingCtx.createImageData(TILE_SIZE, TILE_SIZE);
    for (let i = 0; i < pixels.length; i++) {
      const [r, g, b] = paletteRgb(pixels[i]);
      image.data[i * 4] = r;
      image.data[i * 4 + 1] = g;
      image.data[i * 4 + 2] = b;
      image.data[i * 4 + 3] = 255;
    }
    this.backingCtx.putImageData(image, tileX * TILE_SIZE, tileY * TILE_SIZE);
    this.requestDraw();
  }

  view(): { zoom: number; panX: number; panY: number } {
    return { zoom: this.zoom, panX: this.panX, panY: this.panY };
  }

  /// Centre the viewport on a board pixel at the given zoom.
  centerOn(x: number, y: number, zoom: number): void {
    this.zoom = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, zoom));
    this.panX = this.canvas.width / 2 - (x + 0.5) * this.zoom;
    this.panY = this.canvas.height / 2 - (y + 0.5) * this.zoom;
    this.requestDraw();
  }

  /// The board pixel at the viewport centre, clamped onto the canvas.
  centerBoardPixel(): { x: number; y: number } {
    const clamp = (v: number) => Math.min(CANVAS_SIZE - 1, Math.max(0, Math.floor(v)));
    return {
      x: clamp((this.canvas.width / 2 - this.panX) / this.zoom),
      y: clamp((this.canvas.height / 2 - this.panY) / this.zoom),
    };
  }

  /// Start a fading highlight ring at a board pixel (a remote arrival).
  addRecent(globalX: number, globalY: number): void {
    this.recent.push({ x: globalX, y: globalY, until: Date.now() + RECENT_ARRIVAL_MS });
    this.requestDraw();
  }

  recentArrivals(): { x: number; y: number; until: number }[] {
    const now = Date.now();
    return this.recent.filter((r) => r.until > now);
  }

  setOverlay(
    overlay: { x: number; y: number; opacity: number; canvas: HTMLCanvasElement } | null,
  ): void {
    this.overlayImage = overlay;
    this.requestDraw();
  }

  setDragPlace(enabled: boolean): void {
    this.dragPlace = enabled;
  }

  private resize(): void {
    const parent = this.canvas.parentElement!;
    if (this.canvas.width !== parent.clientWidth || this.canvas.height !== parent.clientHeight) {
      this.canvas.width = parent.clientWidth;
      this.canvas.height = parent.clientHeight;
    }
    this.requestDraw();
  }

  /// Fit the whole board in the viewport, centred.
  private fit(): void {
    const scale =
      Math.min(this.canvas.width / CANVAS_SIZE, this.canvas.height / CANVAS_SIZE) * 0.95 || 1;
    this.zoom = Math.max(MIN_ZOOM, scale);
    this.panX = (this.canvas.width - CANVAS_SIZE * this.zoom) / 2;
    this.panY = (this.canvas.height - CANVAS_SIZE * this.zoom) / 2;
    this.requestDraw();
  }

  private bindEvents(): void {
    this.canvas.addEventListener("wheel", (event) => {
      event.preventDefault();
      const factor = Math.pow(1.0015, -event.deltaY);
      const next = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, this.zoom * factor));
      const rect = this.canvas.getBoundingClientRect();
      const mx = event.clientX - rect.left;
      const my = event.clientY - rect.top;
      // Keep the board point under the cursor fixed while zooming.
      this.panX = mx - ((mx - this.panX) / this.zoom) * next;
      this.panY = my - ((my - this.panY) / this.zoom) * next;
      this.zoom = next;
      this.hover = this.pixelAt(mx, my);
      this.onHover(this.hover);
      this.requestDraw();
    });

    this.canvas.addEventListener("pointerdown", (event) => {
      // Synthesized (untrusted) pointer events have no capturable pointer.
      try {
        this.canvas.setPointerCapture(event.pointerId);
      } catch {
        /* test-dispatched events */
      }
      if (this.pointers.size === 0) {
        this.gestureStart = { x: event.clientX, y: event.clientY };
        this.gestureMoved = false;
      } else {
        this.gestureMoved = true;
      }
      this.pointers.set(event.pointerId, { x: event.clientX, y: event.clientY });
    });

    this.canvas.addEventListener("pointermove", (event) => {
      const rect = this.canvas.getBoundingClientRect();
      const prev = this.pointers.get(event.pointerId);
      if (!prev || this.pointers.size === 1) {
        this.hover = this.pixelAt(event.clientX - rect.left, event.clientY - rect.top);
        this.onHover(this.hover);
      }
      if (prev && this.pointers.size === 2) {
        // Pinch: zoom by the inter-pointer distance ratio, keeping the board
        // point under the old midpoint pinned to the new midpoint (the wheel
        // path's fixed-point math, plus the midpoint's own drag as pan).
        const other = [...this.pointers.entries()].find(([id]) => id !== event.pointerId)![1];
        const oldDist = Math.hypot(prev.x - other.x, prev.y - other.y);
        const newDist = Math.hypot(event.clientX - other.x, event.clientY - other.y);
        const factor = oldDist > 0 ? newDist / oldDist : 1;
        const next = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, this.zoom * factor));
        const oldMx = (prev.x + other.x) / 2 - rect.left;
        const oldMy = (prev.y + other.y) / 2 - rect.top;
        const newMx = (event.clientX + other.x) / 2 - rect.left;
        const newMy = (event.clientY + other.y) / 2 - rect.top;
        this.panX = newMx - ((oldMx - this.panX) / this.zoom) * next;
        this.panY = newMy - ((oldMy - this.panY) / this.zoom) * next;
        this.zoom = next;
        this.gestureMoved = true;
        this.hover = null;
        this.onHover(null);
      } else if (prev && this.pointers.size === 1 && this.gestureStart) {
        const slop = event.pointerType === "touch" ? TOUCH_CLICK_SLOP_PX : CLICK_SLOP_PX;
        const travel = Math.hypot(
          event.clientX - this.gestureStart.x,
          event.clientY - this.gestureStart.y,
        );
        if (this.gestureMoved || travel > slop) {
          this.gestureMoved = true;
          if (this.dragPlace) {
            if (this.hover) this.onDragPlace(this.hover.x, this.hover.y);
          } else {
            // Deltas from the tracked position, not movementX/Y (which some
            // mobile browsers report as 0 for touch-derived pointer events).
            this.panX += event.clientX - prev.x;
            this.panY += event.clientY - prev.y;
          }
        }
      }
      if (prev) this.pointers.set(event.pointerId, { x: event.clientX, y: event.clientY });
      this.requestDraw();
    });

    this.canvas.addEventListener("pointerup", (event) => {
      if (!this.pointers.delete(event.pointerId)) return;
      if (this.pointers.size > 0 || this.gestureMoved) return;
      const rect = this.canvas.getBoundingClientRect();
      const pixel = this.pixelAt(event.clientX - rect.left, event.clientY - rect.top);
      if (pixel) (this.dragPlace ? this.onDragPlace : this.onPlace)(pixel.x, pixel.y);
    });

    this.canvas.addEventListener("pointercancel", (event) => {
      // A cancelled pointer must never place; the gesture stays "moved"
      // until every remaining pointer lifts.
      if (this.pointers.delete(event.pointerId)) this.gestureMoved = true;
    });

    this.canvas.addEventListener("pointerleave", () => {
      this.hover = null;
      this.onHover(null);
      this.requestDraw();
    });
  }

  private pixelAt(canvasX: number, canvasY: number): { x: number; y: number } | null {
    const x = Math.floor((canvasX - this.panX) / this.zoom);
    const y = Math.floor((canvasY - this.panY) / this.zoom);
    if (x < 0 || y < 0 || x >= CANVAS_SIZE || y >= CANVAS_SIZE) return null;
    return { x, y };
  }

  private requestDraw(): void {
    if (this.drawQueued) return;
    this.drawQueued = true;
    requestAnimationFrame(() => {
      this.drawQueued = false;
      this.draw();
    });
  }

  private draw(): void {
    this.ctx.setTransform(1, 0, 0, 1, 0, 0);
    this.ctx.fillStyle = "#1a1a1f";
    this.ctx.fillRect(0, 0, this.canvas.width, this.canvas.height);
    this.ctx.imageSmoothingEnabled = false;
    this.ctx.setTransform(this.zoom, 0, 0, this.zoom, this.panX, this.panY);
    this.ctx.drawImage(this.backing, 0, 0);
    if (this.overlayImage) {
      this.ctx.globalAlpha = this.overlayImage.opacity;
      this.ctx.drawImage(this.overlayImage.canvas, this.overlayImage.x, this.overlayImage.y);
      this.ctx.globalAlpha = 1;
    }
    const now = Date.now();
    this.recent = this.recent.filter((r) => r.until > now);
    for (const r of this.recent) {
      // t runs 1 -> 0 over the ring's lifetime: fade out while expanding.
      const t = (r.until - now) / RECENT_ARRIVAL_MS;
      const pad = (1 - t) * 2;
      this.ctx.lineWidth = 2 / this.zoom;
      this.ctx.strokeStyle = `rgba(80, 160, 255, ${t.toFixed(3)})`;
      this.ctx.strokeRect(r.x - pad, r.y - pad, 1 + 2 * pad, 1 + 2 * pad);
    }
    if (this.hover && this.zoom >= 4) {
      this.ctx.lineWidth = 2 / this.zoom;
      this.ctx.strokeStyle = "#000000";
      this.ctx.strokeRect(this.hover.x, this.hover.y, 1, 1);
    }
    // Keep animating (and eventually pruning) while rings are alive.
    if (this.recent.length > 0) this.requestDraw();
  }
}
