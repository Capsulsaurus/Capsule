fn main() {
    tonic_prost_build::configure()
        .compile_protos(
            &[
                // The key-free sync feed contract (SLICES.md S-C2).
                "proto/capsule/sync/v1/sync.proto",
            ],
            &["proto"],
        )
        .expect("failed to compile protobuf definitions");
}
