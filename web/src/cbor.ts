// Minimal CBOR codec matching ciborium's encoding of our protocol types:
// minimal-length unsigned/negative ints, text, byte strings, arrays, and
// maps in insertion order (ciborium writes struct fields in declaration
// order). The exact bytes are pinned by the wire-format tests in
// common/src/delegate_protocol_tests.rs.
//
// Encoding model:
//   number      -> unsigned/negative int (must be a safe integer)
//   string      -> text string
//   Uint8Array  -> byte string
//   Array       -> array
//   Map         -> map (insertion order preserved)
//   plain object-> map with text keys (insertion order)
//   null        -> null, boolean -> bool
//
// Decoding returns the same shapes (maps always decode to Map, byte strings
// to Uint8Array). 64-bit ints beyond Number.MAX_SAFE_INTEGER are rejected —
// nothing in this protocol produces them.

export type CborValue =
  | number
  | string
  | boolean
  | null
  | Uint8Array
  | CborValue[]
  | Map<CborValue, CborValue>
  | { [key: string]: CborValue };

export function cborEncode(value: CborValue): Uint8Array {
  const out: number[] = [];
  writeValue(out, value);
  return Uint8Array.from(out);
}

function writeHead(out: number[], major: number, length: number): void {
  const m = major << 5;
  if (length < 24) {
    out.push(m | length);
  } else if (length < 0x100) {
    out.push(m | 24, length);
  } else if (length < 0x10000) {
    out.push(m | 25, length >>> 8, length & 0xff);
  } else if (length < 0x100000000) {
    out.push(m | 26, (length >>> 24) & 0xff, (length >>> 16) & 0xff, (length >>> 8) & 0xff, length & 0xff);
  } else {
    if (!Number.isSafeInteger(length)) {
      throw new Error(`CBOR length out of safe range: ${length}`);
    }
    const hi = Math.floor(length / 0x100000000);
    const lo = length >>> 0;
    out.push(m | 27, (hi >>> 24) & 0xff, (hi >>> 16) & 0xff, (hi >>> 8) & 0xff, hi & 0xff, (lo >>> 24) & 0xff, (lo >>> 16) & 0xff, (lo >>> 8) & 0xff, lo & 0xff);
  }
}

function writeValue(out: number[], value: CborValue): void {
  if (value === null) {
    out.push(0xf6);
  } else if (typeof value === "boolean") {
    out.push(value ? 0xf5 : 0xf4);
  } else if (typeof value === "number") {
    if (!Number.isSafeInteger(value)) {
      throw new Error(`CBOR integers only (got ${value})`);
    }
    if (value >= 0) {
      writeHead(out, 0, value);
    } else {
      writeHead(out, 1, -value - 1);
    }
  } else if (typeof value === "string") {
    const bytes = new TextEncoder().encode(value);
    writeHead(out, 3, bytes.length);
    for (const b of bytes) out.push(b);
  } else if (value instanceof Uint8Array) {
    writeHead(out, 2, value.length);
    for (const b of value) out.push(b);
  } else if (Array.isArray(value)) {
    writeHead(out, 4, value.length);
    for (const item of value) writeValue(out, item);
  } else if (value instanceof Map) {
    writeHead(out, 5, value.size);
    for (const [k, v] of value) {
      writeValue(out, k);
      writeValue(out, v);
    }
  } else {
    const entries = Object.entries(value);
    writeHead(out, 5, entries.length);
    for (const [k, v] of entries) {
      writeValue(out, k);
      writeValue(out, v);
    }
  }
}

class Reader {
  constructor(
    private readonly bytes: Uint8Array,
    public pos = 0,
  ) {}

  byte(): number {
    if (this.pos >= this.bytes.length) throw new Error("CBOR: unexpected end of input");
    return this.bytes[this.pos++];
  }

  take(n: number): Uint8Array {
    if (this.pos + n > this.bytes.length) throw new Error("CBOR: unexpected end of input");
    const slice = this.bytes.slice(this.pos, this.pos + n);
    this.pos += n;
    return slice;
  }

  length(info: number): number {
    if (info < 24) return info;
    if (info === 24) return this.byte();
    if (info === 25) return (this.byte() << 8) | this.byte();
    if (info === 26) {
      return ((this.byte() << 24) >>> 0) + (this.byte() << 16) + (this.byte() << 8) + this.byte();
    }
    if (info === 27) {
      let hi = 0;
      for (let i = 0; i < 4; i++) hi = hi * 256 + this.byte();
      let lo = 0;
      for (let i = 0; i < 4; i++) lo = lo * 256 + this.byte();
      const n = hi * 0x100000000 + lo;
      if (!Number.isSafeInteger(n)) throw new Error("CBOR: 64-bit value out of safe range");
      return n;
    }
    throw new Error(`CBOR: unsupported additional info ${info}`);
  }
}

export function cborDecode(bytes: Uint8Array): CborValue {
  const reader = new Reader(bytes);
  const value = readValue(reader);
  if (reader.pos !== bytes.length) throw new Error("CBOR: trailing bytes");
  return value;
}

function readValue(r: Reader): CborValue {
  const head = r.byte();
  const major = head >> 5;
  const info = head & 0x1f;
  switch (major) {
    case 0:
      return r.length(info);
    case 1:
      return -r.length(info) - 1;
    case 2:
      return r.take(r.length(info));
    case 3:
      return new TextDecoder().decode(r.take(r.length(info)));
    case 4: {
      const n = r.length(info);
      const arr: CborValue[] = [];
      for (let i = 0; i < n; i++) arr.push(readValue(r));
      return arr;
    }
    case 5: {
      const n = r.length(info);
      const map = new Map<CborValue, CborValue>();
      for (let i = 0; i < n; i++) {
        const k = readValue(r);
        map.set(k, readValue(r));
      }
      return map;
    }
    case 7:
      if (info === 20) return false;
      if (info === 21) return true;
      if (info === 22) return null;
      throw new Error(`CBOR: unsupported simple value ${info}`);
    default:
      throw new Error(`CBOR: unsupported major type ${major}`);
  }
}

/// Look up a string key in a decoded CBOR map (decoded string keys are
/// primitives, so Map.get works directly).
export function mapGet(value: CborValue, key: string): CborValue | undefined {
  return value instanceof Map ? value.get(key) : undefined;
}

/// Decode a serde externally-tagged enum value: either a bare variant name
/// (unit variant) or a single-entry map {variant: fields}.
export function enumVariant(value: CborValue): { variant: string; fields: CborValue | null } {
  if (typeof value === "string") return { variant: value, fields: null };
  if (value instanceof Map && value.size === 1) {
    const [variant, fields] = value.entries().next().value as [CborValue, CborValue];
    if (typeof variant === "string") return { variant, fields };
  }
  throw new Error("CBOR value is not an enum");
}
