//! Phase 5 browser-smoke fixture helper.
//!
//! `gen-registry <outdir> <webgen>` writes registry publish fixtures (fresh
//! random canvas_id per run) plus the web build-time parameter bytes.
//! `gen-tile <outdir> <webgen> <registry_id_base58>` writes the tile (0,0)
//! publish fixtures bound to that registry instance.
//! `delegate-keys <wasm> <webgen> <versioned-out>` writes the identity
//! delegate key/code-hash byte arrays (key = blake3(code_hash || params) with
//! empty params), the versioned delegate file `fdev publish delegate` expects
//! (u64 BE API version 0, then the 32-byte blake3 code hash, then the WASM),
//! and placeholder ghostkeys-delegate addresses (absent on the dev node, so
//! the ghost key flow exercises its delegate-unavailable state).

use std::env;
use std::fs;
use std::path::Path;
use std::process::exit;
use std::time::{SystemTime, UNIX_EPOCH};

use common::registry::{RegistryParameters, RegistryState};
use common::tile::{TileParameters, TileState};
use common::to_cbor;

fn write(dir: &Path, name: &str, bytes: Vec<u8>) {
    fs::write(dir.join(name), bytes).expect("write fixture");
}

fn json_byte_array(bytes: &[u8]) -> Vec<u8> {
    let inner: Vec<String> = bytes.iter().map(u8::to_string).collect();
    format!("[{}]", inner.join(",")).into_bytes()
}

fn gen_registry(outdir: &Path, webgen: &Path) {
    fs::create_dir_all(outdir).expect("create output dir");
    fs::create_dir_all(webgen).expect("create webgen dir");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let canvas_id = *blake3::hash(&nanos.to_le_bytes()).as_bytes();
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
    println!("registry fixtures written to {}", outdir.display());
}

fn gen_tile(outdir: &Path, webgen: &Path, registry_id_base58: &str) {
    let canvas_id: [u8; 32] = fs::read(outdir.join("canvas-id.bin"))
        .expect("canvas-id.bin from gen-registry")
        .try_into()
        .expect("canvas id is 32 bytes");
    let registry: [u8; 32] = bs58::decode(registry_id_base58)
        .into_vec()
        .expect("registry id is base58")
        .try_into()
        .expect("registry id is 32 bytes");

    let params = TileParameters {
        canvas_id,
        tile_x: 0,
        tile_y: 0,
        registry,
    };
    let params_cbor = to_cbor(&params);
    write(outdir, "tile-params.bin", params_cbor.clone());
    write(outdir, "tile-state.bin", to_cbor(&TileState::default()));
    write(
        webgen,
        "tile_params_bytes.json",
        json_byte_array(&params_cbor),
    );
    println!("tile fixtures written to {}", outdir.display());
}

fn delegate_keys(wasm: &Path, webgen: &Path, versioned_out: &Path) {
    fs::create_dir_all(webgen).expect("create webgen dir");
    let wasm_bytes = fs::read(wasm).expect("read delegate wasm");
    let code_hash = *blake3::hash(&wasm_bytes).as_bytes();
    // Empty parameters: delegate_key = blake3(code_hash || params).
    let delegate_key = *blake3::hash(&code_hash).as_bytes();

    // The versioned container format from freenet-stdlib versioning.rs.
    let mut versioned = Vec::with_capacity(8 + 32 + wasm_bytes.len());
    versioned.extend_from_slice(&0u64.to_be_bytes());
    versioned.extend_from_slice(&code_hash);
    versioned.extend_from_slice(&wasm_bytes);
    fs::write(versioned_out, versioned).expect("write versioned delegate");
    write(
        webgen,
        "identity_delegate_code_hash_bytes.json",
        json_byte_array(&code_hash),
    );
    write(
        webgen,
        "identity_delegate_key_bytes.json",
        json_byte_array(&delegate_key),
    );

    // No ghostkeys delegate exists on the isolated dev node; a well-formed but
    // unregistered address drives the flow into its "unavailable" state.
    let ghost_code_hash = *blake3::hash(b"freeplace:phase5:no-ghostkeys-delegate:code").as_bytes();
    let ghost_key = *blake3::hash(&ghost_code_hash).as_bytes();
    write(
        webgen,
        "ghostkeys_delegate_code_hash_bytes.json",
        json_byte_array(&ghost_code_hash),
    );
    write(
        webgen,
        "ghostkeys_delegate_key_bytes.json",
        json_byte_array(&ghost_key),
    );
    println!("delegate key bytes written to {}", webgen.display());
}

fn main() {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("gen-registry") if args.len() == 4 => {
            gen_registry(Path::new(&args[2]), Path::new(&args[3]))
        }
        Some("gen-tile") if args.len() == 5 => {
            gen_tile(Path::new(&args[2]), Path::new(&args[3]), &args[4])
        }
        Some("delegate-keys") if args.len() == 5 => delegate_keys(
            Path::new(&args[2]),
            Path::new(&args[3]),
            Path::new(&args[4]),
        ),
        _ => {
            eprintln!(
                "usage: phase5_smoke gen-registry <outdir> <webgen> | \
                 gen-tile <outdir> <webgen> <registry_id> | \
                 delegate-keys <wasm> <webgen> <versioned-out>"
            );
            exit(2);
        }
    }
}
