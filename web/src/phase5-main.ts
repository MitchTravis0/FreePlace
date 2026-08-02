// Phase 5 harness page: exercises identity creation/persistence, PoW
// admission, delegate-signed placement, and the ghost key flow against a
// local node. Phase 6 replaces this with the real canvas UI; the modules it
// drives (freenet-api, delegate-api, identity, ghostkey, pow, cbor) are the
// production ones.

import { cborDecode, CborValue, mapGet } from "./cbor";
import { contractKeyFromId, FreenetClient } from "./freenet-api";
import { proveGhostkey } from "./ghostkey";
import { asByteArray, IdentityClient } from "./identity";
import { grindPowNonce } from "./pow";

const IDENTITY_DELEGATE = {
  keyBytes: __IDENTITY_DELEGATE_KEY_BYTES__,
  codeHashBytes: __IDENTITY_DELEGATE_CODE_HASH_BYTES__,
};
const GHOSTKEYS_DELEGATE = {
  keyBytes: __GHOSTKEYS_DELEGATE_KEY_BYTES__,
  codeHashBytes: __GHOSTKEYS_DELEGATE_CODE_HASH_BYTES__,
};
const REGISTRY_CONTRACT_ID = __REGISTRY_CONTRACT_ID__;
const TILE_CONTRACT_ID = __TILE_CONTRACT_ID__;
const REGISTRY_PARAMS = Uint8Array.from(__REGISTRY_PARAMS_BYTES__);
const TILE_PARAMS = Uint8Array.from(__TILE_PARAMS_BYTES__);

const PLACE_COORD = 4242;
const PLACE_COLOR = 7;

function nowTs(): number {
  return Math.floor(Date.now() / 1000);
}

function hex(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

const app = document.querySelector<HTMLDivElement>("#app")!;
app.innerHTML = `
  <h1>FreePlace — Phase 5 identity harness</h1>
  <dl>
    <dt>Node</dt><dd data-testid="conn-status">connecting</dd>
    <dt>Identity</dt><dd data-testid="identity-vk">-</dd>
    <dt>Admission</dt><dd data-testid="admit-status">-</dd>
    <dt>Placement</dt><dd data-testid="place-status">-</dd>
    <dt>Ghost key</dt><dd data-testid="ghostkey-status">-</dd>
  </dl>
  <button data-testid="btn-admit">Admit (PoW)</button>
  <button data-testid="btn-place">Place pixel</button>
  <button data-testid="btn-ghostkey">Use ghost key</button>
  <p>
    Ghost keys cost money; the money funds Freenet, and the mint is
    centralized. The free proof-of-work path is always sufficient for full
    participation — a ghost key only shortens the pixel cooldown.
  </p>
`;

const el = (id: string) => app.querySelector<HTMLElement>(`[data-testid="${id}"]`)!;
const setStatus = (id: string, text: string) => {
  el(id).textContent = text;
};

const client = new FreenetClient({
  onOpen: () => {
    setStatus("conn-status", "connected");
    void loadIdentity();
  },
  onClose: (code, reason) => setStatus("conn-status", `closed: ${reason || code}`),
});
const identity = new IdentityClient(client, IDENTITY_DELEGATE);

async function loadIdentity(): Promise<void> {
  try {
    const vk = await identity.getIdentity();
    setStatus("identity-vk", hex(vk));
  } catch (err) {
    setStatus("identity-vk", `error: ${String(err)}`);
  }
}

el("btn-admit").addEventListener("click", () => {
  void (async () => {
    try {
      setStatus("admit-status", "grinding");
      const challenge = await identity.admissionChallenge(REGISTRY_PARAMS);
      const nonce = grindPowNonce(challenge.bytes, challenge.difficultyBits);
      setStatus("admit-status", `nonce ${nonce}; signing`);
      const signed = await identity.signPowAdmission(REGISTRY_PARAMS, nonce, nowTs(), "phase5");
      await client.updateWithDelta(contractKeyFromId(REGISTRY_CONTRACT_ID), signed.delta);
      setStatus("admit-status", "admitted");
    } catch (err) {
      setStatus("admit-status", `error: ${String(err)}`);
    }
  })();
});

el("btn-place").addEventListener("click", () => {
  void (async () => {
    try {
      setStatus("place-status", "signing");
      const signed = await identity.signPlacement(TILE_PARAMS, PLACE_COORD, PLACE_COLOR, nowTs());
      const tileKey = contractKeyFromId(TILE_CONTRACT_ID);
      await client.updateWithDelta(tileKey, signed.delta);
      setStatus("place-status", "sent; confirming");
      // The validate pass (registry membership via related contracts) runs on
      // the node after the update; poll the stored state briefly.
      for (let attempt = 0; attempt < 10; attempt++) {
        const state = await client.getState(tileKey);
        if (tileHasPlacement(state, signed.verifyingKey, PLACE_COORD, PLACE_COLOR)) {
          setStatus("place-status", "accepted");
          return;
        }
        await new Promise((r) => setTimeout(r, 1000));
      }
      setStatus("place-status", "not visible in tile state");
    } catch (err) {
      setStatus("place-status", `error: ${String(err)}`);
    }
  })();
});

el("btn-ghostkey").addEventListener("click", () => {
  void (async () => {
    try {
      setStatus("ghostkey-status", "requesting");
      const challenge = await identity.admissionChallenge(REGISTRY_PARAMS);
      const outcome = await proveGhostkey(client, GHOSTKEYS_DELEGATE, challenge.bytes);
      switch (outcome.kind) {
        case "signature":
          setStatus("ghostkey-status", `signature (${outcome.signature.length} bytes)`);
          break;
        case "no-identity":
          setStatus("ghostkey-status", `no-identity: ${outcome.detail}`);
          break;
        case "access-denied":
          setStatus("ghostkey-status", "access-denied");
          break;
      }
    } catch (err) {
      setStatus("ghostkey-status", `error: ${String(err)}`);
    }
  })();
});

/// Scan a CBOR TileState for a placement by this author at coord with color.
function tileHasPlacement(
  stateBytes: Uint8Array,
  verifyingKey: Uint8Array,
  coord: number,
  color: number,
): boolean {
  if (stateBytes.length === 0) return false;
  const placements = mapGet(cborDecode(stateBytes), "placements");
  if (!(placements instanceof Map)) return false;
  for (const [author, log] of placements) {
    if (hex(tryBytes(author)) !== hex(verifyingKey)) continue;
    if (!(log instanceof Map)) continue;
    for (const placement of log.values()) {
      if (mapGet(placement, "coord") === coord && mapGet(placement, "color") === color) {
        return true;
      }
    }
  }
  return false;
}

function tryBytes(value: CborValue): Uint8Array {
  try {
    return asByteArray(value);
  } catch {
    return new Uint8Array();
  }
}
