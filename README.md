# FreePlace

A decentralized r/place on [Freenet](https://freenet.org): a shared 1024x1024 pixel
canvas where each user places one pixel and then waits out a cooldown, with a single
global chat room alongside. No servers; everything lives in Freenet contracts,
synchronized peer to peer as CRDTs.

If you run a Freenet node, the app is at:

```
http://127.0.0.1:7509/v1/contract/web/6xAnuGyjYvSjoPhtZhdpD5bPgSSdpEUsUSHDvoubsAAM/
```

(Replace host/port with your own node's gateway. The URL is stable across releases.)

## How it works

| Component | Instances | Purpose |
|---|---|---|
| `registry-contract` | 1 | Identity admission: verifies a proof-of-work nonce or a ghost key certificate once, records `(identity, tier, nickname)`. |
| `tile-contract` | 16 | One 256x256 tile of the canvas. Pixel placements are a signed last-writer-wins CRDT with per-author cooldown enforcement. |
| `chat-contract` | 1 | Global chat room. Signed messages, capped ring buffer, per-author rate limit. |
| `facade-contract` | 1 | Never-rebuilt stable-URL contract holding a signed pointer to the current web container. |
| `identity-delegate` | 1 | Holds the user's ed25519 signing key on their own node; signs placements, messages, and admissions. |
| `web/` | | TypeScript + Vite single-page app served through the node's gateway. |

Anti-bot admission is proof of work for everyone (18-bit blake3 grind, run in a Web
Worker during onboarding). Holders of a [ghost key](https://freenet.org/ghostkey) can
skip the wait and get a shorter pixel cooldown (30s per tile vs 120s). Ghost keys cost
money, the money funds Freenet, and the mint is centralized; the PoW path is always
sufficient for full participation.

The rendered canvas is derived, never stored: each peer independently filters every
author's placement log through a deterministic cooldown filter and takes the
last-writer-wins winner per coordinate, so peers that have seen the same placements
show the same canvas regardless of arrival order. Cooldowns are enforced per tile by
the contracts; the app enforces the global cooldown for honest users (see the in-app
FAQ for the tradeoff).

## Repository layout

```
common/            shared types, signing-byte builders, CRDT core, constants
contracts/         registry, tile, chat, facade contract crates (lockfile-isolated)
delegates/         identity delegate crate
web/               Vite + TypeScript UI, Playwright test suites
scripts/           release pipeline, migration guard, per-phase smoke tests
published/         release manifest: instance ids, canvas id, legacy id registry
```

Contract and delegate crates are deliberately not workspace members: each has its own
lockfile and target dir so a workspace `cargo update` can never silently re-key a
published contract.

## Building and testing

Prerequisites: Rust with the `wasm32-unknown-unknown` target, Node 20+, `fdev` and
`freenet` (from [freenet-core](https://github.com/freenet/freenet-core)), and
`npx playwright install` inside `web/`.

```
make check          # fmt, clippy -D warnings, constants drift check, all tests
cd web && npx playwright test tests/offline.spec.ts tests/narrow.spec.ts
```

The offline Playwright tier drives the full UI against `vite dev` with mock data and
requires no node. For the full local stack:

```
make dev-node       # isolated local node on port 7510 (foreground)
make smoke-phase6   # publish everything to it and drive the UI through the gateway
```

## Releasing

```
make preflight      # refuses to release if a WASM hash changed without a
                    # legacy_contracts.toml entry, or if the facade WASM changed at all
WS_API_PORT=7509 make release    # full publish; flips the facade pointer
WS_API_PORT=7509 make liveness   # post-publish check against the live gateway
```

A contract WASM change moves its key. The release pipeline records displaced instance
ids in `published/legacy/`, and the app probes them on load to carry state forward to
the new keys, so upgrades do not lose the canvas. The facade owner signing key lives
outside the repo (`~/.config/freeplace/facade-owner.key`); back it up, since losing it
permanently strands the stable URL.
