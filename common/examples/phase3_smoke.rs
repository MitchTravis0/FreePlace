//! Phase 3 local-node smoke helper.
//!
//! `gen-registry <outdir>` writes the registry publish fixtures and an
//! admission delta for the smoke identity (fresh random canvas_id per run).
//! `gen-tiles <outdir> <registry_id_base58>` writes the 16 tile parameter
//! files bound to that registry instance plus the placement deltas.
//! `check-tile <statefile>` asserts exactly the valid placement is present.

use std::env;
use std::fs;
use std::path::Path;
use std::process::exit;
use std::time::{SystemTime, UNIX_EPOCH};

use common::constants::TILES_PER_SIDE;
use common::identity::AuthorId;
use common::registry::{
    find_pow_nonce, AdmissionProof, AdmissionRecord, RegistryDelta, RegistryParameters,
    RegistryState, SignedNickname,
};
use common::tile::{SignedPlacement, TileDelta, TileParameters, TileState};
use common::to_cbor;
use ed25519_dalek::SigningKey;

const PLACE_COORD: u16 = 300;
const PLACE_COLOR: u8 = 5;

fn admitted_identity() -> SigningKey {
    SigningKey::from_bytes(&[11; 32])
}

fn unadmitted_identity() -> SigningKey {
    SigningKey::from_bytes(&[22; 32])
}

fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs()
}

fn write(outdir: &Path, name: &str, bytes: Vec<u8>) {
    fs::write(outdir.join(name), bytes).expect("write fixture");
}

fn gen_registry(outdir: &Path) {
    fs::create_dir_all(outdir).expect("create output dir");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let canvas_id = *blake3::hash(&nanos.to_le_bytes()).as_bytes();
    let params = RegistryParameters { canvas_id };

    write(outdir, "canvas-id.bin", canvas_id.to_vec());
    write(outdir, "registry-params.bin", to_cbor(&params));
    write(
        outdir,
        "registry-state.bin",
        to_cbor(&RegistryState::default()),
    );

    let identity = admitted_identity();
    let nonce = find_pow_nonce(&params, &identity.verifying_key());
    let record = AdmissionRecord::sign(
        &identity,
        &params,
        AdmissionProof::Work { nonce },
        Some(SignedNickname::sign(&identity, &params, "smoke", 1)),
        now_ts(),
    );
    write(
        outdir,
        "delta-admit.bin",
        to_cbor(&RegistryDelta {
            admissions: vec![record],
            nicknames: vec![],
        }),
    );
    println!("registry fixtures written to {}", outdir.display());
}

fn gen_tiles(outdir: &Path, registry_id_base58: &str) {
    let canvas_id: [u8; 32] = fs::read(outdir.join("canvas-id.bin"))
        .expect("canvas-id.bin from gen-registry")
        .try_into()
        .expect("canvas id is 32 bytes");
    let registry: [u8; 32] = bs58::decode(registry_id_base58)
        .into_vec()
        .expect("registry id is base58")
        .try_into()
        .expect("registry id is 32 bytes");

    let tile_params = |x: u8, y: u8| TileParameters {
        canvas_id,
        tile_x: x,
        tile_y: y,
        registry,
    };
    for x in 0..TILES_PER_SIDE {
        for y in 0..TILES_PER_SIDE {
            write(
                outdir,
                &format!("tile-params-{x}-{y}.bin"),
                to_cbor(&tile_params(x, y)),
            );
        }
    }
    write(outdir, "tile-state.bin", to_cbor(&TileState::default()));

    // All placement deltas target tile (0,0).
    let params = tile_params(0, 0);
    let ts = now_ts();
    let valid = SignedPlacement::sign(&admitted_identity(), &params, PLACE_COORD, PLACE_COLOR, ts);
    write(
        outdir,
        "delta-place.bin",
        to_cbor(&TileDelta {
            placements: vec![valid],
        }),
    );

    let mut tampered = valid;
    tampered.coord += 1;
    write(
        outdir,
        "delta-place-tampered.bin",
        to_cbor(&TileDelta {
            placements: vec![tampered],
        }),
    );

    let stranger = SignedPlacement::sign(&unadmitted_identity(), &params, PLACE_COORD + 10, 7, ts);
    write(
        outdir,
        "delta-place-unadmitted.bin",
        to_cbor(&TileDelta {
            placements: vec![stranger],
        }),
    );
    println!("tile fixtures written to {}", outdir.display());
}

fn check_tile(statefile: &Path) {
    let bytes = fs::read(statefile).expect("read state file");
    let state: TileState = common::from_cbor(&bytes).expect("fetched state decodes as TileState");

    let author = AuthorId::from(&admitted_identity().verifying_key());
    assert_eq!(
        state.placements.len(),
        1,
        "expected exactly the admitted author, found {} authors",
        state.placements.len()
    );
    let log = state
        .placements
        .get(&author)
        .expect("the admitted identity has placements");
    assert_eq!(log.len(), 1, "expected exactly one placement");
    let placement = log.values().next().unwrap();
    assert_eq!(placement.coord, PLACE_COORD);
    assert_eq!(placement.color, PLACE_COLOR);
    println!(
        "check ok: 1 author, 1 placement at coord {} color {}",
        placement.coord, placement.color
    );
}

fn main() {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("gen-registry") if args.len() == 3 => gen_registry(Path::new(&args[2])),
        Some("gen-tiles") if args.len() == 4 => gen_tiles(Path::new(&args[2]), &args[3]),
        Some("check-tile") if args.len() == 3 => check_tile(Path::new(&args[2])),
        _ => {
            eprintln!(
                "usage: phase3_smoke gen-registry <outdir> | gen-tiles <outdir> <registry_id> | check-tile <statefile>"
            );
            exit(2);
        }
    }
}
