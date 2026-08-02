// Browser-side full-state merge for the PUT fallback: real-network UPDATEs
// can fail with "missing contract" while a contract is out of the update mesh
// (freenet-core#5069), but GET and PUT keep working. The fallback fetches the
// current state, merges our signed delta into it, and re-PUTs the result. The
// merged bytes must be exactly what the Rust side would produce (ciborium:
// minimal-length ints, struct fields in declaration order, BTreeMaps in key
// order), pinned byte-for-byte against Rust fixtures by
// web/tests/merge.spec.ts.
//
// Tile and chat merges ride the state.ts mirrors (their inserts already
// mirror the Rust CRDT inserts, caps included) plus typed encoders. The
// registry mirror is lossy (it drops proofs and signatures), so the registry
// merge works on generic CBOR values instead, splicing delta records into the
// state verbatim.

import { CborValue, cborDecode, cborEncode, mapGet } from "./cbor";
import { asByteArray } from "./identity";
import {
  ChatMessage,
  ChatStateJs,
  compareBytes,
  decodeChatDelta,
  decodeChatState,
  decodeTileDelta,
  decodeTileState,
  Placement,
  TileStateJs,
} from "./state";

type CborMap = Map<CborValue, CborValue>;

function asMap(value: CborValue | undefined, what: string): CborMap {
  if (!(value instanceof Map)) throw new Error(`expected a CBOR map for ${what}`);
  return value;
}

function asTs(value: CborValue | undefined, what: string): number {
  if (typeof value !== "number") throw new Error(`expected a number for ${what}`);
  return value;
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

interface RegistryEntry {
  /// AuthorId bytes (== the record's identity_vk bytes).
  key: Uint8Array;
  record: CborMap;
}

function recordKey(record: CborMap): Uint8Array {
  return asByteArray(mapGet(record, "identity_vk"));
}

/// Mirrors registry.rs core_key ordering: (admitted_ts, signature bytes).
function coreLess(a: CborMap, b: CborMap): boolean {
  const tsA = asTs(mapGet(a, "admitted_ts"), "admitted_ts");
  const tsB = asTs(mapGet(b, "admitted_ts"), "admitted_ts");
  if (tsA !== tsB) return tsA < tsB;
  return compareBytes(asByteArray(mapGet(a, "signature")), asByteArray(mapGet(b, "signature"))) < 0;
}

/// Mirrors registry.rs better_nickname: highest (version, signature bytes),
/// with null losing to any nickname.
function betterNickname(a: CborValue | undefined, b: CborValue | undefined): CborValue {
  const x = a instanceof Map ? a : null;
  const y = b instanceof Map ? b : null;
  if (!x) return y;
  if (!y) return x;
  const vx = asTs(mapGet(x, "version"), "nickname version");
  const vy = asTs(mapGet(y, "version"), "nickname version");
  const cmp =
    vy - vx || compareBytes(asByteArray(mapGet(y, "signature")), asByteArray(mapGet(x, "signature")));
  return cmp > 0 ? y : x;
}

/// Mirrors RegistryState::insert_record on generic CBOR records, keeping the
/// entry list sorted by AuthorId bytes (BTreeMap order).
function insertRecord(entries: RegistryEntry[], record: CborMap): void {
  const key = recordKey(record);
  let i = 0;
  while (i < entries.length && compareBytes(entries[i].key, key) < 0) i++;
  if (i === entries.length || compareBytes(entries[i].key, key) !== 0) {
    entries.splice(i, 0, { key, record });
    return;
  }
  const existing = entries[i].record;
  const nickname = betterNickname(mapGet(existing, "nickname"), mapGet(record, "nickname"));
  const core = coreLess(record, existing) ? record : existing;
  core.set("nickname", nickname);
  entries[i].record = core;
}

export function mergeRegistryState(stateBytes: Uint8Array, deltaBytes: Uint8Array): Uint8Array {
  const entries: RegistryEntry[] = [];
  if (stateBytes.length > 0) {
    for (const record of asMap(mapGet(cborDecode(stateBytes), "identities"), "identities").values()) {
      insertRecord(entries, asMap(record, "admission record"));
    }
  }
  if (deltaBytes.length > 0) {
    const delta = cborDecode(deltaBytes);
    const admissions = mapGet(delta, "admissions");
    if (Array.isArray(admissions)) {
      for (const record of admissions) {
        insertRecord(entries, asMap(record, "admission record"));
      }
    }
    const nicknames = mapGet(delta, "nicknames");
    if (Array.isArray(nicknames)) {
      for (const update of nicknames) {
        const key = asByteArray(mapGet(update, "identity_vk"));
        const entry = entries.find((e) => compareBytes(e.key, key) === 0);
        if (!entry) continue; // not (yet) admitted; dropped like registry.rs
        entry.record.set(
          "nickname",
          betterNickname(mapGet(entry.record, "nickname"), mapGet(update, "nickname")),
        );
      }
    }
  }
  const identities: CborMap = new Map();
  for (const entry of entries) identities.set(Array.from(entry.key), entry.record);
  return cborEncode(new Map<CborValue, CborValue>([["identities", identities]]));
}

// ---------------------------------------------------------------------------
// Tile
// ---------------------------------------------------------------------------

function encodePlacement(p: Placement): CborMap {
  return new Map<CborValue, CborValue>([
    ["coord", p.coord],
    ["color", p.color],
    ["ts", p.ts],
    ["author", p.author],
    ["signature", Array.from(p.signature)],
  ]);
}

export function encodeTileState(state: TileStateJs): Uint8Array {
  const placements: CborMap = new Map();
  for (const author of [...state.placements.keys()].sort()) {
    const log = state.placements.get(author)!;
    const inner: CborMap = new Map();
    let authorBytes: Uint8Array | null = null;
    for (const ts of [...log.keys()].sort((a, b) => a - b)) {
      const placement = log.get(ts)!;
      authorBytes = placement.author;
      inner.set(ts, encodePlacement(placement));
    }
    if (authorBytes) placements.set(Array.from(authorBytes), inner);
  }
  return cborEncode(new Map<CborValue, CborValue>([["placements", placements]]));
}

export function mergeTileState(stateBytes: Uint8Array, deltaBytes: Uint8Array): Uint8Array {
  const state = decodeTileState(stateBytes);
  state.applyDelta(decodeTileDelta(deltaBytes));
  return encodeTileState(state);
}

// ---------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------

function encodeMessage(m: ChatMessage): CborMap {
  return new Map<CborValue, CborValue>([
    ["content", m.content],
    ["ts", m.ts],
    ["seq", m.seq],
    ["author", m.author],
    ["signature", Array.from(m.signature)],
  ]);
}

export function encodeChatState(state: ChatStateJs): Uint8Array {
  const messages: CborMap = new Map();
  for (const key of [...state.messages.keys()].sort()) {
    const message = state.messages.get(key)!;
    const id = new Map<CborValue, CborValue>([
      ["ts", message.ts],
      ["author", Array.from(message.author)],
      ["seq", message.seq],
    ]);
    messages.set(id, encodeMessage(message));
  }
  return cborEncode(new Map<CborValue, CborValue>([["messages", messages]]));
}

export function mergeChatState(stateBytes: Uint8Array, deltaBytes: Uint8Array): Uint8Array {
  const state = decodeChatState(stateBytes);
  state.applyDelta(decodeChatDelta(deltaBytes));
  return encodeChatState(state);
}
