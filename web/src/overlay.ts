// Template overlays (Phase 11): decode a user-supplied image and quantize it
// to the palette, one image pixel per board pixel. Local-only by
// design — nothing is shared through contracts, and the gateway's sandboxed
// iframe has no web storage, so overlays are session state.

import { PALETTE } from "./canvas";
import { EMPTY_PIXEL } from "./state";

export const MAX_OVERLAY_SIZE = 256;

export interface OverlayImage {
  width: number;
  height: number;
  /// Palette index per pixel; EMPTY_PIXEL where the image is transparent
  /// (no target for that board pixel).
  targets: Uint8Array;
}

const PALETTE_RGB = PALETTE.map((color) => [
  parseInt(color.slice(1, 3), 16),
  parseInt(color.slice(3, 5), 16),
  parseInt(color.slice(5, 7), 16),
]);

/// Nearest palette entry by Euclidean RGB distance.
export function nearestPaletteIndex(r: number, g: number, b: number): number {
  let best = 0;
  let bestDist = Infinity;
  for (let i = 0; i < PALETTE_RGB.length; i++) {
    const [pr, pg, pb] = PALETTE_RGB[i];
    const dist = (r - pr) ** 2 + (g - pg) ** 2 + (b - pb) ** 2;
    if (dist < bestDist) {
      bestDist = dist;
      best = i;
    }
  }
  return best;
}

export async function loadOverlayImage(file: File): Promise<OverlayImage> {
  let bitmap: ImageBitmap;
  try {
    bitmap = await createImageBitmap(file);
  } catch {
    throw new Error("could not decode that file as an image");
  }
  const { width, height } = bitmap;
  if (width > MAX_OVERLAY_SIZE || height > MAX_OVERLAY_SIZE) {
    bitmap.close();
    throw new Error(
      `template image must be at most ${MAX_OVERLAY_SIZE}x${MAX_OVERLAY_SIZE} pixels (got ${width}x${height})`,
    );
  }
  const scratch = document.createElement("canvas");
  scratch.width = width;
  scratch.height = height;
  const ctx = scratch.getContext("2d")!;
  ctx.drawImage(bitmap, 0, 0);
  bitmap.close();
  const data = ctx.getImageData(0, 0, width, height).data;
  const targets = new Uint8Array(width * height);
  for (let i = 0; i < targets.length; i++) {
    targets[i] =
      data[i * 4 + 3] < 128
        ? EMPTY_PIXEL
        : nearestPaletteIndex(data[i * 4], data[i * 4 + 1], data[i * 4 + 2]);
  }
  return { width, height, targets };
}
