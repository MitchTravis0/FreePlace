use freenet_migrate_build::Component;

fn main() {
    freenet_migrate_build::codegen()
        .entry_registry(
            "../delegates/identity-delegate/legacy_delegates.toml",
            Component::Delegate,
        )
        .canonical_consts(false)
        .delegate_pair_view("LEGACY_IDENTITY_DELEGATES")
        .out_file("legacy_identity_delegates.rs")
        .emit()
        .expect("codegen legacy identity-delegate registry");

    // Contract registries: parsed and validated at build time; the emitted
    // code-hash views document the lineage (the UI probe itself works from
    // instance ids in the release manifest, not code hashes).
    for (crate_dir, view, out) in [
        (
            "registry",
            "LEGACY_REGISTRY_CONTRACT_CODE_HASHES",
            "legacy_registry_contracts.rs",
        ),
        (
            "tile",
            "LEGACY_TILE_CONTRACT_CODE_HASHES",
            "legacy_tile_contracts.rs",
        ),
        (
            "chat",
            "LEGACY_CHAT_CONTRACT_CODE_HASHES",
            "legacy_chat_contracts.rs",
        ),
    ] {
        freenet_migrate_build::codegen()
            .entry_registry(
                format!("../contracts/{crate_dir}-contract/legacy_contracts.toml"),
                Component::Contract,
            )
            .canonical_consts(false)
            .contract_hash_view(view)
            .out_file(out)
            .emit()
            .unwrap_or_else(|e| panic!("codegen legacy {crate_dir}-contract registry: {e}"));
    }
}
