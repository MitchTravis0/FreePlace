// FreePlace: the shared 1024x1024 canvas with a global chat room. Wires the
// backend (real node or ?mock=1) to the board renderer, palette + cooldown,
// chat sidebar, and the admission onboarding flow (PoW grind in a Web Worker
// with a ghost key skip offer).

import "./style.css";
import { Backend, createBackend, tileIndex } from "./backend";
import { CanvasView, PALETTE, paletteRgb } from "./canvas";
import { loadOverlayImage, OverlayImage } from "./overlay";
import type { PowWorkerMessage } from "./pow-worker";
// Inline (blob URL) worker: the gateway's sandboxed iframe has an opaque
// origin, so constructing a Worker from an http(s) asset URL throws a
// SecurityError there; blob: is allowed by the gateway CSP.
import PowWorker from "./pow-worker?worker&inline";
import {
  CANVAS_SIZE,
  canvasFromWinners,
  EMPTY_PIXEL,
  hex,
  MAX_CHAT_MESSAGE_BYTES,
  MAX_NICKNAME_BYTES,
  Placement,
  TILE_SIZE,
  TILES_PER_SIDE,
  tierCooldownSecs,
} from "./state";

const app = document.querySelector<HTMLDivElement>("#app")!;
app.innerHTML = `
  <header>
    <h1>FreePlace</h1>
    <span class="status-chip" data-testid="conn-status">connecting</span>
    <span class="status-chip identity-chip" data-testid="identity-chip" title="click to edit your nickname"></span>
    <form class="nickname-form hidden" data-testid="nickname-form">
      <input data-testid="nickname-edit" maxlength="32" autocomplete="off" />
      <button class="primary" type="submit" data-testid="nickname-save">Save</button>
    </form>
    <button class="faq-button hidden" data-testid="btn-upgrade" title="admitted by proof of work? a ghost key shortens your cooldown">Use ghost key</button>
    <span class="status-chip" data-testid="activity"></span>
    <span class="cooldown" data-testid="cooldown"></span>
    <button class="faq-button" data-testid="btn-replay">History</button>
    <button class="faq-button" data-testid="btn-faq">FAQ</button>
    <button class="chat-toggle" data-testid="btn-chat-toggle">Chat<span class="unread-badge" data-testid="chat-unread"></span></button>
  </header>
  <div class="banner hidden" data-testid="config-banner"></div>
  <div class="content">
    <div class="board-area">
      <canvas data-testid="board"></canvas>
      <div class="replay-bar hidden" data-testid="replay-bar">
        <span class="replay-banner" data-testid="replay-banner">viewing history</span>
        <button type="button" data-testid="btn-replay-play">Play</button>
        <input type="range" step="1" data-testid="replay-scrubber" />
      </div>
      <div class="pixel-tooltip hidden" data-testid="pixel-tooltip"></div>
      <span class="coords-chip" data-testid="coords"></span>
      <div class="palette" data-testid="palette"></div>
      <div class="template-panel" data-testid="overlay-panel">
        <button class="panel-toggle" data-testid="btn-overlay-toggle">Template</button>
        <div class="panel-body hidden" data-testid="overlay-body">
          <input type="file" accept="image/*" data-testid="overlay-file" />
          <div class="anchor-row">
            <label>x <input type="number" min="0" max="1023" value="0" data-testid="overlay-x" /></label>
            <label>y <input type="number" min="0" max="1023" value="0" data-testid="overlay-y" /></label>
            <button
              type="button"
              data-testid="btn-overlay-drag"
              title="click or drag on the board to position the template"
            >drag to place</button>
          </div>
          <label class="slider-row">
            opacity
            <input
              type="range"
              min="0.1"
              max="1"
              step="0.05"
              value="0.5"
              data-testid="overlay-opacity"
            />
          </label>
          <label class="diff-row">
            <input type="checkbox" data-testid="overlay-diff" />
            diff mode
            <span data-testid="overlay-remaining"></span>
          </label>
          <button type="button" data-testid="btn-overlay-clear">clear template</button>
          <p class="muted" data-testid="overlay-note">
            Overlays are session-only: the app runs in a sandboxed frame
            without storage, so a reload clears them. Nothing is shared with
            other users.
          </p>
        </div>
      </div>
    </div>
    <aside class="chat" data-testid="chat-panel">
      <h2>Global chat</h2>
      <div class="chat-messages" data-testid="chat-messages"></div>
      <form class="chat-form" data-testid="chat-form">
        <button
          class="share-spot"
          type="button"
          data-testid="btn-share-spot"
          title="insert the coordinate at the centre of your view"
        >&#128205;</button>
        <input
          data-testid="chat-input"
          maxlength="512"
          placeholder="say something"
          autocomplete="off"
        />
        <button class="primary" type="submit" data-testid="chat-send">Send</button>
      </form>
    </aside>
  </div>
  <div class="overlay hidden" data-testid="onboarding">
    <div class="onboarding-card">
      <h2>Join the canvas</h2>
      <p class="muted">
        Placing pixels needs a one-time admission so bots cannot flood the
        board. Your browser is solving a small proof-of-work puzzle; you can
        skip the wait with a ghost key.
      </p>
      <label>
        Nickname (optional)
        <input data-testid="nickname-input" maxlength="32" autocomplete="off" />
      </label>
      <section>
        <h3>Proof of work</h3>
        <p class="muted" data-testid="pow-status">preparing challenge</p>
        <progress data-testid="pow-progress" max="100" value="0"></progress>
      </section>
      <section>
        <h3>Have a ghost key?</h3>
        <p class="muted">Skip the wait and get a faster pixel cooldown.</p>
        <button class="primary" data-testid="btn-ghostkey">Use a ghost key</button>
        <p class="muted" data-testid="ghostkey-status"></p>
        <p class="disclosure" data-testid="ghostkey-disclosure">
          Ghost keys cost money; the money funds Freenet, and the mint is
          centralized. The free proof-of-work path is always sufficient for
          full participation &mdash; a ghost key only shortens the pixel
          cooldown.
        </p>
      </section>
      <button class="faq-button" data-testid="btn-onboarding-dismiss">
        Watch the canvas for now
      </button>
      <p class="muted">
        Admission keeps running in the background; you can browse the board
        and chat history meanwhile. Click the canvas to come back here.
      </p>
    </div>
  </div>
  <div class="overlay hidden" data-testid="faq">
    <div class="onboarding-card faq-card">
      <h2>FAQ</h2>
      <section>
        <h3>How does placing work?</h3>
        <p class="muted">
          Pick a color, click the canvas: one pixel, then a cooldown before the
          next (20&nbsp;s with proof-of-work admission, 2&nbsp;s with a ghost
          key). Everything lives in Freenet contracts &mdash; there is no
          server.
        </p>
      </section>
      <section>
        <h3>How is the cooldown enforced?</h3>
        <p class="muted" data-testid="faq-cooldown">
          The canvas is 16 independent tile contracts, and each tile enforces
          the cooldown separately; this app enforces the true global cooldown
          for you. A determined cheater running modified software could
          therefore place up to 16 pixels (one per tile) per cooldown window,
          each from an identity that costs proof-of-work to create. This is a
          known, accepted v1 compromise.
        </p>
      </section>
      <section>
        <h3>What are ghost keys?</h3>
        <p class="muted">
          Ghost keys cost money; the money funds Freenet, and the mint is
          centralized. The free proof-of-work path is always sufficient for
          full participation &mdash; a ghost key only shortens the pixel
          cooldown.
        </p>
      </section>
      <section>
        <h3>Do template overlays persist?</h3>
        <p class="muted" data-testid="faq-overlay">
          No &mdash; overlays are session-only. The gateway serves the app in
          a sandboxed frame without web storage, so a template must be loaded
          again after a reload. Overlays are local: nothing about them is
          shared with other users.
        </p>
      </section>
      <section>
        <h3>How far back does the history replay go?</h3>
        <p class="muted" data-testid="faq-replay">
          Only the recent window this client holds: each tile keeps the last 8
          placements per painter, so the timelapse replays recent activity,
          not full history since the canvas began.
        </p>
      </section>
      <section>
        <h3>Just joined and things seem stuck?</h3>
        <p class="muted">
          A fresh admission can take a few minutes to reach every peer. The app
          quietly retries your placements and messages for up to 5 minutes
          before reporting an error.
        </p>
      </section>
      <button class="primary" data-testid="btn-faq-close">Close</button>
    </div>
  </div>
  <div class="toast hidden" data-testid="error-toast"></div>
`;

const el = <T extends HTMLElement>(id: string): T => app.querySelector<T>(`[data-testid="${id}"]`)!;

const connStatus = el("conn-status");
const identityChip = el("identity-chip");
const cooldownEl = el("cooldown");
const paletteEl = el("palette");
const chatMessagesEl = el("chat-messages");
const chatForm = el<HTMLFormElement>("chat-form");
const chatInput = el<HTMLInputElement>("chat-input");
const onboardingEl = el("onboarding");
const powStatus = el("pow-status");
const powProgress = el<HTMLProgressElement>("pow-progress");
const ghostkeyButton = el<HTMLButtonElement>("btn-ghostkey");
const ghostkeyStatus = el("ghostkey-status");
const upgradeButton = el<HTMLButtonElement>("btn-upgrade");
const nicknameInput = el<HTMLInputElement>("nickname-input");
const toastEl = el("error-toast");
const faqEl = el("faq");
const tooltipEl = el("pixel-tooltip");
const coordsEl = el("coords");
const activityEl = el("activity");
const nicknameForm = el<HTMLFormElement>("nickname-form");
const nicknameEditInput = el<HTMLInputElement>("nickname-edit");
const chatPanel = el("chat-panel");
const chatToggle = el<HTMLButtonElement>("btn-chat-toggle");
const chatUnread = el("chat-unread");

el("btn-faq").addEventListener("click", () => faqEl.classList.remove("hidden"));
el("btn-faq-close").addEventListener("click", () => faqEl.classList.add("hidden"));
// Hide onboarding without stopping the grind or the admission retries; a
// placement click reopens it (placePixel -> openOnboarding) and a successful
// admission closes it for good.
el("btn-onboarding-dismiss").addEventListener("click", () => {
  onboardingEl.classList.add("hidden");
});

let selectedColor = 5;
let cooldownUntil = 0;
let started = false;
let onboardingStarted = false;
let powWorker: Worker | null = null;
let toastTimer: ReturnType<typeof setTimeout> | undefined;

const derivedTiles: Uint8Array[] = Array.from(
  { length: TILES_PER_SIDE * TILES_PER_SIDE },
  () => new Uint8Array(TILE_SIZE * TILE_SIZE).fill(EMPTY_PIXEL),
);
/// Per-tile LWW winner maps, cached alongside the derived pixels so the hover
/// tooltip and winnerAt hook read authorship without re-deriving.
const derivedWinners: Map<number, Placement>[] = Array.from(
  { length: TILES_PER_SIDE * TILES_PER_SIDE },
  () => new Map<number, Placement>(),
);

function toast(message: string): void {
  toastEl.textContent = message;
  toastEl.classList.remove("hidden");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => toastEl.classList.add("hidden"), 6000);
}

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

PALETTE.forEach((color, index) => {
  const swatch = document.createElement("button");
  swatch.style.background = color;
  swatch.dataset.color = String(index);
  swatch.title = `color ${index}`;
  if (index === selectedColor) swatch.classList.add("selected");
  swatch.addEventListener("click", () => {
    selectedColor = index;
    paletteEl.querySelectorAll("button").forEach((b) => b.classList.remove("selected"));
    swatch.classList.add("selected");
  });
  paletteEl.appendChild(swatch);
});

// ---------------------------------------------------------------------------
// Backend wiring
// ---------------------------------------------------------------------------

const backend: Backend = createBackend({
  onConnection: (status) => {
    connStatus.textContent = status;
    if (status === "connected" && !started) {
      started = true;
      onConnected();
    }
  },
  onRegistry: () => {
    updateIdentityChip();
    // Tier changes alter placement validity everywhere; re-derive all tiles.
    for (let x = 0; x < TILES_PER_SIDE; x++) {
      for (let y = 0; y < TILES_PER_SIDE; y++) renderTile(x, y);
    }
    renderChat();
    scheduleActivity();
    refreshOverlay();
    if (started && backend.myTier() !== null) closeOnboarding();
  },
  onTile: (tileX, tileY, arrivals) => {
    renderTile(tileX, tileY);
    scheduleActivity();
    overlayOnTile(tileX, tileY);
    // Arrival animations pause during replay (the rings would point at pixels
    // the historical board does not show).
    if (!arrivals || replay) return;
    // Animate placements from other authors delivered by this delta.
    const me = backend.myAuthorHex();
    for (const placement of arrivals) {
      if (hex(placement.author) === me) continue;
      board.addRecent(
        tileX * TILE_SIZE + (placement.coord % TILE_SIZE),
        tileY * TILE_SIZE + Math.floor(placement.coord / TILE_SIZE),
      );
    }
  },
  onChat: onChatEvent,
  onError: toast,
});

const board = new CanvasView(
  el<HTMLCanvasElement>("board"),
  (x, y) => void placePixel(x, y),
  onHover,
  (x, y) => onOverlayDragPlace(x, y),
);

function renderTile(tileX: number, tileY: number): void {
  const idx = tileIndex(tileX, tileY);
  derivedWinners[idx] = backend.tiles[idx].deriveWinners(backend.registry.tierOf, replay?.cutoff);
  derivedTiles[idx] = canvasFromWinners(derivedWinners[idx]);
  board.renderTile(tileX, tileY, derivedTiles[idx]);
}

function winnerAt(x: number, y: number): Placement | null {
  if (x < 0 || y < 0 || x >= CANVAS_SIZE || y >= CANVAS_SIZE) return null;
  const idx = tileIndex(Math.floor(x / TILE_SIZE), Math.floor(y / TILE_SIZE));
  return derivedWinners[idx].get((y % TILE_SIZE) * TILE_SIZE + (x % TILE_SIZE)) ?? null;
}

function relativeAge(ts: number): string {
  const secs = Math.max(0, Math.floor(Date.now() / 1000) - ts);
  if (secs < 60) return `${secs}s ago`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ago`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h ago`;
  return `${Math.floor(secs / 86400)}d ago`;
}

/// Hover feedback: the coords chip tracks every hovered pixel; the tooltip
/// shows authorship only for painted pixels at the pixel-cursor zoom level.
function onHover(pixel: { x: number; y: number } | null): void {
  if (!pixel) {
    coordsEl.textContent = "";
    tooltipEl.classList.add("hidden");
    return;
  }
  coordsEl.textContent = `(${pixel.x}, ${pixel.y})`;
  const { zoom, panX, panY } = board.view();
  const winner = winnerAt(pixel.x, pixel.y);
  if (zoom < 4 || !winner) {
    tooltipEl.classList.add("hidden");
    return;
  }
  tooltipEl.textContent =
    `${displayName(hex(winner.author))} · color ${winner.color} · ${relativeAge(winner.ts)}`;
  tooltipEl.style.left = `${panX + (pixel.x + 1.5) * zoom}px`;
  tooltipEl.style.top = `${panY + (pixel.y - 0.5) * zoom}px`;
  tooltipEl.classList.remove("hidden");
}

function displayName(authorHex: string): string {
  return backend.registry.nicknameOf(authorHex) ?? authorHex.slice(0, 8);
}

function updateIdentityChip(): void {
  const me = backend.myAuthorHex();
  if (!me) return;
  const tier = backend.myTier();
  identityChip.textContent = tier
    ? `${displayName(me)} · ${tier === "Ghostkey" ? "ghost key" : "proof of work"}`
    : `${me.slice(0, 8)} · not admitted`;
  // The upgrade offer only makes sense for an admitted PoW identity.
  upgradeButton.classList.toggle("hidden", tier !== "Pow");
}

/// Coordinates in either "(x, y)" or bare "x,y" form; a match only becomes a
/// link when both numbers are on the canvas.
const COORD_PATTERN = /\(\s*(\d{1,4})\s*,\s*(\d{1,4})\s*\)|\b(\d{1,4})\s*,\s*(\d{1,4})\b/g;

/// Split a message into text nodes and coordinate-link buttons (never
/// innerHTML); clicking a link centres the viewport on that pixel.
function messageContentNodes(content: string): (Node | string)[] {
  const nodes: (Node | string)[] = [];
  let consumed = 0;
  for (const match of content.matchAll(COORD_PATTERN)) {
    const x = Number(match[1] ?? match[3]);
    const y = Number(match[2] ?? match[4]);
    if (x >= CANVAS_SIZE || y >= CANVAS_SIZE) continue;
    if (match.index > consumed) nodes.push(content.slice(consumed, match.index));
    const link = document.createElement("button");
    link.type = "button";
    link.className = "coord-link";
    link.textContent = match[0];
    link.addEventListener("click", () => {
      board.centerOn(x, y, Math.max(8, board.view().zoom));
    });
    nodes.push(link);
    consumed = match.index + match[0].length;
  }
  if (consumed < content.length) nodes.push(content.slice(consumed));
  return nodes;
}

function renderChat(): void {
  const me = backend.myAuthorHex();
  const messages = backend.chat.validMessages();
  chatMessagesEl.replaceChildren(
    ...messages.map((message) => {
      const row = document.createElement("div");
      row.className = "chat-message";
      const authorHex = hex(message.author);
      if (authorHex === me) row.classList.add("mine");
      const nick = document.createElement("span");
      nick.className = "nick";
      nick.textContent = displayName(authorHex);
      const time = document.createElement("span");
      time.className = "msg-time";
      time.textContent = relativeAge(message.ts);
      time.title = new Date(message.ts * 1000).toLocaleString();
      const text = document.createElement("span");
      text.append(...messageContentNodes(message.content));
      row.append(nick, time, text);
      return row;
    }),
  );
  chatMessagesEl.scrollTop = chatMessagesEl.scrollHeight;
}

chatForm.addEventListener("submit", (event) => {
  event.preventDefault();
  const content = chatInput.value.trim();
  if (!content || new TextEncoder().encode(content).length > MAX_CHAT_MESSAGE_BYTES) return;
  if (backend.myTier() === null) {
    toast("join the canvas before chatting");
    return;
  }
  chatInput.value = "";
  backend.sendChat(content).catch((err: unknown) => toast(`chat send failed: ${String(err)}`));
});

// In-chat coordinate links are the primary sharing mechanism: the gateway's
// sandboxed iframe means the address bar never reflects the app's location,
// so a URL is not something users can copy.
el("btn-share-spot").addEventListener("click", () => {
  const { x, y } = board.centerBoardPixel();
  const prefix = chatInput.value && !chatInput.value.endsWith(" ") ? `${chatInput.value} ` : chatInput.value;
  chatInput.value = `${prefix}(${x}, ${y})`;
  chatInput.focus();
});

// ---------------------------------------------------------------------------
// Narrow-layout chat overlay (Phase 10)
// ---------------------------------------------------------------------------

/// Below this width the chat sidebar leaves the flow and becomes a toggled
/// overlay (style.css media query); the toggle button is only visible there.
const narrowLayout = window.matchMedia("(max-width: 720px)");
let chatSeenCount = 0;

function chatPanelVisible(): boolean {
  return !narrowLayout.matches || chatPanel.classList.contains("open");
}

function updateUnreadBadge(): void {
  const unread = backend.chat.validMessages().length - chatSeenCount;
  chatUnread.textContent = unread > 0 ? String(unread) : "";
}

function markChatSeen(): void {
  chatSeenCount = backend.chat.validMessages().length;
  updateUnreadBadge();
}

function onChatEvent(): void {
  renderChat();
  if (chatPanelVisible()) {
    chatSeenCount = backend.chat.validMessages().length;
  }
  updateUnreadBadge();
}

chatToggle.addEventListener("click", () => {
  chatPanel.classList.toggle("open");
  if (chatPanel.classList.contains("open")) {
    markChatSeen();
    chatMessagesEl.scrollTop = chatMessagesEl.scrollHeight;
  }
});

// Widening the window reveals the sidebar, so everything becomes seen.
narrowLayout.addEventListener("change", () => {
  if (chatPanelVisible()) markChatSeen();
});

// ---------------------------------------------------------------------------
// Nickname editing
// ---------------------------------------------------------------------------

identityChip.addEventListener("click", () => {
  if (backend.myTier() === null) return;
  nicknameEditInput.value = backend.registry.nicknameOf(backend.myAuthorHex()) ?? "";
  nicknameForm.classList.remove("hidden");
  nicknameEditInput.focus();
});

nicknameForm.addEventListener("submit", (event) => {
  event.preventDefault();
  const name = nicknameEditInput.value.trim();
  if (!name || new TextEncoder().encode(name).length > MAX_NICKNAME_BYTES) {
    toast(`nickname must be 1 to ${MAX_NICKNAME_BYTES} bytes`);
    return;
  }
  nicknameForm.classList.add("hidden");
  backend.setNickname(name).catch((err: unknown) => toast(`nickname update failed: ${String(err)}`));
});

// Upgrade a PoW-admitted identity to ghost key tier. The registry converges
// every identity to its earliest admission, so the upgrade record re-admits
// with a timestamp one below the existing one to win that merge.
upgradeButton.addEventListener("click", () => {
  void (async () => {
    upgradeButton.disabled = true;
    toast("asking the ghost key delegate");
    try {
      const challenge = await backend.admissionChallenge();
      const outcome = await backend.requestGhostkey(challenge.bytes);
      switch (outcome.kind) {
        case "signature": {
          const current = backend.myAdmittedTs();
          await backend.admitGhostkey(
            {
              scopedPayload: outcome.scopedPayload,
              signature: outcome.signature,
              certificatePem: outcome.certificatePem,
            },
            null,
            current !== null ? current - 1 : undefined,
          );
          toast("ghost key active: shorter cooldown");
          break;
        }
        case "no-identity":
          toast(`no ghost key available: ${outcome.detail}`);
          break;
        case "access-denied":
          toast("ghost key request declined");
          break;
      }
    } catch (err) {
      toast(`ghost key upgrade failed: ${String(err)}`);
    } finally {
      upgradeButton.disabled = false;
    }
  })();
});

// ---------------------------------------------------------------------------
// Activity signal
// ---------------------------------------------------------------------------

const ACTIVITY_WINDOW_SECS = 3600;
let activityTimer: ReturnType<typeof setTimeout> | undefined;

/// "N painters · M pixels / hour" across all 16 tiles' valid placements.
/// Bounded cost: tiles x authors x K=8 log entries.
function computeActivity(): void {
  const cutoff = Math.floor(Date.now() / 1000) - ACTIVITY_WINDOW_SECS;
  const painters = new Set<string>();
  let pixels = 0;
  for (const tile of backend.tiles) {
    for (const placement of tile.validPlacements(backend.registry.tierOf)) {
      if (placement.ts >= cutoff) {
        painters.add(hex(placement.author));
        pixels++;
      }
    }
  }
  activityEl.textContent = `${painters.size} painters · ${pixels} pixels / hour`;
}

/// Coalesce bursts of tile events into one recount.
function scheduleActivity(): void {
  if (activityTimer !== undefined) return;
  activityTimer = setTimeout(() => {
    activityTimer = undefined;
    computeActivity();
  }, 250);
}

setInterval(computeActivity, 60_000);

// ---------------------------------------------------------------------------
// Template overlay (Phase 11) — local-only coordination aid
// ---------------------------------------------------------------------------

interface OverlayUiState {
  image: OverlayImage;
  x: number;
  y: number;
  remaining: number;
  /// Offscreen w x h canvas the CanvasView blits (repainted by refreshOverlay).
  render: HTMLCanvasElement;
}

let overlayState: OverlayUiState | null = null;
let overlayDragMode = false;

const overlayBody = el("overlay-body");
const overlayFileInput = el<HTMLInputElement>("overlay-file");
const overlayXInput = el<HTMLInputElement>("overlay-x");
const overlayYInput = el<HTMLInputElement>("overlay-y");
const overlayOpacityInput = el<HTMLInputElement>("overlay-opacity");
const overlayDiffInput = el<HTMLInputElement>("overlay-diff");
const overlayRemainingEl = el("overlay-remaining");
const overlayDragButton = el<HTMLButtonElement>("btn-overlay-drag");

function boardColorAt(x: number, y: number): number {
  const idx = tileIndex(Math.floor(x / TILE_SIZE), Math.floor(y / TILE_SIZE));
  return derivedTiles[idx][(y % TILE_SIZE) * TILE_SIZE + (x % TILE_SIZE)];
}

/// Repaint the overlay's offscreen canvas — every target pixel, or in diff
/// mode only those differing from the derived board — and recount the
/// remaining work queue. Bounded by the overlay's own w x h.
function refreshOverlay(): void {
  // Diff recompute pauses during replay: the derived tiles hold historical
  // pixels, so a recount would be against the wrong board. The last-rendered
  // overlay stays as-is; exiting replay refreshes it.
  if (replay) return;
  if (!overlayState) {
    board.setOverlay(null);
    overlayRemainingEl.textContent = "";
    return;
  }
  const { image, x, y, render } = overlayState;
  const ctx = render.getContext("2d")!;
  const pixels = ctx.createImageData(image.width, image.height);
  let remaining = 0;
  for (let i = 0; i < image.targets.length; i++) {
    const target = image.targets[i];
    if (target === EMPTY_PIXEL) continue;
    const differs =
      boardColorAt(x + (i % image.width), y + Math.floor(i / image.width)) !== target;
    if (differs) remaining++;
    if (overlayDiffInput.checked && !differs) continue;
    const [r, g, b] = paletteRgb(target);
    pixels.data[i * 4] = r;
    pixels.data[i * 4 + 1] = g;
    pixels.data[i * 4 + 2] = b;
    pixels.data[i * 4 + 3] = 255;
  }
  ctx.putImageData(pixels, 0, 0);
  overlayState.remaining = remaining;
  overlayRemainingEl.textContent = `remaining: ${remaining}`;
  board.setOverlay({ x, y, opacity: Number(overlayOpacityInput.value), canvas: render });
}

/// Recompute the diff only when the changed tile intersects the overlay's
/// bounding box.
function overlayOnTile(tileX: number, tileY: number): void {
  if (!overlayState) return;
  const { x, y, image } = overlayState;
  const tx = tileX * TILE_SIZE;
  const ty = tileY * TILE_SIZE;
  if (x + image.width <= tx || x >= tx + TILE_SIZE) return;
  if (y + image.height <= ty || y >= ty + TILE_SIZE) return;
  refreshOverlay();
}

/// Clamp so the whole overlay stays on the board.
function setOverlayAnchor(x: number, y: number): void {
  if (!overlayState) return;
  const { image } = overlayState;
  overlayState.x = Math.min(CANVAS_SIZE - image.width, Math.max(0, Math.floor(x) || 0));
  overlayState.y = Math.min(CANVAS_SIZE - image.height, Math.max(0, Math.floor(y) || 0));
  overlayXInput.value = String(overlayState.x);
  overlayYInput.value = String(overlayState.y);
  refreshOverlay();
}

/// Drag-to-place drops the overlay centred under the pointer.
function onOverlayDragPlace(x: number, y: number): void {
  if (!overlayState) return;
  const { image } = overlayState;
  setOverlayAnchor(x - Math.floor(image.width / 2), y - Math.floor(image.height / 2));
}

el("btn-overlay-toggle").addEventListener("click", () => {
  overlayBody.classList.toggle("hidden");
});

overlayFileInput.addEventListener("change", () => {
  const file = overlayFileInput.files?.[0];
  overlayFileInput.value = "";
  if (!file) return;
  loadOverlayImage(file).then(
    (image) => {
      const render = document.createElement("canvas");
      render.width = image.width;
      render.height = image.height;
      overlayState = { image, x: 0, y: 0, remaining: 0, render };
      setOverlayAnchor(Number(overlayXInput.value), Number(overlayYInput.value));
    },
    (err: unknown) => toast(err instanceof Error ? err.message : String(err)),
  );
});

for (const input of [overlayXInput, overlayYInput]) {
  input.addEventListener("input", () => {
    setOverlayAnchor(Number(overlayXInput.value), Number(overlayYInput.value));
  });
}

overlayOpacityInput.addEventListener("input", refreshOverlay);
overlayDiffInput.addEventListener("change", refreshOverlay);

overlayDragButton.addEventListener("click", () => {
  overlayDragMode = !overlayDragMode;
  overlayDragButton.classList.toggle("drag-active", overlayDragMode);
  board.setDragPlace(overlayDragMode);
});

el("btn-overlay-clear").addEventListener("click", () => {
  overlayState = null;
  refreshOverlay();
});

// ---------------------------------------------------------------------------
// Timelapse replay (Phase 12) — display-only playback of the recent window
// each client holds (tile logs keep K=8 placements per author per tile, so
// this is not full history; the FAQ says so). Tile states are never touched:
// the board is re-derived with a ts cutoff, and exiting re-derives live.
// ---------------------------------------------------------------------------

/// Wall-time compression while playing: the cutoff jumps to the next
/// placement timestamp every tick, ~20 placements per second.
const REPLAY_PLACEMENTS_PER_SEC = 20;

interface ReplayState {
  /// Sorted unique timestamps of every valid placement at replay entry.
  timestamps: number[];
  min: number;
  max: number;
  cutoff: number;
  playing: boolean;
}

let replay: ReplayState | null = null;
let replayTimer: ReturnType<typeof setInterval> | undefined;

const replayBar = el("replay-bar");
const replayButton = el<HTMLButtonElement>("btn-replay");
const replayPlayButton = el<HTMLButtonElement>("btn-replay-play");
const replayScrubber = el<HTMLInputElement>("replay-scrubber");

function renderAllTiles(): void {
  for (let x = 0; x < TILES_PER_SIDE; x++) {
    for (let y = 0; y < TILES_PER_SIDE; y++) renderTile(x, y);
  }
}

function setReplayCutoff(ts: number): void {
  if (!replay) return;
  replay.cutoff = Math.min(replay.max, Math.max(replay.min, ts));
  replayScrubber.value = String(replay.cutoff);
  renderAllTiles();
}

function pauseReplay(): void {
  if (replay) replay.playing = false;
  clearInterval(replayTimer);
  replayTimer = undefined;
  replayPlayButton.textContent = "Play";
}

replayPlayButton.addEventListener("click", () => {
  if (!replay) return;
  if (replay.playing) {
    pauseReplay();
    return;
  }
  replay.playing = true;
  replayPlayButton.textContent = "Pause";
  replayTimer = setInterval(() => {
    const active = replay!;
    const next = active.timestamps.find((ts) => ts > active.cutoff);
    if (next === undefined) {
      setReplayCutoff(active.max);
      pauseReplay();
    } else {
      setReplayCutoff(next);
    }
  }, 1000 / REPLAY_PLACEMENTS_PER_SEC);
});

replayScrubber.addEventListener("input", () => {
  setReplayCutoff(Number(replayScrubber.value));
});

replayButton.addEventListener("click", () => {
  if (replay) {
    // Exit: re-deriving without a cutoff restores the live board exactly
    // (state was never modified), and the overlay diff resumes.
    pauseReplay();
    replay = null;
    replayBar.classList.add("hidden");
    replayButton.classList.remove("active");
    renderAllTiles();
    refreshOverlay();
    return;
  }
  const timestamps = [
    ...new Set(
      backend.tiles.flatMap((tile) => tile.validPlacements(backend.registry.tierOf).map((p) => p.ts)),
    ),
  ].sort((a, b) => a - b);
  const now = Math.floor(Date.now() / 1000);
  const min = timestamps[0] ?? now;
  replay = { timestamps, min, max: now, cutoff: min, playing: false };
  replayScrubber.min = String(min);
  replayScrubber.max = String(now);
  replayBar.classList.remove("hidden");
  replayButton.classList.add("active");
  setReplayCutoff(min);
});

// ---------------------------------------------------------------------------
// Placement + cooldown
// ---------------------------------------------------------------------------

async function placePixel(x: number, y: number): Promise<void> {
  if (!started) return;
  if (replay) {
    toast("viewing history — exit replay to place pixels");
    return;
  }
  const tier = backend.myTier();
  if (tier === null) {
    openOnboarding();
    return;
  }
  const remainingMs = cooldownUntil - Date.now();
  if (remainingMs > 0) {
    // New users clicking during cooldown must never see nothing happen.
    toast(`next pixel in ${Math.ceil(remainingMs / 1000)}s`);
    cooldownEl.classList.remove("shake");
    void cooldownEl.offsetWidth; // restart the animation on rapid clicks
    cooldownEl.classList.add("shake");
    return;
  }
  cooldownUntil = Date.now() + tierCooldownSecs(tier) * 1000;
  try {
    await backend.placePixel(x, y, selectedColor);
  } catch (err) {
    cooldownUntil = 0;
    toast(`placement failed: ${String(err)}`);
  }
}

setInterval(() => {
  const remaining = Math.max(0, cooldownUntil - Date.now());
  if (backend.myTier() === null) {
    cooldownEl.textContent = "";
    return;
  }
  if (remaining === 0) {
    cooldownEl.textContent = "ready";
    cooldownEl.classList.add("ready");
  } else {
    cooldownEl.textContent = `next pixel in ${Math.ceil(remaining / 1000)}s`;
    cooldownEl.classList.remove("ready");
  }
}, 250);

// ---------------------------------------------------------------------------
// Onboarding (PoW worker + ghost key offer)
// ---------------------------------------------------------------------------

function onConnected(): void {
  updateIdentityChip();
  if (backend.myTier() === null) {
    openOnboarding();
  }
}

function openOnboarding(): void {
  onboardingEl.classList.remove("hidden");
  if (onboardingStarted) return;
  onboardingStarted = true;
  void startAdmission();
}

function closeOnboarding(): void {
  onboardingEl.classList.add("hidden");
  powWorker?.terminate();
  powWorker = null;
}

function nickname(): string | null {
  const name = nicknameInput.value.trim();
  return name.length > 0 ? name : null;
}

async function startAdmission(): Promise<void> {
  const challengePromise = backend.admissionChallenge();

  // Ghost key offer is live from the start, so its (possible) delegate
  // timeout overlaps the grind instead of adding to it.
  ghostkeyButton.addEventListener("click", () => {
    void (async () => {
      ghostkeyButton.disabled = true;
      ghostkeyStatus.textContent = "asking the ghost key delegate";
      try {
        const challenge = await challengePromise;
        const outcome = await backend.requestGhostkey(challenge.bytes);
        switch (outcome.kind) {
          case "signature":
            ghostkeyStatus.textContent = "ghost key accepted; admitting";
            await backend.admitGhostkey(
              {
                scopedPayload: outcome.scopedPayload,
                signature: outcome.signature,
                certificatePem: outcome.certificatePem,
              },
              nickname(),
            );
            break;
          case "no-identity":
            ghostkeyStatus.textContent = `no ghost key available: ${outcome.detail}`;
            ghostkeyButton.disabled = false;
            break;
          case "access-denied":
            ghostkeyStatus.textContent = "request declined";
            ghostkeyButton.disabled = false;
            break;
        }
      } catch (err) {
        ghostkeyStatus.textContent = `ghost key admission failed: ${String(err)}`;
        toast(`ghost key admission failed: ${String(err)}`);
        ghostkeyButton.disabled = false;
      }
    })();
  });

  let challenge: { bytes: Uint8Array; difficultyBits: number };
  try {
    challenge = await challengePromise;
  } catch (err) {
    powStatus.textContent = `challenge failed: ${String(err)}`;
    return;
  }

  const expected = Math.pow(2, challenge.difficultyBits);
  powStatus.textContent = "searching for a valid nonce";
  powWorker = new PowWorker();
  powWorker.onmessage = (event: MessageEvent<PowWorkerMessage>) => {
    const message = event.data;
    if (message.type === "progress") {
      // Geometric CDF: the honest "how likely were we to be done by now".
      powProgress.value = Math.min(99, 100 * (1 - Math.exp(-message.tried / expected)));
      powStatus.textContent = `searching for a valid nonce (${message.tried.toLocaleString()} hashes)`;
    } else {
      powProgress.value = 100;
      powStatus.textContent = "nonce found; admitting (can take a minute or two on a busy network)";
      backend.admitPow(message.nonce, nickname()).catch((err: unknown) => {
        powStatus.textContent = `admission failed: ${String(err)}`;
        // Also surfaced as a toast so a dismissed onboarding still reports it.
        toast(`admission failed: ${String(err)}`);
      });
    }
  };
  powWorker.postMessage({ challenge: challenge.bytes, difficultyBits: challenge.difficultyBits });
}

// ---------------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------------

const bootQuery = new URLSearchParams(location.search);
const isMock = bootQuery.get("mock") === "1";

// Deep-link params (work in vite dev/standalone; harmless under the gateway,
// where the iframe URL is not user-visible anyway).
const bootX = Number(bootQuery.get("x"));
const bootY = Number(bootQuery.get("y"));
if (
  bootQuery.has("x") &&
  bootQuery.has("y") &&
  Number.isInteger(bootX) &&
  Number.isInteger(bootY) &&
  bootX >= 0 &&
  bootY >= 0 &&
  bootX < CANVAS_SIZE &&
  bootY < CANVAS_SIZE
) {
  board.centerOn(bootX, bootY, Number(bootQuery.get("zoom")) || 8);
}
if (!isMock && __REGISTRY_CONTRACT_ID__ === "") {
  const banner = el("config-banner");
  banner.textContent =
    "No contracts configured for this build. Publish fixtures (make smoke-phase6) or open with ?mock=1.";
  banner.classList.remove("hidden");
  connStatus.textContent = "unconfigured";
} else {
  backend.start().catch((err: unknown) => {
    connStatus.textContent = "connection failed";
    toast(`could not reach the node: ${String(err)}`);
  });
}

// Read-only debug/test hooks (used by the Playwright suites).
(window as unknown as { __freeplace: unknown }).__freeplace = {
  view: () => board.view(),
  pixel: (x: number, y: number): number | null => {
    if (x < 0 || y < 0 || x >= CANVAS_SIZE || y >= CANVAS_SIZE) return null;
    const idx = tileIndex(Math.floor(x / TILE_SIZE), Math.floor(y / TILE_SIZE));
    const color = derivedTiles[idx][(y % TILE_SIZE) * TILE_SIZE + (x % TILE_SIZE)];
    return color === EMPTY_PIXEL ? null : color;
  },
  cooldownRemainingMs: () => Math.max(0, cooldownUntil - Date.now()),
  winnerAt: (x: number, y: number): { author: string; ts: number } | null => {
    const winner = winnerAt(x, y);
    return winner ? { author: hex(winner.author), ts: winner.ts } : null;
  },
  recentArrivals: () => board.recentArrivals(),
  centerPixel: () => board.centerBoardPixel(),
  replay: (): { cutoff: number; min: number; max: number; playing: boolean } | null =>
    replay
      ? { cutoff: replay.cutoff, min: replay.min, max: replay.max, playing: replay.playing }
      : null,
  overlay: (): { x: number; y: number; w: number; h: number; remaining: number } | null =>
    overlayState
      ? {
          x: overlayState.x,
          y: overlayState.y,
          w: overlayState.image.width,
          h: overlayState.image.height,
          remaining: overlayState.remaining,
        }
      : null,
};
