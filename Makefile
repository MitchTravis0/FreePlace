PROJECT_DIR := $(abspath .)
WEB_DIR := $(PROJECT_DIR)/web

# Contract/delegate crates are lockfile-isolated (not workspace members).
CONTRACT_CRATES := registry tile chat facade
CONTRACT_DIRS := $(foreach c,$(CONTRACT_CRATES),contracts/$(c)-contract)
DELEGATE_DIRS := delegates/identity-delegate
ISOLATED_DIRS := $(CONTRACT_DIRS) $(DELEGATE_DIRS)

# Dev node port. 7509 is the system node (connected to the REAL network);
# never publish there during development. The isolated dev node runs on 7510.
WS_API_PORT ?= 7510
DEV_NODE_DIR := $(HOME)/.local/share/freeplace-dev-node

.PHONY: build build-workspace build-contracts build-delegates \
        check check-constants check-migration preflight release \
        fmt fmt-check clippy test hooks \
        publish-hello get-hello smoke-phase2 smoke-phase3 smoke-phase4 \
        smoke-phase5 smoke-phase6 smoke-phase7 liveness node-status clean

build: build-workspace build-contracts build-delegates

build-workspace:
	cargo build --workspace

# Each isolated crate builds into its own target/ so fdev and workspace builds
# never fight over a shared resolver (build-system.md).
build-contracts:
	@for d in $(CONTRACT_DIRS); do \
		echo "== building $$d"; \
		(cd $$d && CARGO_TARGET_DIR="$$PWD/target" \
			cargo build --release --locked --target wasm32-unknown-unknown) || exit 1; \
	done

build-delegates:
	@for d in $(DELEGATE_DIRS); do \
		echo "== building $$d"; \
		(cd $$d && CARGO_TARGET_DIR="$$PWD/target" \
			cargo build --release --locked --target wasm32-unknown-unknown) || exit 1; \
	done

check: fmt-check clippy check-constants test

# The TS mirror in web/src/state.ts must match common/src/constants.rs.
check-constants:
	./scripts/check-constants.sh

# Fails when a contract/delegate WASM hash changed since the last release
# without the outgoing hash registered in its legacy_*.toml (or when the
# never-rebuilt facade WASM changed at all).
check-migration: build-contracts build-delegates
	./scripts/check-migration.sh

# Everything that must be green before a publish.
preflight: check check-migration

# Full release to the node on WS_API_PORT (default: the isolated dev node on
# 7510). The real-network publish is the same command run by hand with
# WS_API_PORT=7509 after `make dev-node` is stopped.
release: build-contracts build-delegates preflight
	./scripts/release.sh

fmt:
	cargo fmt
	@for d in $(ISOLATED_DIRS); do (cd $$d && cargo fmt) || exit 1; done

fmt-check:
	cargo fmt --check
	@for d in $(ISOLATED_DIRS); do \
		echo "== fmt $$d"; \
		(cd $$d && cargo fmt --check) || exit 1; \
	done

clippy:
	cargo clippy --workspace --all-targets -- -D warnings
	@for d in $(ISOLATED_DIRS); do \
		echo "== clippy $$d"; \
		(cd $$d && CARGO_TARGET_DIR="$$PWD/target" \
			cargo clippy --all-targets -- -D warnings) || exit 1; \
	done

test:
	cargo test --workspace
	@for d in $(ISOLATED_DIRS); do \
		echo "== test $$d"; \
		(cd $$d && CARGO_TARGET_DIR="$$PWD/target" cargo test) || exit 1; \
	done

# Run once per clone to activate the repo-managed git hooks.
hooks:
	git config core.hooksPath .githooks

# Isolated local-mode node (no P2P, own config/data/log dirs). Runs in the
# foreground; use a second terminal or `make dev-node &`.
dev-node:
	mkdir -p $(DEV_NODE_DIR)/config $(DEV_NODE_DIR)/data $(DEV_NODE_DIR)/logs
	freenet local local \
		--ws-api-port $(WS_API_PORT) \
		--config-dir $(DEV_NODE_DIR)/config \
		--data-dir $(DEV_NODE_DIR)/data \
		--log-dir $(DEV_NODE_DIR)/logs

# ---- Phase 0 exit check: publish a hello contract to the local node -------

HELLO_DIR := contracts/registry-contract
HELLO_WASM := $(HELLO_DIR)/target/wasm32-unknown-unknown/release/freeplace_registry_contract.wasm
HELLO_OUT := $(PROJECT_DIR)/.local/hello

publish-hello: build-contracts
	mkdir -p $(HELLO_OUT)
	printf 'freeplace-phase0' > $(HELLO_OUT)/params.bin
	printf 'hello freeplace'  > $(HELLO_OUT)/state.bin
	fdev -p $(WS_API_PORT) publish \
		--code $(HELLO_WASM) \
		--parameters $(HELLO_OUT)/params.bin \
		contract \
		--state $(HELLO_OUT)/state.bin

hello-key:
	@fdev get-contract-id \
		--code $(HELLO_WASM) \
		--parameters $(HELLO_OUT)/params.bin

# ---- Phase 2 exit check: registry admission smoke on the local node -------

smoke-phase2: build-contracts
	./scripts/phase2-smoke.sh

# ---- Phase 3 exit check: 16 tiles + registry-gated placements --------------

smoke-phase3: build-contracts
	./scripts/phase3-smoke.sh

# ---- Phase 4 exit check: chat post + subscription + capped eviction --------

smoke-phase4: build-contracts
	./scripts/phase4-smoke.sh

# ---- Phase 5 exit check: browser-driven identity delegate + ghost key flow -

smoke-phase5: build-contracts build-delegates
	./scripts/phase5-smoke.sh

# ---- Phase 6 exit check: full UI through the gateway-served webapp ---------

smoke-phase6: build-contracts build-delegates
	./scripts/phase6-smoke.sh

# ---- Phase 7 exit check: release pipeline + migration probe + preflight ----

smoke-phase7: build-contracts build-delegates
	./scripts/phase7-smoke.sh

# ---- Phase 13 exit check: liveness spec against the published gateway ------

# After a publish, run the minimal liveness spec against the stable facade
# URL. Pass the URL explicitly (real network: port 7509):
#   make liveness FREEPLACE_LIVE_URL=http://127.0.0.1:7509/v1/contract/web/<facade-id>/
# Without it, the URL is derived from published/release.env and WS_API_PORT.
liveness:
	@url="$(FREEPLACE_LIVE_URL)"; \
	if [ -z "$$url" ] && [ -f published/release.env ]; then \
		url="http://127.0.0.1:$(WS_API_PORT)/v1/contract/web/$$(. published/release.env && echo $$FACADE_ID)/"; \
	fi; \
	[ -n "$$url" ] || { echo "set FREEPLACE_LIVE_URL or run a release first"; exit 1; }; \
	echo "== liveness against $$url"; \
	(cd web && FREEPLACE_LIVE_URL="$$url" npx playwright test tests/liveness.spec.ts)

node-status:
	curl -s http://127.0.0.1:$(WS_API_PORT)/ >/dev/null && echo "node up on $(WS_API_PORT)" \
		|| echo "node DOWN on $(WS_API_PORT)"

clean:
	cargo clean
	@for d in $(ISOLATED_DIRS); do rm -rf $$d/target; done
	rm -rf $(WEB_DIR)/dist $(PROJECT_DIR)/.local
