//! Phase 6 gateway-smoke fixture helper.
//!
//! `gen-registry <outdir> <webgen>` writes registry publish fixtures (fresh
//! random canvas_id per run) plus the web build-time parameter bytes.
//! `gen-tiles <outdir> <webgen> <registry_id_base58>` writes publish fixtures
//! for all 16 tiles bound to that registry instance, plus one
//! `tile_params_bytes-<x>-<y>.json` per tile for the web build (the smoke
//! script assembles them into `tiles.json` together with the contract ids).
//! `gen-chat <outdir> <webgen> <registry_id_base58>` writes the chat publish
//! fixtures and the web build-time chat parameter bytes.
//!
//! Delegate key material is produced by `phase5_smoke -- delegate-keys`.

use std::env;
use std::fs;
use std::path::Path;
use std::process::exit;
use std::time::{SystemTime, UNIX_EPOCH};

use common::chat::{ChatParameters, ChatState};
use common::constants::TILES_PER_SIDE;
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

fn read_canvas_id(outdir: &Path) -> [u8; 32] {
    fs::read(outdir.join("canvas-id.bin"))
        .expect("canvas-id.bin from gen-registry")
        .try_into()
        .expect("canvas id is 32 bytes")
}

fn decode_registry_id(registry_id_base58: &str) -> [u8; 32] {
    bs58::decode(registry_id_base58)
        .into_vec()
        .expect("registry id is base58")
        .try_into()
        .expect("registry id is 32 bytes")
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

fn gen_tiles(outdir: &Path, webgen: &Path, registry_id_base58: &str) {
    let canvas_id = read_canvas_id(outdir);
    let registry = decode_registry_id(registry_id_base58);

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
    println!("tile fixtures written to {}", outdir.display());
}

fn gen_chat(outdir: &Path, webgen: &Path, registry_id_base58: &str) {
    let canvas_id = read_canvas_id(outdir);
    let registry = decode_registry_id(registry_id_base58);

    let params = ChatParameters {
        canvas_id,
        registry,
    };
    let params_cbor = to_cbor(&params);
    write(outdir, "chat-params.bin", params_cbor.clone());
    write(outdir, "chat-state.bin", to_cbor(&ChatState::default()));
    write(
        webgen,
        "chat_params_bytes.json",
        json_byte_array(&params_cbor),
    );
    println!("chat fixtures written to {}", outdir.display());
}

fn main() {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("gen-registry") if args.len() == 4 => {
            gen_registry(Path::new(&args[2]), Path::new(&args[3]))
        }
        Some("gen-tiles") if args.len() == 5 => {
            gen_tiles(Path::new(&args[2]), Path::new(&args[3]), &args[4])
        }
        Some("gen-chat") if args.len() == 5 => {
            gen_chat(Path::new(&args[2]), Path::new(&args[3]), &args[4])
        }
        _ => {
            eprintln!(
                "usage: phase6_smoke gen-registry <outdir> <webgen> | \
                 gen-tiles <outdir> <webgen> <registry_id> | \
                 gen-chat <outdir> <webgen> <registry_id>"
            );
            exit(2);
        }
    }
}
