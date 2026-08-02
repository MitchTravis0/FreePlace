//! Phase 4 local-node smoke helper.
//!
//! `gen-registry <outdir>` writes the registry publish fixtures and an
//! admission delta for the smoke identity (fresh random canvas_id per run).
//! `gen-chat <outdir> <registry_id_base58>` writes the chat parameter file
//! bound to that registry instance plus the post/tamper/flood deltas.
//! `check-chat <statefile>` asserts exactly the valid post is present.
//! `check-sub <file> <needle>` asserts a subscription capture contains the
//! posted content bytes.
//! `check-evicted <statefile>` asserts the flood evicted deterministically.

use std::env;
use std::fs;
use std::path::Path;
use std::process::exit;
use std::time::{SystemTime, UNIX_EPOCH};

use common::chat::{ChatDelta, ChatParameters, ChatState, SignedMessage};
use common::constants::{CHAT_MAX_MESSAGES_PER_AUTHOR, CHAT_MESSAGE_CAP};
use common::registry::{
    find_pow_nonce, AdmissionProof, AdmissionRecord, RegistryDelta, RegistryParameters,
    RegistryState, SignedNickname,
};
use common::to_cbor;
use ed25519_dalek::SigningKey;

const POST_CONTENT: &str = "hello from the phase 4 smoke";
/// The flood is CHAT_MESSAGE_CAP + 10 messages from one author, all older
/// than the post. The per-author stored cap bounds that author (post
/// included) to CHAT_MAX_MESSAGES_PER_AUTHOR messages, newest kept, so the
/// oldest survivor is the flood message CHAT_MAX_MESSAGES_PER_AUTHOR - 1
/// slots from the end.
const FLOOD_COUNT: u64 = CHAT_MESSAGE_CAP as u64 + 10;
const OLDEST_SURVIVOR_INDEX: u64 = FLOOD_COUNT - (CHAT_MAX_MESSAGES_PER_AUTHOR as u64 - 1);

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

fn gen_chat(outdir: &Path, registry_id_base58: &str) {
    let canvas_id: [u8; 32] = fs::read(outdir.join("canvas-id.bin"))
        .expect("canvas-id.bin from gen-registry")
        .try_into()
        .expect("canvas id is 32 bytes");
    let registry: [u8; 32] = bs58::decode(registry_id_base58)
        .into_vec()
        .expect("registry id is base58")
        .try_into()
        .expect("registry id is 32 bytes");

    let params = ChatParameters {
        canvas_id,
        registry,
    };
    write(outdir, "chat-params.bin", to_cbor(&params));
    write(outdir, "chat-state.bin", to_cbor(&ChatState::default()));

    let ts = now_ts();
    let post = SignedMessage::sign(&admitted_identity(), &params, POST_CONTENT, ts, 0);
    write(
        outdir,
        "delta-post.bin",
        to_cbor(&ChatDelta {
            messages: vec![post.clone()],
        }),
    );

    let mut tampered = post;
    tampered.content.push('!');
    write(
        outdir,
        "delta-post-tampered.bin",
        to_cbor(&ChatDelta {
            messages: vec![tampered],
        }),
    );

    let stranger = SignedMessage::sign(&unadmitted_identity(), &params, "intruder", ts, 1);
    write(
        outdir,
        "delta-post-unadmitted.bin",
        to_cbor(&ChatDelta {
            messages: vec![stranger],
        }),
    );

    // All flood messages are older than the post, so eviction keeps the post.
    let flood: Vec<SignedMessage> = (0..FLOOD_COUNT)
        .map(|i| {
            SignedMessage::sign(
                &admitted_identity(),
                &params,
                &format!("flood {i}"),
                ts - 1000 + i,
                0,
            )
        })
        .collect();
    write(
        outdir,
        "delta-flood.bin",
        to_cbor(&ChatDelta { messages: flood }),
    );
    println!("chat fixtures written to {}", outdir.display());
}

fn read_state(statefile: &Path) -> ChatState {
    let bytes = fs::read(statefile).expect("read state file");
    common::from_cbor(&bytes).expect("fetched state decodes as ChatState")
}

fn check_chat(statefile: &Path) {
    let state = read_state(statefile);
    assert_eq!(
        state.messages.len(),
        1,
        "expected exactly one message, found {}",
        state.messages.len()
    );
    let message = state.messages.values().next().unwrap();
    assert_eq!(message.content, POST_CONTENT);
    assert_eq!(
        message.author,
        admitted_identity().verifying_key(),
        "message author must be the admitted smoke identity"
    );
    println!("check ok: 1 message, content {:?}", message.content);
}

fn check_sub(file: &Path, needle: &str) {
    let bytes = fs::read(file).expect("read subscription capture");
    assert!(
        bytes
            .windows(needle.len())
            .any(|window| window == needle.as_bytes()),
        "subscription capture ({} bytes) does not contain {needle:?}",
        bytes.len()
    );
    println!("check ok: subscription capture contains {needle:?}");
}

fn check_evicted(statefile: &Path) {
    let state = read_state(statefile);
    assert_eq!(
        state.messages.len(),
        CHAT_MAX_MESSAGES_PER_AUTHOR,
        "expected the single flooding author capped at CHAT_MAX_MESSAGES_PER_AUTHOR, found {} messages",
        state.messages.len()
    );
    let oldest = state.messages.values().next().unwrap();
    let oldest_survivor = format!("flood {OLDEST_SURVIVOR_INDEX}");
    assert_eq!(
        oldest.content, oldest_survivor,
        "eviction was not deterministic: oldest survivor is {:?}",
        oldest.content
    );
    let newest = state.messages.values().next_back().unwrap();
    assert_eq!(
        newest.content, POST_CONTENT,
        "the original post must survive the flood"
    );
    println!(
        "check ok: {} messages, oldest survivor {:?}, post retained",
        state.messages.len(),
        oldest.content
    );
}

fn main() {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("gen-registry") if args.len() == 3 => gen_registry(Path::new(&args[2])),
        Some("gen-chat") if args.len() == 4 => gen_chat(Path::new(&args[2]), &args[3]),
        Some("check-chat") if args.len() == 3 => check_chat(Path::new(&args[2])),
        Some("check-sub") if args.len() == 4 => check_sub(Path::new(&args[2]), &args[3]),
        Some("check-evicted") if args.len() == 3 => check_evicted(Path::new(&args[2])),
        _ => {
            eprintln!(
                "usage: phase4_smoke gen-registry <outdir> | gen-chat <outdir> <registry_id> | check-chat <statefile> | check-sub <file> <needle> | check-evicted <statefile>"
            );
            exit(2);
        }
    }
}
