// The app's data layer behind one interface: RealBackend speaks to the local
// node (subscriptions to the registry, all 16 tiles, and the chat room;
// signing via the identity delegate), MockBackend (?mock=1) serves seeded
// in-memory state for the offline Playwright tier and exposes injection hooks
// so tests can simulate remote peers.

import { ContractKey } from "@freenetorg/freenet-stdlib";
import { contractKeyFromId, FreenetClient } from "./freenet-api";
import { GhostkeyOutcome, proveGhostkey } from "./ghostkey";
import { IdentityClient } from "./identity";
import { leadingZeroBits, powDigest } from "./pow";
import {
  applyRegistryDelta,
  ChatStateJs,
  decodeChatDelta,
  decodeChatState,
  decodeRegistryState,
  decodeTileDelta,
  decodeTileState,
  hex,
  looksLikeFullState,
  Placement,
  RegistryStateJs,
  Tier,
  TILE_SIZE,
  TILES_PER_SIDE,
  TileStateJs,
} from "./state";

export type ConnectionStatus = "connecting" | "connected" | "reconnecting";

export interface BackendEvents {
  onConnection(status: ConnectionStatus): void;
  onRegistry(): void;
  /// `arrivals` carries the placements a subscription delta just delivered
  /// (absent for full-state merges and local optimistic applies), so the UI
  /// can animate remote arrivals.
  onTile(tileX: number, tileY: number, arrivals?: Placement[]): void;
  onChat(): void;
  /// User-facing, recoverable problem (send failed, node unreachable, ...).
  onError(message: string): void;
}

export interface GhostkeyProof {
  scopedPayload: Uint8Array;
  signature: Uint8Array;
  certificatePem: string;
}

export interface Backend {
  registry: RegistryStateJs;
  tiles: TileStateJs[]; // indexed tileX * TILES_PER_SIDE + tileY
  chat: ChatStateJs;
  start(): Promise<void>;
  /// Hex of our verifying key; available after start() resolves.
  myAuthorHex(): string;
  myTier(): Tier | null;
  /// admitted_ts of our registry record, for the ghost key upgrade flow
  /// (the upgrade record must pre-date it to win the earliest-wins merge).
  myAdmittedTs(): number | null;
  admissionChallenge(): Promise<{ bytes: Uint8Array; difficultyBits: number }>;
  admitPow(nonce: number, nickname: string | null): Promise<void>;
  requestGhostkey(challenge: Uint8Array): Promise<GhostkeyOutcome>;
  admitGhostkey(proof: GhostkeyProof, nickname: string | null, admittedTs?: number): Promise<void>;
  placePixel(globalX: number, globalY: number, color: number): Promise<void>;
  sendChat(content: string): Promise<void>;
  setNickname(name: string): Promise<void>;
}

export function tileIndex(tileX: number, tileY: number): number {
  return tileX * TILES_PER_SIDE + tileY;
}

/// How long after a fresh admission failed updates keep being retried: the
/// admission record is a rarely-changing field and can lag on other peers for
/// ~5 min (plan.md risks), making valid placements look rejected.
const ADMISSION_LAG_RETRY_MS = 5 * 60_000;
const UPDATE_RETRY_INTERVAL_MS = 5_000;

function nowTs(): number {
  return Math.floor(Date.now() / 1000);
}

function emptyTiles(): TileStateJs[] {
  return Array.from({ length: TILES_PER_SIDE * TILES_PER_SIDE }, () => new TileStateJs());
}

// ---------------------------------------------------------------------------
// Real backend
// ---------------------------------------------------------------------------

interface TileConfig {
  x: number;
  y: number;
  id: string;
  params: number[];
}

export interface LegacyIds {
  registry: string[];
  chat: string[];
  tiles: { x: number; y: number; ids: string[] }[];
}

export class RealBackend implements Backend {
  registry = new RegistryStateJs();
  tiles = emptyTiles();
  chat = new ChatStateJs();

  private client!: FreenetClient;
  private identity!: IdentityClient;
  private myVk: Uint8Array = new Uint8Array();
  private chatSeq = { ts: 0, seq: 0 };
  private admittedAt: number | null = null;
  private disposed = false;
  /// instance-id hex -> route for subscription notifications.
  private routes = new Map<
    string,
    { kind: "registry" } | { kind: "chat" } | { kind: "tile"; x: number; y: number }
  >();

  constructor(
    private readonly events: BackendEvents,
    private readonly config: {
      registryId: string;
      registryParams: Uint8Array;
      tiles: TileConfig[];
      chatId: string;
      chatParams: Uint8Array;
      identityDelegate: { keyBytes: number[]; codeHashBytes: number[] };
      ghostkeysDelegate: { keyBytes: number[]; codeHashBytes: number[] };
      legacyIds: LegacyIds;
    },
  ) {}

  async start(): Promise<void> {
    this.events.onConnection("connecting");
    await this.connect();
  }

  private connect(): Promise<void> {
    return new Promise((resolve, reject) => {
      let settled = false;
      this.client = new FreenetClient({
        onOpen: () => {
          settled = true;
          void this.onOpen().then(resolve, reject);
        },
        onClose: (code, reason) => {
          if (!settled) {
            settled = true;
            reject(new Error(`connection failed: ${reason || code}`));
            return;
          }
          this.scheduleReconnect();
        },
        onNotification: (instanceHex, update) => this.onNotification(instanceHex, update),
      });
      this.identity = new IdentityClient(this.client, this.config.identityDelegate);
    });
  }

  private async onOpen(): Promise<void> {
    this.myVk = await this.identity.getIdentity();
    await this.syncAll();
    this.events.onConnection("connected");
  }

  /// The SDK does not auto-reconnect; recreate the socket and re-sync. Full
  /// state gets re-merged, which the CRDTs make idempotent.
  private scheduleReconnect(): void {
    if (this.disposed) return;
    this.events.onConnection("reconnecting");
    setTimeout(() => {
      void this.connect().catch(() => this.scheduleReconnect());
    }, 2000);
  }

  private key(kind: "registry" | "chat" | "tile", tile?: TileConfig): ContractKey {
    const id =
      kind === "registry" ? this.config.registryId : kind === "chat" ? this.config.chatId : tile!.id;
    return contractKeyFromId(id);
  }

  /// Fetch a contract's state; when the current instance is still empty and a
  /// previous release's instance holds state, the backward migration probe
  /// carries it forward first (upgrade-and-migration.md): GET the newest
  /// legacy instance, fold its bytes into the current key with a full-state
  /// update (the contract re-validates every byte), and use them locally.
  /// Gated on "destination empty" so a stale source never clobbers newer
  /// data; merge semantics make re-runs idempotent; the legacy instance is
  /// left untouched.
  private async getStateWithLegacyProbe(
    label: string,
    key: ContractKey,
    legacyIds: string[],
    isEmpty: (bytes: Uint8Array) => boolean,
  ): Promise<Uint8Array> {
    const current = await this.client.getState(key);
    if (!isEmpty(current) || legacyIds.length === 0) return current;
    for (const legacyId of legacyIds) {
      try {
        const old = await this.client.getState(contractKeyFromId(legacyId));
        if (isEmpty(old)) continue;
        await this.client.updateWithState(key, old);
        console.info(`freeplace migration: carried ${label} state forward from ${legacyId}`);
        return old;
      } catch (err) {
        console.info(`freeplace migration: legacy ${label} ${legacyId} unreachable: ${err}`);
      }
    }
    return current;
  }

  private async syncAll(): Promise<void> {
    const legacy = this.config.legacyIds;
    const registryKey = this.key("registry");
    this.routes.set(hex(registryKey.bytes()), { kind: "registry" });
    this.registry = decodeRegistryState(
      await this.getStateWithLegacyProbe(
        "registry",
        registryKey,
        legacy.registry,
        (bytes) => decodeRegistryState(bytes).identities.size === 0,
      ),
    );
    await this.client.subscribe(registryKey);
    this.events.onRegistry();

    const chatKey = this.key("chat");
    this.routes.set(hex(chatKey.bytes()), { kind: "chat" });
    const chatState = decodeChatState(
      await this.getStateWithLegacyProbe(
        "chat",
        chatKey,
        legacy.chat,
        (bytes) => decodeChatState(bytes).messages.size === 0,
      ),
    );
    for (const message of chatState.messages.values()) this.chat.insert(message);
    await this.client.subscribe(chatKey);
    this.events.onChat();

    for (const tile of this.config.tiles) {
      const tileKey = this.key("tile", tile);
      this.routes.set(hex(tileKey.bytes()), { kind: "tile", x: tile.x, y: tile.y });
      const legacyTileIds =
        legacy.tiles.find((t) => t.x === tile.x && t.y === tile.y)?.ids ?? [];
      const state = decodeTileState(
        await this.getStateWithLegacyProbe(
          `tile(${tile.x},${tile.y})`,
          tileKey,
          legacyTileIds,
          (bytes) => decodeTileState(bytes).placements.size === 0,
        ),
      );
      const target = this.tiles[tileIndex(tile.x, tile.y)];
      for (const log of state.placements.values()) {
        for (const placement of log.values()) target.insert(placement);
      }
      await this.client.subscribe(tileKey);
      this.events.onTile(tile.x, tile.y);
    }
  }

  private onNotification(instanceHex: string, update: { state?: Uint8Array; delta?: Uint8Array }): void {
    const route = this.routes.get(instanceHex);
    if (!route) return;
    const bytes = update.delta ?? update.state;
    if (!bytes || bytes.length === 0) return;
    switch (route.kind) {
      case "registry":
        if (looksLikeFullState(bytes, "identities")) {
          this.registry = decodeRegistryState(bytes);
        } else {
          applyRegistryDelta(this.registry, bytes);
        }
        this.events.onRegistry();
        break;
      case "chat": {
        const messages = looksLikeFullState(bytes, "messages")
          ? [...decodeChatState(bytes).messages.values()]
          : decodeChatDelta(bytes);
        this.chat.applyDelta(messages);
        this.events.onChat();
        break;
      }
      case "tile": {
        const tile = this.tiles[tileIndex(route.x, route.y)];
        let arrivals: Placement[] | undefined;
        if (looksLikeFullState(bytes, "placements")) {
          for (const log of decodeTileState(bytes).placements.values()) {
            for (const placement of log.values()) tile.insert(placement);
          }
        } else {
          arrivals = decodeTileDelta(bytes);
          tile.applyDelta(arrivals);
        }
        this.events.onTile(route.x, route.y, arrivals);
        break;
      }
    }
  }

  myAuthorHex(): string {
    return hex(this.myVk);
  }

  myTier(): Tier | null {
    return this.registry.tierOf(this.myAuthorHex());
  }

  myAdmittedTs(): number | null {
    return this.registry.identities.get(this.myAuthorHex())?.admittedTs ?? null;
  }

  async admissionChallenge(): Promise<{ bytes: Uint8Array; difficultyBits: number }> {
    return this.identity.admissionChallenge(this.config.registryParams);
  }

  async admitPow(nonce: number, nickname: string | null): Promise<void> {
    const signed = await this.identity.signPowAdmission(
      this.config.registryParams,
      nonce,
      nowTs(),
      nickname,
    );
    await this.sendRegistryDelta(signed.delta);
  }

  async requestGhostkey(challenge: Uint8Array): Promise<GhostkeyOutcome> {
    return proveGhostkey(this.client, this.config.ghostkeysDelegate, challenge);
  }

  async admitGhostkey(
    proof: GhostkeyProof,
    nickname: string | null,
    admittedTs?: number,
  ): Promise<void> {
    const signed = await this.identity.signGhostkeyAdmission(
      this.config.registryParams,
      proof,
      admittedTs ?? nowTs(),
      nickname,
    );
    await this.sendRegistryDelta(signed.delta);
  }

  private async sendRegistryDelta(delta: Uint8Array): Promise<void> {
    await this.client.updateWithDelta(this.key("registry"), delta);
    this.admittedAt = Date.now();
    // Optimistic: the subscription notification confirms, but a fresh
    // admission must unlock the UI immediately.
    applyRegistryDelta(this.registry, delta);
    this.events.onRegistry();
  }

  async placePixel(globalX: number, globalY: number, color: number): Promise<void> {
    const tileX = Math.floor(globalX / TILE_SIZE);
    const tileY = Math.floor(globalY / TILE_SIZE);
    const tile = this.config.tiles.find((t) => t.x === tileX && t.y === tileY);
    if (!tile) throw new Error(`no tile contract configured for (${tileX}, ${tileY})`);
    const coord = (globalY % TILE_SIZE) * TILE_SIZE + (globalX % TILE_SIZE);
    const signed = await this.identity.signPlacement(
      Uint8Array.from(tile.params),
      coord,
      color,
      nowTs(),
    );
    // Optimistic local apply; the subscription notification reconciles.
    this.tiles[tileIndex(tileX, tileY)].applyDelta(decodeTileDelta(signed.delta));
    this.events.onTile(tileX, tileY);
    await this.updateWithRetry(this.key("tile", tile), signed.delta);
  }

  async sendChat(content: string): Promise<void> {
    const ts = nowTs();
    if (ts === this.chatSeq.ts) {
      this.chatSeq.seq += 1;
    } else {
      this.chatSeq = { ts, seq: 0 };
    }
    const signed = await this.identity.signChatMessage(
      this.config.chatParams,
      content,
      ts,
      this.chatSeq.seq,
    );
    this.chat.applyDelta(decodeChatDelta(signed.delta));
    this.events.onChat();
    await this.updateWithRetry(this.key("chat"), signed.delta);
  }

  /// Nickname edits ride the registry's monotonic version counter. Unlike an
  /// admission this must not touch admittedAt: editing a nickname does not
  /// restart the admission-lag retry window.
  async setNickname(name: string): Promise<void> {
    const record = this.registry.identities.get(this.myAuthorHex());
    const version = (record?.nicknameVersion ?? 0) + 1;
    const signed = await this.identity.signNickname(this.config.registryParams, name, version);
    // Optimistic; the subscription notification reconciles.
    applyRegistryDelta(this.registry, signed.delta);
    this.events.onRegistry();
    await this.updateWithRetry(this.key("registry"), signed.delta);
  }

  /// A fresh admission can take minutes to become visible to every peer's
  /// validate pass (rarely-changing-field lag, plan.md risks); within the lag
  /// window after admitting, keep retrying instead of surfacing a spurious
  /// rejection. Outside it, three quick attempts cover transient failures.
  private async updateWithRetry(key: ContractKey, delta: Uint8Array): Promise<void> {
    const lagDeadline = this.admittedAt === null ? 0 : this.admittedAt + ADMISSION_LAG_RETRY_MS;
    let lastError: unknown;
    for (let attempt = 0; ; attempt++) {
      try {
        await this.client.updateWithDelta(key, delta);
        return;
      } catch (err) {
        lastError = err;
        if (attempt >= 2 && Date.now() >= lagDeadline) break;
        await new Promise((r) => setTimeout(r, UPDATE_RETRY_INTERVAL_MS));
      }
    }
    throw lastError instanceof Error ? lastError : new Error(String(lastError));
  }
}

// ---------------------------------------------------------------------------
// Mock backend (offline tier)
// ---------------------------------------------------------------------------

const MOCK_DIFFICULTY_BITS_DEFAULT = 12;

function mockAuthor(fill: number): Uint8Array {
  return new Uint8Array(32).fill(fill);
}

function mockSignature(): Uint8Array {
  const sig = new Uint8Array(64);
  crypto.getRandomValues(sig);
  return sig;
}

export interface MockHooks {
  injectPlacement(globalX: number, globalY: number, color: number): void;
  injectChat(content: string): void;
  /// Resumes an admission challenge held by ?holdpow=1.
  releasePow(): void;
}

export interface MockOptions {
  /// Start with our identity already admitted (skips onboarding).
  admitted?: boolean;
  /// PoW difficulty for the mock challenge.
  powBits?: number;
  /// Hold the admission challenge until the test calls releasePow(), making
  /// onboarding interactions deterministic instead of racing the grind.
  holdPow?: boolean;
  /// Make the ghost key path succeed with a fake proof.
  ghostkeySucceeds?: boolean;
}

export class MockBackend implements Backend {
  registry = new RegistryStateJs();
  tiles = emptyTiles();
  chat = new ChatStateJs();

  private myVk = mockAuthor(0xaa);
  private remoteVk = mockAuthor(0xee);
  private challenge: Uint8Array | null = null;
  private chatSeq = 0;
  private releaseHold: (() => void) | null = null;
  private readonly difficultyBits: number;

  constructor(
    private readonly events: BackendEvents,
    private readonly options: MockOptions,
  ) {
    this.difficultyBits = options.powBits ?? MOCK_DIFFICULTY_BITS_DEFAULT;
  }

  async start(): Promise<void> {
    this.events.onConnection("connecting");
    this.seed();
    if (this.options.admitted) {
      this.registry.insertAdmission(hex(this.myVk), {
        tier: "Pow",
        admittedTs: nowTs() - 3600,
        nickname: "you",
        nicknameVersion: 1,
      });
    }
    (window as unknown as { __freeplaceMock: MockHooks }).__freeplaceMock = {
      injectPlacement: (globalX, globalY, color) => {
        this.applyRemotePlacement(this.remoteVk, globalX, globalY, color, nowTs(), true);
      },
      injectChat: (content) => {
        this.chat.insert({
          content,
          ts: nowTs(),
          seq: this.chatSeq++,
          author: this.remoteVk,
          signature: mockSignature(),
        });
        this.events.onChat();
      },
      releasePow: () => {
        this.releaseHold?.();
        this.releaseHold = null;
      },
    };
    this.events.onConnection("connected");
    this.events.onRegistry();
    this.events.onChat();
  }

  /// Deterministic seed content: three admitted authors, a marker pixel at
  /// (10, 10), a colour strip near the canvas centre spanning two tiles, and
  /// a pixel in tile (2, 1); two chat messages.
  private seed(): void {
    const alice = mockAuthor(0x01);
    const bob = mockAuthor(0x02);
    const carol = mockAuthor(0x03);
    for (const [vk, nickname] of [
      [alice, "alice"],
      [bob, "bob"],
      [carol, "carol"],
    ] as const) {
      this.registry.insertAdmission(hex(vk), {
        tier: "Pow",
        admittedTs: nowTs() - 48 * 3600,
        nickname,
        nicknameVersion: 1,
      });
    }
    this.registry.insertAdmission(hex(this.remoteVk), {
      tier: "Ghostkey",
      admittedTs: nowTs() - 48 * 3600,
      nickname: "eve",
      nicknameVersion: 1,
    });

    const base = nowTs() - 24 * 3600;
    // Marker pixel asserted by the offline suite: global (10, 10), color 5.
    this.applyRemotePlacement(alice, 10, 10, 5, base);
    // A strip crossing the tile (1,1)/(2,1) boundary at y = 300.
    for (let i = 0; i < 8; i++) {
      this.applyRemotePlacement(bob, 508 + i, 300, i + 4, base + (i + 1) * 130);
    }
    // Lone pixel deep in tile (3, 2).
    this.applyRemotePlacement(carol, 900, 700, 11, base);
    // Recent placements so the activity chip is non-zero offline: exactly two
    // authors and two valid placements within the last hour.
    this.applyRemotePlacement(alice, 40, 40, 2, nowTs() - 600);
    this.applyRemotePlacement(carol, 60, 40, 7, nowTs() - 300);

    this.chat.insert({
      content: "welcome to FreePlace",
      ts: base,
      seq: 0,
      author: alice,
      signature: mockSignature(),
    });
    this.chat.insert({
      content: "the canvas is alive",
      ts: base + 60,
      seq: 0,
      author: bob,
      signature: mockSignature(),
    });
  }

  /// `asArrival` mirrors the real backend's delta notifications: injected
  /// placements report themselves, seed data and our own placements do not.
  private applyRemotePlacement(
    author: Uint8Array,
    globalX: number,
    globalY: number,
    color: number,
    ts: number,
    asArrival = false,
  ): void {
    const tileX = Math.floor(globalX / TILE_SIZE);
    const tileY = Math.floor(globalY / TILE_SIZE);
    const placement: Placement = {
      coord: (globalY % TILE_SIZE) * TILE_SIZE + (globalX % TILE_SIZE),
      color,
      ts,
      author,
      signature: mockSignature(),
    };
    this.tiles[tileIndex(tileX, tileY)].insert(placement);
    this.events.onTile(tileX, tileY, asArrival ? [placement] : undefined);
  }

  myAuthorHex(): string {
    return hex(this.myVk);
  }

  myTier(): Tier | null {
    return this.registry.tierOf(this.myAuthorHex());
  }

  myAdmittedTs(): number | null {
    return this.registry.identities.get(this.myAuthorHex())?.admittedTs ?? null;
  }

  async admissionChallenge(): Promise<{ bytes: Uint8Array; difficultyBits: number }> {
    if (this.options.holdPow) {
      await new Promise<void>((resolve) => {
        this.releaseHold = resolve;
      });
    }
    const bytes = new Uint8Array(16);
    crypto.getRandomValues(bytes);
    this.challenge = bytes;
    return { bytes, difficultyBits: this.difficultyBits };
  }

  async admitPow(nonce: number, nickname: string | null): Promise<void> {
    if (!this.challenge || leadingZeroBits(powDigest(this.challenge, nonce)) < this.difficultyBits) {
      throw new Error("mock: invalid proof of work");
    }
    this.registry.insertAdmission(this.myAuthorHex(), {
      tier: "Pow",
      admittedTs: nowTs(),
      nickname,
      nicknameVersion: nickname === null ? 0 : 1,
    });
    this.events.onRegistry();
  }

  async requestGhostkey(): Promise<GhostkeyOutcome> {
    if (this.options.ghostkeySucceeds) {
      return {
        kind: "signature",
        scopedPayload: new Uint8Array(8),
        signature: mockSignature(),
        certificatePem: "mock certificate",
      };
    }
    return { kind: "no-identity", detail: "no ghost key stored; see freenet.org/ghostkey" };
  }

  async admitGhostkey(
    _proof: GhostkeyProof,
    nickname: string | null,
    admittedTs?: number,
  ): Promise<void> {
    this.registry.insertAdmission(this.myAuthorHex(), {
      tier: "Ghostkey",
      admittedTs: admittedTs ?? nowTs(),
      nickname,
      nicknameVersion: nickname === null ? 0 : 1,
    });
    this.events.onRegistry();
  }

  async placePixel(globalX: number, globalY: number, color: number): Promise<void> {
    this.applyRemotePlacement(this.myVk, globalX, globalY, color, nowTs());
  }

  async sendChat(content: string): Promise<void> {
    this.chat.insert({
      content,
      ts: nowTs(),
      seq: this.chatSeq++,
      author: this.myVk,
      signature: mockSignature(),
    });
    this.events.onChat();
  }

  /// Mirrors the real backend's sign-nickname path against the mock registry.
  async setNickname(name: string): Promise<void> {
    const record = this.registry.identities.get(this.myAuthorHex());
    if (!record) throw new Error("mock: not admitted");
    this.registry.applyNickname(this.myAuthorHex(), name, record.nicknameVersion + 1);
    this.events.onRegistry();
  }
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

export function createBackend(events: BackendEvents): Backend {
  const query = new URLSearchParams(location.search);
  if (query.get("mock") === "1") {
    return new MockBackend(events, {
      admitted: query.get("admitted") === "1",
      powBits: Number(query.get("powbits")) || undefined,
      holdPow: query.get("holdpow") === "1",
      ghostkeySucceeds: query.get("ghostkey") === "yes",
    });
  }
  return new RealBackend(events, {
    registryId: __REGISTRY_CONTRACT_ID__,
    registryParams: Uint8Array.from(__REGISTRY_PARAMS_BYTES__),
    tiles: __TILES_JSON__,
    chatId: __CHAT_CONTRACT_ID__,
    chatParams: Uint8Array.from(__CHAT_PARAMS_BYTES__),
    identityDelegate: {
      keyBytes: __IDENTITY_DELEGATE_KEY_BYTES__,
      codeHashBytes: __IDENTITY_DELEGATE_CODE_HASH_BYTES__,
    },
    ghostkeysDelegate: {
      keyBytes: __GHOSTKEYS_DELEGATE_KEY_BYTES__,
      codeHashBytes: __GHOSTKEYS_DELEGATE_CODE_HASH_BYTES__,
    },
    legacyIds: __LEGACY_IDS_JSON__,
  });
}
