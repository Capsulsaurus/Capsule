//! Compile the in-tree key-free sync proto into a **client-only** gRPC stub for
//! the SDK's sync consumer (slice `S-D2`). The `.proto` is owned by the sync
//! server crate (slice `S-C2`, SSoT); the SDK references it verbatim rather than
//! forking a copy, so the wire contract stays single-sourced. Server codegen is
//! disabled — the SDK never serves the feed, only consumes it.

use std::path::Path;

fn main() {
    let proto_root = Path::new("../capsule-api/sync/proto");
    let sync_proto = proto_root.join("capsule/sync/v1/sync.proto");

    // Rebuild if the single-sourced contract changes.
    println!("cargo:rerun-if-changed={}", sync_proto.display());

    tonic_prost_build::configure()
        .build_server(false)
        .build_client(true)
        .compile_protos(&[sync_proto.as_path()], &[proto_root])
        .expect("failed to compile the capsule.sync.v1 proto for the SDK client");
}
