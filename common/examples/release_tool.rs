//! Phase 7 release tooling, driven by scripts/release.sh, scripts/
//! check-migration.sh, and scripts/phase7-smoke.sh.
//!
//! `hash <file>` prints the blake3 hex of a file (code_hash of a WASM).
//! `guard <contract|delegate> <registry_toml> <base_hash> <head_hash>` fails
//! (exit 1) when the WASM hash changed without the outgoing hash registered.
//! `keygen <keyfile>` creates the facade owner signing key if missing and
//! prints the verifying key hex.
//! `gen-registry <outdir> <webgen> <canvas_id_hex>` writes registry publish
//! fixtures for the given stable canvas id (empty genesis state).
//! `gen-tiles` / `gen-chat <outdir> <webgen> <registry_id>` write tile/chat
//! publish fixtures bound to that registry instance (empty genesis states).
//! `sign-webapp <keyfile> <archive> <version> <params_out> <meta_out>` signs a
//! web-container release (instance = version, so each release gets a fresh id).
//! `sign-facade <keyfile> <loader_tar> <version> <current_app_id> <params_out>
//! <meta_out> <state_out> [prev_app_id...]` signs the stable facade pointer;
//! `state_out` is the framed state for `fdev execute update --as-state`.
//! `gen-seed <outdir> <registry_params> <tile_params>` writes an admission and
//! a placement delta for the smoke identity.
//! `assert-tile-has <statefile> <coord> <color>` fails unless the state holds
//! that placement.

use std::env;
use std::fs;
use std::path::Path;
use std::process::exit;
use std::time::{SystemTime, UNIX_EPOCH};

use common::constants::{FACADE_MAX_PREV_APPS, TILES_PER_SIDE};
use common::facade::{encode_facade_frame, FacadeMetadata, FacadeParameters, FacadePointer};
use common::registry::{
    find_pow_nonce, AdmissionProof, AdmissionRecord, NicknameUpdate, RegistryDelta,
    RegistryParameters, RegistryState, SignedNickname,
};
use common::tile::{SignedPlacement, TileDelta, TileParameters, TileState};
use common::to_cbor;
use ed25519_dalek::SigningKey;
use freenet_migrate_build::{check_migration_guard, Component, Registry};

const SEED_COORD: u16 = 1234;
const SEED_COLOR: u8 = 7;

fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs()
}

fn write(dir: &Path, name: &str, bytes: Vec<u8>) {
    fs::write(dir.join(name), bytes).expect("write output");
}

fn json_byte_array(bytes: &[u8]) -> Vec<u8> {
    let inner: Vec<String> = bytes.iter().map(u8::to_string).collect();
    format!("[{}]", inner.join(",")).into_bytes()
}

fn decode_hex32(hex_str: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    let hex_str = hex_str.trim();
    assert_eq!(hex_str.len(), 64, "expected 64 hex chars");
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex_str[2 * i..2 * i + 2], 16).expect("valid hex");
    }
    out
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn decode_b58_id(id: &str) -> [u8; 32] {
    bs58::decode(id.trim())
        .into_vec()
        .expect("base58 contract id")
        .try_into()
        .expect("contract id is 32 bytes")
}

fn load_key(keyfile: &Path) -> SigningKey {
    let seed: [u8; 32] = fs::read(keyfile)
        .unwrap_or_else(|e| panic!("read key file {}: {e}", keyfile.display()))
        .try_into()
        .expect("key file holds a 32-byte ed25519 seed");
    SigningKey::from_bytes(&seed)
}

fn keygen(keyfile: &Path) {
    if !keyfile.exists() {
        if let Some(parent) = keyfile.parent() {
            fs::create_dir_all(parent).expect("create key dir");
        }
        use std::io::Read;
        let mut buf = [0u8; 32];
        fs::File::open("/dev/urandom")
            .expect("open /dev/urandom")
            .read_exact(&mut buf)
            .expect("read entropy");
        fs::write(keyfile, buf).expect("write key file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(keyfile, fs::Permissions::from_mode(0o600))
                .expect("chmod key file");
        }
    }
    let key = load_key(keyfile);
    println!("{}", hex_encode(key.verifying_key().as_bytes()));
}

fn guard(component: &str, registry_toml: &Path, base: &str, head: &str) {
    let component = match component {
        "contract" => Component::Contract,
        "delegate" => Component::Delegate,
        other => panic!("unknown component {other:?}"),
    };
    let registry = Registry::from_entry_path(registry_toml, component)
        .unwrap_or_else(|e| panic!("parse {}: {e}", registry_toml.display()));
    let outcome = check_migration_guard(component, base, head, &registry).expect("valid hashes");
    if let Some(advice) = outcome.advice(component) {
        eprintln!("check-migration: {} ({advice})", registry_toml.display());
        exit(1);
    }
    println!("guard ok: {}", registry_toml.display());
}

fn gen_registry(outdir: &Path, webgen: &Path, canvas_id_hex: &str) {
    fs::create_dir_all(outdir).expect("create output dir");
    fs::create_dir_all(webgen).expect("create webgen dir");
    let canvas_id = decode_hex32(canvas_id_hex);
    let params = RegistryParameters { canvas_id };
    let params_cbor = to_cbor(&params);
    write(outdir, "canvas-id.bin", canvas_id.to_vec());
    write(outdir, "registry-params.bin", params_cbor.clone());
    write(
        outdir,
        "registry-state.bin",
        to_cbor(&RegistryState::default()),
    );
    write(
        webgen,
        "registry_params_bytes.json",
        json_byte_array(&params_cbor),
    );
}

fn read_canvas_id(outdir: &Path) -> [u8; 32] {
    fs::read(outdir.join("canvas-id.bin"))
        .expect("canvas-id.bin from gen-registry")
        .try_into()
        .expect("canvas id is 32 bytes")
}

fn gen_tiles(outdir: &Path, webgen: &Path, registry_id: &str) {
    let canvas_id = read_canvas_id(outdir);
    let registry = decode_b58_id(registry_id);
    write(outdir, "tile-state.bin", to_cbor(&TileState::default()));
    for tile_x in 0..TILES_PER_SIDE {
        for tile_y in 0..TILES_PER_SIDE {
            let params = TileParameters {
                canvas_id,
                tile_x,
                tile_y,
                registry,
            };
            let params_cbor = to_cbor(&params);
            write(
                outdir,
                &format!("tile-params-{tile_x}-{tile_y}.bin"),
                params_cbor.clone(),
            );
            write(
                webgen,
                &format!("tile_params_bytes-{tile_x}-{tile_y}.json"),
                json_byte_array(&params_cbor),
            );
        }
    }
}

fn gen_chat(outdir: &Path, webgen: &Path, registry_id: &str) {
    let canvas_id = read_canvas_id(outdir);
    let registry = decode_b58_id(registry_id);
    let params = common::chat::ChatParameters {
        canvas_id,
        registry,
    };
    let params_cbor = to_cbor(&params);
    write(outdir, "chat-params.bin", params_cbor.clone());
    write(
        outdir,
        "chat-state.bin",
        to_cbor(&common::chat::ChatState::default()),
    );
    write(
        webgen,
        "chat_params_bytes.json",
        json_byte_array(&params_cbor),
    );
}

fn sign_webapp(keyfile: &Path, archive: &Path, version: u64, params_out: &Path, meta_out: &Path) {
    let key = load_key(keyfile);
    let web = fs::read(archive).expect("read webapp archive");
    let params = FacadeParameters {
        owner_vk: key.verifying_key(),
        instance: version,
    };
    let pointer = FacadePointer {
        version,
        current_app: None,
        prev_apps: vec![],
        web_hash: *blake3::hash(&web).as_bytes(),
    };
    let meta = FacadeMetadata::sign(&key, &params, pointer);
    fs::write(params_out, to_cbor(&params)).expect("write params");
    fs::write(meta_out, to_cbor(&meta)).expect("write metadata");
}

#[allow(clippy::too_many_arguments)]
fn sign_facade(
    keyfile: &Path,
    loader_tar: &Path,
    version: u64,
    current_app: &str,
    params_out: &Path,
    meta_out: &Path,
    state_out: &Path,
    prev_apps: &[String],
) {
    let key = load_key(keyfile);
    let web = fs::read(loader_tar).expect("read loader archive");
    let params = FacadeParameters {
        owner_vk: key.verifying_key(),
        instance: 0,
    };
    let pointer = FacadePointer {
        version,
        current_app: Some(decode_b58_id(current_app)),
        prev_apps: prev_apps
            .iter()
            .take(FACADE_MAX_PREV_APPS)
            .map(|id| decode_b58_id(id))
            .collect(),
        web_hash: *blake3::hash(&web).as_bytes(),
    };
    let meta = FacadeMetadata::sign(&key, &params, pointer);
    let meta_cbor = to_cbor(&meta);
    fs::write(params_out, to_cbor(&params)).expect("write params");
    fs::write(meta_out, &meta_cbor).expect("write metadata");
    fs::write(state_out, encode_facade_frame(&meta_cbor, &web)).expect("write state");
}

fn seed_identity() -> SigningKey {
    SigningKey::from_bytes(&[77; 32])
}

/// Ops tool for the PUT fallback (freenet-core#5069): merge a registry delta
/// into fetched state bytes, producing the full state to re-PUT with
/// `fdev publish ... contract --state <out>` when UPDATEs fail with
/// "missing contract".
fn apply_registry_delta(state_file: &Path, delta_file: &Path, out: &Path) {
    let state_bytes = fs::read(state_file).expect("read state file");
    let mut state: RegistryState = if state_bytes.is_empty() {
        RegistryState::default()
    } else {
        common::from_cbor(&state_bytes).expect("state decodes as RegistryState")
    };
    let delta: RegistryDelta =
        common::from_cbor(&fs::read(delta_file).expect("read delta file")).expect("decode delta");
    state.apply_delta(&delta);
    fs::write(out, to_cbor(&state)).expect("write merged state");
    println!("merged state: {} identities", state.identities.len());
}

/// Cross-language lock for the UI's PUT-fallback state merge: for each
/// contract kind, emit (state, delta, merged-by-Rust) byte fixtures. The TS
/// merge in web/src/merge.ts must reproduce `merged` byte-for-byte
/// (web/tests/merge.spec.ts).
fn merge_fixtures(out: &Path) {
    let key = |seed: u8| SigningKey::from_bytes(&[seed; 32]);
    let reg_params = RegistryParameters { canvas_id: [7; 32] };
    let tile_params = TileParameters {
        canvas_id: [7; 32],
        tile_x: 1,
        tile_y: 2,
        registry: [9; 32],
    };
    let chat_params = common::chat::ChatParameters {
        canvas_id: [7; 32],
        registry: [9; 32],
    };

    let pow = |seed: u8, ts: u64, nick: Option<(&str, u64)>| {
        AdmissionRecord::sign(
            &key(seed),
            &reg_params,
            AdmissionProof::Work { nonce: 42 },
            nick.map(|(name, version)| {
                SignedNickname::sign(&key(seed), &reg_params, name, version)
            }),
            ts,
        )
    };
    let ghost = |seed: u8, ts: u64| {
        AdmissionRecord::sign(
            &key(seed),
            &reg_params,
            AdmissionProof::Ghostkey {
                scoped_payload: vec![1, 2],
                signature: vec![3; 64],
                certificate_pem: "PEM".to_string(),
            },
            None,
            ts,
        )
    };
    let registry_case = |name: &str, state: RegistryState, delta: RegistryDelta| {
        let mut merged = state.clone();
        merged.apply_delta(&delta);
        fixture_entry(name, &to_cbor(&state), &to_cbor(&delta), &to_cbor(&merged))
    };
    let mut two = RegistryState::default();
    two.insert_record(pow(1, 1000, Some(("nick", 1))));
    two.insert_record(ghost(2, 2000));
    let mut one = RegistryState::default();
    one.insert_record(pow(1, 1000, Some(("nick", 1))));
    let registry_cases = [
        registry_case(
            "empty-admit-pow",
            RegistryState::default(),
            RegistryDelta {
                admissions: vec![pow(1, 1000, Some(("nick", 1)))],
                nicknames: vec![],
            },
        ),
        registry_case(
            "add-and-nickname",
            two,
            RegistryDelta {
                admissions: vec![pow(3, 1500, None)],
                nicknames: vec![NicknameUpdate {
                    identity_vk: key(1).verifying_key(),
                    nickname: SignedNickname::sign(&key(1), &reg_params, "renamed", 2),
                }],
            },
        ),
        registry_case(
            "earlier-core-wins-keeps-nickname",
            one,
            RegistryDelta {
                admissions: vec![ghost(1, 500)],
                nicknames: vec![],
            },
        ),
    ];

    let place = |seed: u8, coord: u16, color: u8, ts: u64| {
        SignedPlacement::sign(&key(seed), &tile_params, coord, color, ts)
    };
    let tile_case = |name: &str, seeds: &[SignedPlacement], delta: Vec<SignedPlacement>| {
        let mut state = TileState::default();
        for p in seeds {
            state.insert(*p);
        }
        let delta = TileDelta { placements: delta };
        let mut merged = state.clone();
        merged.apply_delta(&delta);
        fixture_entry(name, &to_cbor(&state), &to_cbor(&delta), &to_cbor(&merged))
    };
    let full_log: Vec<SignedPlacement> = (0..8)
        .map(|i| place(1, 100 + i, (i % 16) as u8, 10 + 10 * u64::from(i)))
        .collect();
    let tile_cases = [
        tile_case(
            "add-authors",
            &[
                place(1, 1, 2, 10),
                place(1, 3, 4, 20),
                place(2, 500, 15, 30),
            ],
            vec![place(2, 501, 6, 40), place(3, 5, 9, 5)],
        ),
        tile_case("cap-evicts-oldest", &full_log, vec![place(1, 200, 3, 90)]),
        tile_case(
            "same-ts-slot-conflict",
            &[place(1, 1, 2, 10)],
            vec![place(1, 7, 8, 10)],
        ),
    ];

    let msg = |seed: u8, content: &str, ts: u64, seq: u32| {
        common::chat::SignedMessage::sign(&key(seed), &chat_params, content, ts, seq)
    };
    let chat_case = |name: &str,
                     seeds: Vec<common::chat::SignedMessage>,
                     delta: Vec<common::chat::SignedMessage>| {
        let mut state = common::chat::ChatState::default();
        for m in seeds {
            state.insert(m);
        }
        let delta = common::chat::ChatDelta { messages: delta };
        let mut merged = state.clone();
        merged.apply_delta(&delta);
        fixture_entry(name, &to_cbor(&state), &to_cbor(&delta), &to_cbor(&merged))
    };
    let author_full: Vec<common::chat::SignedMessage> = (0u64..32)
        .map(|i| msg(1, &format!("m{i}"), 100 + 10 * i, 0))
        .collect();
    let chat_cases = [
        chat_case(
            "add-message",
            vec![
                msg(1, "m1", 10, 0),
                msg(2, "m2", 10, 0),
                msg(1, "m3", 30, 0),
            ],
            vec![msg(3, "hello", 40, 1)],
        ),
        chat_case(
            "author-cap-evicts-oldest",
            author_full,
            vec![msg(1, "newest", 900, 0)],
        ),
        chat_case(
            "same-id-slot-conflict",
            vec![msg(1, "aaa", 10, 0)],
            vec![msg(1, "zzz", 10, 0)],
        ),
    ];

    let section = |cases: &[String]| format!("[{}]", cases.join(","));
    let json = format!(
        "{{\"registry\":{},\"tile\":{},\"chat\":{}}}\n",
        section(&registry_cases),
        section(&tile_cases),
        section(&chat_cases),
    );
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).expect("create fixture dir");
    }
    fs::write(out, json).expect("write merge fixtures");
    println!("merge fixtures written to {}", out.display());
}

fn fixture_entry(name: &str, state: &[u8], delta: &[u8], merged: &[u8]) -> String {
    format!(
        "{{\"name\":\"{name}\",\"state\":\"{}\",\"delta\":\"{}\",\"merged\":\"{}\"}}",
        hex_encode(state),
        hex_encode(delta),
        hex_encode(merged),
    )
}

fn gen_seed(outdir: &Path, registry_params: &Path, tile_params: &Path) {
    fs::create_dir_all(outdir).expect("create output dir");
    let reg_params: RegistryParameters =
        common::from_cbor(&fs::read(registry_params).expect("read registry params"))
            .expect("decode registry params");
    let tile_params: TileParameters =
        common::from_cbor(&fs::read(tile_params).expect("read tile params"))
            .expect("decode tile params");

    let identity = seed_identity();
    let ts = now_ts();
    let nonce = find_pow_nonce(&reg_params, &identity.verifying_key());
    let record = AdmissionRecord::sign(
        &identity,
        &reg_params,
        AdmissionProof::Work { nonce },
        Some(SignedNickname::sign(&identity, &reg_params, "seed", 1)),
        ts,
    );
    write(
        outdir,
        "seed-admit.bin",
        to_cbor(&RegistryDelta {
            admissions: vec![record],
            nicknames: vec![],
        }),
    );
    let placement = SignedPlacement::sign(&identity, &tile_params, SEED_COORD, SEED_COLOR, ts);
    write(
        outdir,
        "seed-place.bin",
        to_cbor(&TileDelta {
            placements: vec![placement],
        }),
    );
    println!("seed placement: coord {SEED_COORD} color {SEED_COLOR}");
}

fn assert_tile_has(statefile: &Path, coord: u16, color: u8) {
    let bytes = fs::read(statefile).expect("read state file");
    let state: TileState = common::from_cbor(&bytes).expect("state decodes as TileState");
    let found = state
        .placements
        .values()
        .flat_map(|log| log.values())
        .any(|p| p.coord == coord && p.color == color);
    if !found {
        eprintln!("assert-tile-has: no placement at coord {coord} with color {color}");
        exit(1);
    }
    println!("assert-tile-has ok: coord {coord} color {color}");
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let arg = |i: usize| -> &str { &args[i] };
    match args.get(1).map(String::as_str) {
        Some("hash") if args.len() == 3 => {
            let bytes = fs::read(arg(2)).expect("read file");
            println!("{}", blake3::hash(&bytes).to_hex());
        }
        Some("guard") if args.len() == 6 => {
            guard(arg(2), Path::new(arg(3)), arg(4), arg(5));
        }
        Some("keygen") if args.len() == 3 => keygen(Path::new(arg(2))),
        Some("gen-registry") if args.len() == 5 => {
            gen_registry(Path::new(arg(2)), Path::new(arg(3)), arg(4));
        }
        Some("gen-tiles") if args.len() == 5 => {
            gen_tiles(Path::new(arg(2)), Path::new(arg(3)), arg(4));
        }
        Some("gen-chat") if args.len() == 5 => {
            gen_chat(Path::new(arg(2)), Path::new(arg(3)), arg(4));
        }
        Some("sign-webapp") if args.len() == 7 => {
            let version: u64 = arg(4).parse().expect("version is a u64");
            sign_webapp(
                Path::new(arg(2)),
                Path::new(arg(3)),
                version,
                Path::new(arg(5)),
                Path::new(arg(6)),
            );
        }
        Some("sign-facade") if args.len() >= 9 => {
            let version: u64 = arg(4).parse().expect("version is a u64");
            sign_facade(
                Path::new(arg(2)),
                Path::new(arg(3)),
                version,
                arg(5),
                Path::new(arg(6)),
                Path::new(arg(7)),
                Path::new(arg(8)),
                &args[9..],
            );
        }
        Some("gen-seed") if args.len() == 5 => {
            gen_seed(Path::new(arg(2)), Path::new(arg(3)), Path::new(arg(4)));
        }
        Some("merge-fixtures") if args.len() == 3 => merge_fixtures(Path::new(arg(2))),
        Some("apply-registry-delta") if args.len() == 5 => {
            apply_registry_delta(Path::new(arg(2)), Path::new(arg(3)), Path::new(arg(4)));
        }
        Some("assert-tile-has") if args.len() == 5 => {
            assert_tile_has(
                Path::new(arg(2)),
                arg(3).parse().expect("coord is a u16"),
                arg(4).parse().expect("color is a u8"),
            );
        }
        _ => {
            eprintln!(
                "usage: release_tool hash <file> | \
                 guard <contract|delegate> <registry_toml> <base_hash> <head_hash> | \
                 keygen <keyfile> | \
                 gen-registry <outdir> <webgen> <canvas_id_hex> | \
                 gen-tiles <outdir> <webgen> <registry_id> | \
                 gen-chat <outdir> <webgen> <registry_id> | \
                 sign-webapp <keyfile> <archive> <version> <params_out> <meta_out> | \
                 sign-facade <keyfile> <loader_tar> <version> <current_app_id> \
                 <params_out> <meta_out> <state_out> [prev_app_id...] | \
                 gen-seed <outdir> <registry_params> <tile_params> | \
                 merge-fixtures <out_json> | \
                 apply-registry-delta <state> <delta> <out> | \
                 assert-tile-has <statefile> <coord> <color>"
            );
            exit(2);
        }
    }
}
