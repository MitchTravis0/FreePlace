// TypeScript port of the read side of the common/ CRDT cores: CBOR decoding
// of contract states and deltas, commutative inserts, the greedy cooldown /
// rate-limit filters, and LWW canvas derivation. Must mirror common/src/
// {tile,chat,registry}.rs exactly — peers agree on the derived canvas only if
// every client applies the same pure functions. Signature *verification* is
// the contracts' job; the UI only carries signature bytes through (they take
// part in deterministic tie-breaks).

import { CborValue, cborDecode, enumVariant, mapGet } from "./cbor";
import { asByteArray } from "./identity";

// Mirrors common/src/constants.rs (source of truth).
export const TILE_SIZE = 256;
export const TILES_PER_SIDE = 4;
export const CANVAS_SIZE = TILE_SIZE * TILES_PER_SIDE;
export const PALETTE_COLORS = 16;
export const EMPTY_PIXEL = 0xff;
export const MAX_PLACEMENTS_PER_AUTHOR = 8;
export const POW_TILE_COOLDOWN_SECS = 120;
export const GHOSTKEY_TILE_COOLDOWN_SECS = 30;
export const CHAT_MESSAGE_CAP = 500;
export const CHAT_MAX_MESSAGES_PER_AUTHOR = 32;
export const CHAT_MIN_MESSAGE_INTERVAL_SECS = 3;
export const MAX_CHAT_MESSAGE_BYTES = 512;
export const MAX_NICKNAME_BYTES = 32;

export type Tier = "Pow" | "Ghostkey";

export function tierCooldownSecs(tier: Tier): number {
  return tier === "Ghostkey" ? GHOSTKEY_TILE_COOLDOWN_SECS : POW_TILE_COOLDOWN_SECS;
}

export function hex(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

function compareBytes(a: Uint8Array, b: Uint8Array): number {
  const n = Math.min(a.length, b.length);
  for (let i = 0; i < n; i++) {
    if (a[i] !== b[i]) return a[i] - b[i];
  }
  return a.length - b.length;
}

// ---------------------------------------------------------------------------
// Tile state
// ---------------------------------------------------------------------------

export interface Placement {
  coord: number;
  color: number;
  ts: number;
  author: Uint8Array;
  signature: Uint8Array;
}

function decodePlacement(value: CborValue): Placement {
  return {
    coord: asNumber(mapGet(value, "coord")),
    color: asNumber(mapGet(value, "color")),
    ts: asNumber(mapGet(value, "ts")),
    author: asByteArray(mapGet(value, "author")),
    signature: asByteArray(mapGet(value, "signature")),
  };
}

/// Mirrors tile.rs slot_winner: keep `a` iff (sig, coord, color) of a >= b.
function placementSlotWinner(a: Placement, b: Placement): Placement {
  const cmp = compareBytes(a.signature, b.signature) || a.coord - b.coord || a.color - b.color;
  return cmp >= 0 ? a : b;
}

/// Mirrors tile.rs derive_canvas tiebreak: (ts, author bytes, sig bytes).
function lwwGreater(a: Placement, b: Placement): boolean {
  const cmp = a.ts - b.ts || compareBytes(a.author, b.author) || compareBytes(a.signature, b.signature);
  return cmp > 0;
}

/// One tile's placement logs: author hex -> (ts -> placement).
export class TileStateJs {
  placements = new Map<string, Map<number, Placement>>();

  insert(placement: Placement): void {
    const author = hex(placement.author);
    let log = this.placements.get(author);
    if (!log) {
      log = new Map();
      this.placements.set(author, log);
    }
    const existing = log.get(placement.ts);
    log.set(placement.ts, existing ? placementSlotWinner(existing, placement) : placement);
    while (log.size > MAX_PLACEMENTS_PER_AUTHOR) {
      log.delete(Math.min(...log.keys()));
    }
  }

  applyDelta(delta: Placement[]): void {
    for (const placement of delta) this.insert(placement);
  }

  /// The greedy cooldown filter from tile.rs: per author, earliest-first,
  /// accept each placement >= the tier cooldown after the last accepted.
  /// Unadmitted authors contribute nothing. `cutoffTs` (replay) restricts the
  /// input set to placements with ts <= cutoff *before* the filter runs, so
  /// the result is "what the canvas would have shown had only those existed".
  validPlacements(tierOf: (authorHex: string) => Tier | null, cutoffTs?: number): Placement[] {
    const accepted: Placement[] = [];
    for (const [author, log] of this.placements) {
      const tier = tierOf(author);
      if (!tier) continue;
      const cooldown = tierCooldownSecs(tier);
      let lastAccepted: number | null = null;
      const timestamps = [...log.keys()]
        .filter((ts) => cutoffTs === undefined || ts <= cutoffTs)
        .sort((a, b) => a - b);
      for (const ts of timestamps) {
        if (lastAccepted === null || ts - lastAccepted >= cooldown) {
          accepted.push(log.get(ts)!);
          lastAccepted = ts;
        }
      }
    }
    return accepted;
  }

  /// LWW winner per coordinate among valid placements (tiebreak: ts, author
  /// bytes, signature bytes). `cutoffTs` derives the historical board for
  /// replay; display-only, contract semantics untouched.
  deriveWinners(tierOf: (authorHex: string) => Tier | null, cutoffTs?: number): Map<number, Placement> {
    const winners = new Map<number, Placement>();
    for (const placement of this.validPlacements(tierOf, cutoffTs)) {
      const current = winners.get(placement.coord);
      if (!current || lwwGreater(placement, current)) {
        winners.set(placement.coord, placement);
      }
    }
    return winners;
  }

  /// Returns TILE_SIZE^2 palette indices, EMPTY_PIXEL for coordinates with no
  /// valid placement.
  deriveCanvas(tierOf: (authorHex: string) => Tier | null): Uint8Array {
    return canvasFromWinners(this.deriveWinners(tierOf));
  }
}

/// Project a per-coordinate winner map onto tile pixels.
export function canvasFromWinners(winners: Map<number, Placement>): Uint8Array {
  const canvas = new Uint8Array(TILE_SIZE * TILE_SIZE).fill(EMPTY_PIXEL);
  for (const [coord, placement] of winners) {
    canvas[coord] = placement.color;
  }
  return canvas;
}

/// Decode a full TileState (CBOR map {placements: {author -> {ts -> p}}}).
export function decodeTileState(bytes: Uint8Array): TileStateJs {
  const state = new TileStateJs();
  if (bytes.length === 0) return state;
  const placements = mapGet(cborDecode(bytes), "placements");
  if (!(placements instanceof Map)) return state;
  for (const log of placements.values()) {
    if (!(log instanceof Map)) continue;
    for (const placement of log.values()) {
      state.insert(decodePlacement(placement));
    }
  }
  return state;
}

/// Decode a TileDelta (CBOR map {placements: [p, ...]}).
export function decodeTileDelta(bytes: Uint8Array): Placement[] {
  if (bytes.length === 0) return [];
  const placements = mapGet(cborDecode(bytes), "placements");
  if (!Array.isArray(placements)) return [];
  return placements.map(decodePlacement);
}

// ---------------------------------------------------------------------------
// Chat state
// ---------------------------------------------------------------------------

export interface ChatMessage {
  content: string;
  ts: number;
  seq: number;
  author: Uint8Array;
  signature: Uint8Array;
}

function decodeChatMessage(value: CborValue): ChatMessage {
  return {
    content: asString(mapGet(value, "content")),
    ts: asNumber(mapGet(value, "ts")),
    seq: asNumber(mapGet(value, "seq")),
    author: asByteArray(mapGet(value, "author")),
    signature: asByteArray(mapGet(value, "signature")),
  };
}

/// Sortable id string whose lexicographic order equals the Rust MessageId
/// order (ts, author bytes, seq): fixed-width decimal ts and seq, and the
/// author as lowercase hex (hex order == byte order).
function messageIdKey(m: ChatMessage): string {
  return `${m.ts.toString().padStart(16, "0")}:${hex(m.author)}:${m.seq.toString().padStart(10, "0")}`;
}

/// Mirrors chat.rs slot_beats: challenger wins iff (sig, content) is greater.
function messageSlotBeats(challenger: ChatMessage, incumbent: ChatMessage): boolean {
  const enc = new TextEncoder();
  const cmp =
    compareBytes(challenger.signature, incumbent.signature) ||
    compareBytes(enc.encode(challenger.content), enc.encode(incumbent.content));
  return cmp > 0;
}

export class ChatStateJs {
  /// Keyed by messageIdKey; kept sorted lazily at read/evict time.
  messages = new Map<string, ChatMessage>();

  insert(message: ChatMessage): void {
    const key = messageIdKey(message);
    const existing = this.messages.get(key);
    if (!existing || messageSlotBeats(message, existing)) {
      this.messages.set(key, message);
    }
    // Per-author cap first, then the global ring buffer (mirrors chat.rs
    // insert_bounded; both keep the newest).
    const author = hex(message.author);
    const mine = [...this.messages.keys()].filter((k) => k.split(":")[1] === author).sort();
    for (const evicted of mine.slice(0, Math.max(0, mine.length - CHAT_MAX_MESSAGES_PER_AUTHOR))) {
      this.messages.delete(evicted);
    }
    while (this.messages.size > CHAT_MESSAGE_CAP) {
      const oldest = [...this.messages.keys()].sort()[0];
      this.messages.delete(oldest);
    }
  }

  applyDelta(delta: ChatMessage[]): void {
    for (const message of delta) this.insert(message);
  }

  /// The greedy rate-limit filter from chat.rs, in display (id) order.
  validMessages(): ChatMessage[] {
    const lastAccepted = new Map<string, number>();
    const accepted: ChatMessage[] = [];
    for (const key of [...this.messages.keys()].sort()) {
      const message = this.messages.get(key)!;
      const author = hex(message.author);
      const last = lastAccepted.get(author);
      if (last === undefined || message.ts - last >= CHAT_MIN_MESSAGE_INTERVAL_SECS) {
        accepted.push(message);
        lastAccepted.set(author, message.ts);
      }
    }
    return accepted;
  }
}

/// Decode a full ChatState (CBOR map {messages: {id -> message}}).
export function decodeChatState(bytes: Uint8Array): ChatStateJs {
  const state = new ChatStateJs();
  if (bytes.length === 0) return state;
  const messages = mapGet(cborDecode(bytes), "messages");
  if (!(messages instanceof Map)) return state;
  for (const message of messages.values()) {
    state.insert(decodeChatMessage(message));
  }
  return state;
}

/// Decode a ChatDelta (CBOR map {messages: [message, ...]}).
export function decodeChatDelta(bytes: Uint8Array): ChatMessage[] {
  if (bytes.length === 0) return [];
  const messages = mapGet(cborDecode(bytes), "messages");
  if (!Array.isArray(messages)) return [];
  return messages.map(decodeChatMessage);
}

// ---------------------------------------------------------------------------
// Registry state
// ---------------------------------------------------------------------------

export interface RegistryRecord {
  tier: Tier;
  nickname: string | null;
  nicknameVersion: number;
}

export class RegistryStateJs {
  identities = new Map<string, RegistryRecord>();

  tierOf = (authorHex: string): Tier | null => this.identities.get(authorHex)?.tier ?? null;

  nicknameOf(authorHex: string): string | null {
    return this.identities.get(authorHex)?.nickname ?? null;
  }

  insertAdmission(authorHex: string, record: RegistryRecord): void {
    const existing = this.identities.get(authorHex);
    if (!existing) {
      this.identities.set(authorHex, record);
      return;
    }
    // Only the key holder can produce competing records; keep the existing
    // core and take the higher-versioned nickname (mirrors registry.rs).
    if (record.nickname !== null && record.nicknameVersion > existing.nicknameVersion) {
      existing.nickname = record.nickname;
      existing.nicknameVersion = record.nicknameVersion;
    }
  }

  applyNickname(authorHex: string, name: string, version: number): void {
    const existing = this.identities.get(authorHex);
    if (existing && version > existing.nicknameVersion) {
      existing.nickname = name;
      existing.nicknameVersion = version;
    }
  }
}

function decodeAdmissionRecord(value: CborValue): { authorHex: string; record: RegistryRecord } {
  const authorHex = hex(asByteArray(mapGet(value, "identity_vk")));
  const proof = enumVariant(mapGet(value, "proof")!);
  const tier: Tier = proof.variant === "Ghostkey" ? "Ghostkey" : "Pow";
  const nicknameValue = mapGet(value, "nickname");
  let nickname: string | null = null;
  let nicknameVersion = 0;
  if (nicknameValue instanceof Map) {
    nickname = asString(mapGet(nicknameValue, "name"));
    nicknameVersion = asNumber(mapGet(nicknameValue, "version"));
  }
  return { authorHex, record: { tier, nickname, nicknameVersion } };
}

/// Decode a full RegistryState (CBOR map {identities: {author -> record}}).
export function decodeRegistryState(bytes: Uint8Array): RegistryStateJs {
  const state = new RegistryStateJs();
  if (bytes.length === 0) return state;
  const identities = mapGet(cborDecode(bytes), "identities");
  if (!(identities instanceof Map)) return state;
  for (const record of identities.values()) {
    const { authorHex, record: decoded } = decodeAdmissionRecord(record);
    state.insertAdmission(authorHex, decoded);
  }
  return state;
}

/// Apply a RegistryDelta ({admissions: [...], nicknames: [...]}) in place.
export function applyRegistryDelta(state: RegistryStateJs, bytes: Uint8Array): void {
  if (bytes.length === 0) return;
  const delta = cborDecode(bytes);
  const admissions = mapGet(delta, "admissions");
  if (Array.isArray(admissions)) {
    for (const admission of admissions) {
      const { authorHex, record } = decodeAdmissionRecord(admission);
      state.insertAdmission(authorHex, record);
    }
  }
  const nicknames = mapGet(delta, "nicknames");
  if (Array.isArray(nicknames)) {
    for (const update of nicknames) {
      const authorHex = hex(asByteArray(mapGet(update, "identity_vk")));
      const nickname = mapGet(update, "nickname");
      if (nickname instanceof Map) {
        state.applyNickname(
          authorHex,
          asString(mapGet(nickname, "name")),
          asNumber(mapGet(nickname, "version")),
        );
      }
    }
  }
}

/// A CBOR map with an "identities"/"placements"/"messages" *map* field is a
/// full state; the delta forms carry arrays. Used when an update notification
/// may carry either.
export function looksLikeFullState(bytes: Uint8Array, field: string): boolean {
  if (bytes.length === 0) return false;
  try {
    return mapGet(cborDecode(bytes), field) instanceof Map;
  } catch {
    return false;
  }
}

function asNumber(value: CborValue | undefined): number {
  if (typeof value !== "number") throw new Error("expected a numeric field");
  return value;
}

function asString(value: CborValue | undefined): string {
  if (typeof value !== "string") throw new Error("expected a string field");
  return value;
}
