// The app's data layer behind one interface: RealBackend speaks to the local
// node (subscriptions to the registry, all 16 tiles, and the chat room;
// signing via the identity delegate), MockBackend (?mock=1) serves seeded
// in-memory state for the offline Playwright tier and exposes injection hooks
// so tests can simulate remote peers.

import { ContractContainer, ContractKey } from "@freenetorg/freenet-stdlib";
import { base64ToBytes, registerDelegate } from "./delegate-api";
import { IDENTITY_DELEGATE_WASM_B64 } from "./delegate-wasm";
import { contractKeyFromId, FreenetClient } from "./freenet-api";
import {
  GhostkeyOutcome,
  GhostkeyPresence,
  ghostkeyPresence as probeGhostkeyPresence,
  proveGhostkey,
} from "./ghostkey";
import { IdentityClient } from "./identity";
import { mergeChatState, mergeRegistryState, mergeTileState } from "./merge";
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
  /// HasIdentity probe (never prompts): does the user's vault hold any ghost
  /// keys? null when unknowable (no delegate on this node). Offer-only
  /// signal — must never gate requestGhostkey.
  ghostkeyPresence(): Promise<GhostkeyPresence | null>;
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
// The TS SDK's request/response matching is arrival-order FIFO with no
// correlation (freenet-core#5048): an update whose error response is
// mis-routed leaves the promise pending forever. Every attempt gets a hard
// timeout so retry loops keep moving and failures surface instead of
// freezing the UI.
const UPDATE_ATTEMPT_TIMEOUT_MS = 20_000;
// On a fresh node the FIRST GET of a low-traffic contract can stall forever
// node-side while still kicking off the network fetch; an immediate re-GET
// then answers from local state in milliseconds (reproduced live
// 2026-08-02, 4 of 16 tiles). So sync GETs use a short per-attempt timeout
// and a few retries instead of trusting the SDK's single 30s wait.
const SYNC_GET_TIMEOUT_MS = 15_000;
const SYNC_GET_ATTEMPTS = 3;
// Tiles that still fail after that keep retrying in the background so one
// cold tile never blocks the whole app.
const SYNC_RETRY_INTERVAL_MS = 10_000;
/// Steady-state re-pull of all subscribed contracts (see
/// scheduleStateRefresh); bounds how stale a viewer can be when updates
/// arrive at the node without a subscription notification.
const STATE_REFRESH_INTERVAL_MS = 60_000;

/// GET with a per-attempt hard timeout and retries; null when every attempt
/// failed. Mis-routed responses (checkResponseKey) and stalls both land in
/// the catch, and the retry re-issues the request.
export async function getWithRetry(
  channel: Pick<UpdateChannel, "getState">,
  key: ContractKey,
  label: string,
  attempts = SYNC_GET_ATTEMPTS,
  timeoutMs = SYNC_GET_TIMEOUT_MS,
): Promise<Uint8Array | null> {
  for (let attempt = 1; attempt <= attempts; attempt++) {
    try {
      return await withTimeout(channel.getState(key), timeoutMs, `${label} fetch`);
    } catch (err) {
      console.info(`freeplace sync: ${label} fetch attempt ${attempt}/${attempts} failed: ${err}`);
    }
  }
  return null;
}

/// Reject after `ms` so one lost response cannot hang a retry loop.
export function withTimeout<T>(promise: Promise<T>, ms: number, label: string): Promise<T> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`${label} timed out after ${ms}ms`)), ms);
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (err: unknown) => {
        clearTimeout(timer);
        reject(err instanceof Error ? err : new Error(String(err)));
      },
    );
  });
}

/// Merges a signed delta into full contract state bytes (see merge.ts).
export type StateMerge = (state: Uint8Array, delta: Uint8Array) => Uint8Array;

/// What updateOrPut needs from the connection layer (FreenetClient implements
/// it; tests substitute fakes).
export interface UpdateChannel {
  updateWithDelta(key: ContractKey, delta: Uint8Array): Promise<void>;
  getState(key: ContractKey): Promise<Uint8Array>;
  getStateWithContract(
    key: ContractKey,
  ): Promise<{ state: Uint8Array; contract: ContractContainer }>;
  putState(contract: ContractContainer, state: Uint8Array): Promise<void>;
}

/// Per-connection caches for the PUT fallback: fetched contract containers,
/// and the keys whose UPDATE path already failed once (those go PUT-first, so
/// e.g. pixel placements do not wait out a doomed 20s UPDATE every time).
export interface PutFallbackCaches {
  containers: Map<string, ContractContainer>;
  putFirst: Set<string>;
}

/// One delivery attempt. UPDATE is the cheap, semantically-right primitive,
/// but on the real network a low-traffic contract can drop out of the update
/// mesh: every UPDATE fails with "missing contract" while GET and PUT keep
/// working (freenet-core#5069; confirmed live 2026-08-01, even seconds after
/// a successful re-PUT). The fallback fetches the current state (plus the
/// contract container, cached), merges the delta locally, and re-PUTs the
/// result; peers re-validate and CRDT-merge it, so this is safe and
/// idempotent.
export async function updateOrPut(
  channel: UpdateChannel,
  key: ContractKey,
  delta: Uint8Array,
  merge: StateMerge,
  caches: PutFallbackCaches,
): Promise<void> {
  const keyHex = hex(key.bytes());
  const doUpdate = () =>
    withTimeout(channel.updateWithDelta(key, delta), UPDATE_ATTEMPT_TIMEOUT_MS, "contract update");
  const doPut = async () => {
    let container = caches.containers.get(keyHex);
    let state: Uint8Array;
    if (container === undefined) {
      const got = await withTimeout(
        channel.getStateWithContract(key),
        UPDATE_ATTEMPT_TIMEOUT_MS,
        "contract fetch",
      );
      container = got.contract;
      caches.containers.set(keyHex, container);
      state = got.state;
    } else {
      state = await withTimeout(channel.getState(key), UPDATE_ATTEMPT_TIMEOUT_MS, "state fetch");
    }
    await withTimeout(
      channel.putState(container, merge(state, delta)),
      UPDATE_ATTEMPT_TIMEOUT_MS,
      "contract put",
    );
    if (!caches.putFirst.has(keyHex)) {
      // A PUT lands but does not broadcast to remote subscribers, so peers
      // see this write only via the periodic heal — worth a field record.
      console.info(
        `freeplace update: ${keyHex.slice(0, 8)} UPDATE failed, using PUT fallback ` +
          "(off the update mesh; remote peers rely on the background heal)",
      );
    }
    caches.putFirst.add(keyHex);
  };
  if (caches.putFirst.has(keyHex)) {
    try {
      await doPut();
    } catch {
      await doUpdate();
    }
    return;
  }
  try {
    await doUpdate();
  } catch {
    await doPut();
  }
}

/// Fold a decoded full tile state into the render state, BOTH layers: live
/// placements via insert (displacement bakes, as in tile.rs), baked entries
/// straight into the baked layer (keep-newest-K idempotent merge). Copying
/// only the live log made every baked pixel invisible to freshly-loading
/// clients (latent since the baked layer shipped, found 2026-08-02).
export function mergeTileInto(target: TileStateJs, decoded: TileStateJs): void {
  for (const log of decoded.placements.values()) {
    for (const placement of log.values()) target.insert(placement);
  }
  for (const log of decoded.baked.values()) {
    for (const placement of log.values()) target.bake(placement);
  }
}

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
  /// Previous releases' web-container ids (newest first), probed by the
  /// identity delegate's AdoptLegacyOrigin so identities survive re-keys.
  /// Optional: absent in legacy_ids.json files written before this shipped.
  webapps?: string[];
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
  private putFallback: PutFallbackCaches = { containers: new Map(), putFirst: new Set() };
  private delegateRegistered = false;
  private pendingChat = false;
  private pendingTiles: TileConfig[] = [];
  private syncRetryTimer: number | null = null;
  private disposed = false;
  private refreshTimer: number | null = null;
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
      /// Raw delegate WASM (base64, "" when the build had none): registered
      /// on the node at startup, because delegates never propagate over the
      /// network — a node that never ran `fdev publish delegate` locally
      /// would otherwise ignore every identity request.
      identityDelegateWasmB64: string;
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
    await this.ensureIdentityDelegate();
    this.myVk = await this.identity.adoptOrGetIdentity(this.config.legacyIds.webapps ?? []);
    await this.syncAll();
    this.events.onConnection("connected");
    void this.processLegacyProbes();
    this.scheduleStateRefresh();
  }

  /// Subscription notifications do not fire for node-side background heals,
  /// and peers whose writes ride the PUT fallback (off the update mesh,
  /// freenet-core#5069 — every placement on a fresh tile instance) never
  /// broadcast at all. Re-pulling every subscribed contract from the local
  /// node on a slow timer bounds viewer staleness at the refresh interval
  /// where it was previously unbounded; GETs of subscribed contracts answer
  /// from local node state in ~60 ms, so the steady-state cost is trivial.
  private scheduleStateRefresh(): void {
    if (this.disposed || this.refreshTimer !== null) return;
    this.refreshTimer = window.setTimeout(() => {
      this.refreshTimer = null;
      void this.refreshAll().finally(() => this.scheduleStateRefresh());
    }, STATE_REFRESH_INTERVAL_MS);
  }

  private async refreshAll(): Promise<void> {
    if (this.disposed) return;
    const registryBytes = await getWithRetry(this.client, this.key("registry"), "registry refresh", 1);
    if (registryBytes !== null) {
      for (const [author, record] of decodeRegistryState(registryBytes).identities) {
        this.registry.insertAdmission(author, record);
      }
      this.events.onRegistry();
    }
    const chatBytes = await getWithRetry(this.client, this.key("chat"), "chat refresh", 1);
    if (chatBytes !== null) {
      for (const message of decodeChatState(chatBytes).messages.values()) this.chat.insert(message);
      this.events.onChat();
    }
    for (const tile of this.config.tiles) {
      if (this.disposed) return;
      const bytes = await getWithRetry(
        this.client,
        this.key("tile", tile),
        `tile(${tile.x},${tile.y}) refresh`,
        1,
      );
      if (bytes !== null) {
        mergeTileInto(this.tiles[tileIndex(tile.x, tile.y)], decodeTileState(bytes));
        this.events.onTile(tile.x, tile.y);
      }
    }
  }

  /// Registration is idempotent node-side and skipped after the first
  /// success this page load. Failure is tolerated: a node provisioned by
  /// `fdev publish delegate` answers anyway, and a genuinely missing
  /// delegate surfaces downstream as the existing timeout error.
  private async ensureIdentityDelegate(): Promise<void> {
    if (this.delegateRegistered || this.config.identityDelegateWasmB64 === "") return;
    try {
      await registerDelegate(
        this.client,
        this.config.identityDelegate,
        base64ToBytes(this.config.identityDelegateWasmB64),
      );
      this.delegateRegistered = true;
    } catch (err) {
      console.info(`freeplace identity: delegate registration failed: ${err}`);
    }
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

  /// Legacy fold work queued off the connect critical path, keyed by label.
  private legacyProbes = new Map<
    string,
    {
      key: ContractKey;
      legacyIds: string[];
      isEmpty: (bytes: Uint8Array) => boolean;
      apply: (bytes: Uint8Array) => void;
    }
  >();
  private probing = false;

  /// Fetch a contract's state; when the current instance is still empty and a
  /// previous release's instance holds state, QUEUE the backward migration
  /// probe (upgrade-and-migration.md) instead of running it inline: a fold is
  /// a network write that can take 20 s+ per instance on the real network, and
  /// 16 inline folds kept conn-status at "connecting" for minutes (found live
  /// 2026-08-02, first tile re-key release). The queue drains after
  /// "connected"; a successful fold applies the carried bytes via `apply`.
  private async getStateWithLegacyProbe(
    label: string,
    key: ContractKey,
    legacyIds: string[],
    isEmpty: (bytes: Uint8Array) => boolean,
    apply: (bytes: Uint8Array) => void,
    attempts?: number,
  ): Promise<Uint8Array | null> {
    const current = await getWithRetry(this.client, key, label, attempts);
    if (current === null) return null;
    if (!isEmpty(current) || legacyIds.length === 0) return current;
    if (!this.legacyProbes.has(label)) this.legacyProbes.set(label, { key, legacyIds, isEmpty, apply });
    return current;
  }

  /// One probe pass per connect, sequential (each fold is an expensive
  /// network write): GET the newest legacy instance, fold its bytes into the
  /// current key with a full-state update (the contract re-validates every
  /// byte), and apply them locally. Gated on "destination empty" at queue
  /// time so a stale source never clobbers newer data; merge semantics make
  /// re-runs idempotent; the legacy instance is left untouched.
  private async processLegacyProbes(): Promise<void> {
    if (this.probing) return;
    this.probing = true;
    try {
      for (const [label, probe] of [...this.legacyProbes]) {
        this.legacyProbes.delete(label);
        for (const legacyId of probe.legacyIds) {
          try {
            const old = await withTimeout(
              this.client.getState(contractKeyFromId(legacyId)),
              SYNC_GET_TIMEOUT_MS,
              `legacy ${label} fetch`,
            );
            if (probe.isEmpty(old)) continue;
            await withTimeout(
              this.client.updateWithState(probe.key, old),
              UPDATE_ATTEMPT_TIMEOUT_MS,
              `${label} fold-forward`,
            );
            console.info(`freeplace migration: carried ${label} state forward from ${legacyId}`);
            probe.apply(old);
            break;
          } catch (err) {
            console.info(`freeplace migration: legacy ${label} ${legacyId} unreachable: ${err}`);
          }
        }
      }
    } finally {
      this.probing = false;
    }
  }

  /// One cold tile must never block the app: registry failure is fatal (the
  /// admission gate is unusable without it), but chat and tiles that fail
  /// their initial sync are queued and keep retrying in the background while
  /// everything that did load is live.
  private async syncAll(): Promise<void> {
    const legacy = this.config.legacyIds;
    const registryKey = this.key("registry");
    this.routes.set(hex(registryKey.bytes()), { kind: "registry" });
    const registryBytes = await this.getStateWithLegacyProbe(
      "registry",
      registryKey,
      legacy.registry,
      (bytes) => decodeRegistryState(bytes).identities.size === 0,
      (bytes) => {
        for (const [author, record] of decodeRegistryState(bytes).identities) {
          this.registry.insertAdmission(author, record);
        }
        this.events.onRegistry();
      },
    );
    if (registryBytes === null) throw new Error("registry unreachable (all attempts timed out)");
    this.registry = decodeRegistryState(registryBytes);
    await withTimeout(this.client.subscribe(registryKey), SYNC_GET_TIMEOUT_MS, "registry subscribe");
    this.events.onRegistry();

    this.pendingChat = !(await this.syncChat());
    this.pendingTiles = [];
    for (const tile of this.config.tiles) {
      if (!(await this.syncTile(tile))) this.pendingTiles.push(tile);
    }
    if (this.pendingChat || this.pendingTiles.length > 0) {
      console.info(
        `freeplace sync: ${this.pendingTiles.length} tile(s)${this.pendingChat ? " + chat" : ""} still syncing in the background`,
      );
      this.scheduleSyncRetry();
    }
  }

  private async syncChat(attempts?: number): Promise<boolean> {
    const chatKey = this.key("chat");
    this.routes.set(hex(chatKey.bytes()), { kind: "chat" });
    const bytes = await this.getStateWithLegacyProbe(
      "chat",
      chatKey,
      this.config.legacyIds.chat,
      (b) => decodeChatState(b).messages.size === 0,
      (b) => {
        for (const message of decodeChatState(b).messages.values()) this.chat.insert(message);
        this.events.onChat();
      },
      attempts,
    );
    if (bytes === null) return false;
    for (const message of decodeChatState(bytes).messages.values()) this.chat.insert(message);
    this.events.onChat();
    try {
      await withTimeout(this.client.subscribe(chatKey), SYNC_GET_TIMEOUT_MS, "chat subscribe");
    } catch (err) {
      console.info(`freeplace sync: chat subscribe failed: ${err}`);
      return false;
    }
    return true;
  }

  private async syncTile(tile: TileConfig, attempts?: number): Promise<boolean> {
    const tileKey = this.key("tile", tile);
    const label = `tile(${tile.x},${tile.y})`;
    this.routes.set(hex(tileKey.bytes()), { kind: "tile", x: tile.x, y: tile.y });
    const legacyTileIds =
      this.config.legacyIds.tiles.find((t) => t.x === tile.x && t.y === tile.y)?.ids ?? [];
    const bytes = await this.getStateWithLegacyProbe(
      label,
      tileKey,
      legacyTileIds,
      (b) => decodeTileState(b).placements.size === 0,
      (b) => {
        mergeTileInto(this.tiles[tileIndex(tile.x, tile.y)], decodeTileState(b));
        this.events.onTile(tile.x, tile.y);
      },
      attempts,
    );
    if (bytes === null) return false;
    mergeTileInto(this.tiles[tileIndex(tile.x, tile.y)], decodeTileState(bytes));
    this.events.onTile(tile.x, tile.y);
    try {
      await withTimeout(this.client.subscribe(tileKey), SYNC_GET_TIMEOUT_MS, `${label} subscribe`);
    } catch (err) {
      console.info(`freeplace sync: ${label} subscribe failed: ${err}`);
      return false;
    }
    return true;
  }

  /// Single background timer; re-arms itself while anything is pending.
  /// Retries are single-attempt (the stalled first fetch completes in the
  /// background, so the re-GET is answered from local state).
  private scheduleSyncRetry(): void {
    if (this.disposed || this.syncRetryTimer !== null) return;
    if (!this.pendingChat && this.pendingTiles.length === 0) return;
    this.syncRetryTimer = window.setTimeout(() => {
      this.syncRetryTimer = null;
      void this.retryPendingSync();
    }, SYNC_RETRY_INTERVAL_MS);
  }

  private async retryPendingSync(): Promise<void> {
    if (this.disposed) return;
    if (this.pendingChat && (await this.syncChat(1))) this.pendingChat = false;
    const still: TileConfig[] = [];
    for (const tile of this.pendingTiles) {
      if (!(await this.syncTile(tile, 1))) still.push(tile);
    }
    this.pendingTiles = still;
    this.scheduleSyncRetry();
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
          mergeTileInto(tile, decodeTileState(bytes));
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

  async ghostkeyPresence(): Promise<GhostkeyPresence | null> {
    return probeGhostkeyPresence(this.client, this.config.ghostkeysDelegate);
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
    // Patient retries: the admission must land, and each attempt already
    // carries the PUT fallback for a contract dropped from the update mesh.
    await this.updateWithRetry(this.key("registry"), delta, mergeRegistryState, true);
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
    await this.updateWithRetry(this.key("tile", tile), signed.delta, mergeTileState);
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
    await this.updateWithRetry(this.key("chat"), signed.delta, mergeChatState);
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
    await this.updateWithRetry(this.key("registry"), signed.delta, mergeRegistryState);
  }

  /// A fresh admission can take minutes to become visible to every peer's
  /// validate pass (rarely-changing-field lag, plan.md risks); within the lag
  /// window after admitting, keep retrying instead of surfacing a spurious
  /// rejection. Outside it, three quick attempts cover transient failures.
  /// `patient` forces the full window regardless of admission state (used by
  /// the admission update itself). Each attempt is UPDATE with a PUT-merged-
  /// state fallback (updateOrPut), hard-timed-out per step (#5048).
  private async updateWithRetry(
    key: ContractKey,
    delta: Uint8Array,
    merge: StateMerge,
    patient = false,
  ): Promise<void> {
    const lagDeadline = patient
      ? Date.now() + ADMISSION_LAG_RETRY_MS
      : this.admittedAt === null
        ? 0
        : this.admittedAt + ADMISSION_LAG_RETRY_MS;
    let lastError: unknown;
    for (let attempt = 0; ; attempt++) {
      try {
        await updateOrPut(this.client, key, delta, merge, this.putFallback);
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

  async ghostkeyPresence(): Promise<GhostkeyPresence | null> {
    return this.options.ghostkeySucceeds ? { usable: 1, unusable: 0 } : { usable: 0, unusable: 0 };
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
    identityDelegateWasmB64: IDENTITY_DELEGATE_WASM_B64,
    ghostkeysDelegate: {
      keyBytes: __GHOSTKEYS_DELEGATE_KEY_BYTES__,
      codeHashBytes: __GHOSTKEYS_DELEGATE_CODE_HASH_BYTES__,
    },
    legacyIds: __LEGACY_IDS_JSON__,
  });
}
