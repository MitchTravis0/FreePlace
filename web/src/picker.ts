// Extended-palette color picker: a 24-spoke hue wheel, a 9-variant shade
// grid for the chosen hue, the gray ramp, and a hex input snapping to the
// nearest palette entry. Everything reports a contract color index; the
// wheel is a plain 2D canvas (no native color input — its behavior inside
// the gateway's sandboxed iframe is untestable from vite dev).

import {
  GRAY_RAMP_START,
  GRAY_RAMP_STEPS,
  HUE_COUNT,
  HUE_START,
  HUE_VARIANTS,
  PALETTE,
} from "./canvas";
import { nearestPaletteIndex } from "./overlay";

const WHEEL_SIZE = 176;
const OUTER_R = 84;
const INNER_R = 54;
/// The vivid (s=100, l=50) variant used as each spoke's wheel color and as
/// the selection a bare hue click lands on.
const VIVID_VARIANT = 2;

export interface Picker {
  setSelected(index: number): void;
}

export function createPicker(panel: HTMLElement, onSelect: (index: number) => void): Picker {
  const wheel = document.createElement("canvas");
  wheel.width = WHEEL_SIZE;
  wheel.height = WHEEL_SIZE;
  wheel.dataset.testid = "picker-wheel";
  const shades = document.createElement("div");
  shades.className = "picker-shades";
  shades.dataset.testid = "picker-shades";
  const grays = document.createElement("div");
  grays.className = "picker-grays";
  grays.dataset.testid = "picker-grays";
  const hexRow = document.createElement("label");
  hexRow.className = "picker-hex-row";
  hexRow.append("hex");
  const hexInput = document.createElement("input");
  hexInput.dataset.testid = "picker-hex";
  hexInput.placeholder = "#rrggbb";
  hexInput.autocomplete = "off";
  hexInput.spellcheck = false;
  hexRow.appendChild(hexInput);
  panel.append(wheel, shades, grays, hexRow);

  let selected = 0;
  let currentHue = 0;
  const ctx = wheel.getContext("2d")!;

  const hueOf = (index: number): number | null =>
    index >= HUE_START ? Math.floor((index - HUE_START) / HUE_VARIANTS.length) : null;

  function drawWheel(): void {
    ctx.clearRect(0, 0, WHEEL_SIZE, WHEEL_SIZE);
    const c = WHEEL_SIZE / 2;
    const seg = (2 * Math.PI) / HUE_COUNT;
    for (let h = 0; h < HUE_COUNT; h++) {
      // Hue 0 at 12 o'clock, clockwise.
      const start = h * seg - Math.PI / 2 - seg / 2;
      ctx.beginPath();
      ctx.arc(c, c, OUTER_R, start, start + seg);
      ctx.arc(c, c, INNER_R, start + seg, start, true);
      ctx.closePath();
      ctx.fillStyle = PALETTE[HUE_START + h * HUE_VARIANTS.length + VIVID_VARIANT];
      ctx.fill();
      if (h === currentHue) {
        ctx.lineWidth = 2.5;
        ctx.strokeStyle = "#ffffff";
        ctx.stroke();
      }
    }
    // Centre preview of the current selection.
    ctx.beginPath();
    ctx.arc(c, c, INNER_R - 10, 0, 2 * Math.PI);
    ctx.fillStyle = PALETTE[selected];
    ctx.fill();
    ctx.lineWidth = 1.5;
    ctx.strokeStyle = "#2c2c34";
    ctx.stroke();
  }

  function swatch(index: number, parent: HTMLElement): HTMLButtonElement {
    const button = document.createElement("button");
    button.type = "button";
    button.style.background = PALETTE[index];
    button.dataset.color = String(index);
    button.title = `color ${index}`;
    button.classList.toggle("selected", index === selected);
    button.addEventListener("click", () => onSelect(index));
    parent.appendChild(button);
    return button;
  }

  function rebuildShades(): void {
    shades.replaceChildren();
    for (let v = 0; v < HUE_VARIANTS.length; v++) {
      swatch(HUE_START + currentHue * HUE_VARIANTS.length + v, shades);
    }
  }

  grays.replaceChildren();
  for (let i = 0; i < GRAY_RAMP_STEPS; i++) swatch(GRAY_RAMP_START + i, grays);
  rebuildShades();
  drawWheel();

  wheel.addEventListener("click", (event) => {
    const rect = wheel.getBoundingClientRect();
    const dx = event.clientX - rect.left - WHEEL_SIZE / 2;
    const dy = event.clientY - rect.top - WHEEL_SIZE / 2;
    const dist = Math.hypot(dx, dy);
    if (dist < INNER_R - 4 || dist > OUTER_R + 4) return;
    const seg = 360 / HUE_COUNT;
    const angle = ((Math.atan2(dy, dx) * 180) / Math.PI + 90 + seg / 2 + 360) % 360;
    const h = Math.floor(angle / seg) % HUE_COUNT;
    onSelect(HUE_START + h * HUE_VARIANTS.length + VIVID_VARIANT);
  });

  hexInput.addEventListener("change", () => {
    const match = /^#?([0-9a-f]{6})$/i.exec(hexInput.value.trim());
    if (!match) return;
    const rgb = parseInt(match[1], 16);
    onSelect(nearestPaletteIndex((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff));
  });

  return {
    setSelected(index: number): void {
      selected = index;
      currentHue = hueOf(index) ?? currentHue;
      rebuildShades();
      for (const button of grays.querySelectorAll("button")) {
        button.classList.toggle("selected", Number(button.dataset.color) === index);
      }
      hexInput.value = PALETTE[index];
      drawWheel();
    },
  };
}
