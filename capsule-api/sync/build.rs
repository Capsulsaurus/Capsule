fn main() {
    tonic_prost_build::configure()
        .compile_protos(
            &[
                // LEGACY-PLAINTEXT (frozen): SLICES.md S-G2 — the pre-E2EE metadata
                // service, retiring once the key-free feed below reaches parity.
                "proto/photolibrary/metadata/v1/metadata.proto",
                // The key-free sync feed contract (SLICES.md S-C2).
                "proto/capsule/sync/v1/sync.proto",
            ],
            &["proto"],
        )
        .expect("failed to compile protobuf definitions");
}
