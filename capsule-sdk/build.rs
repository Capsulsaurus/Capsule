//! Build-time code generation for the SDK's two typed transports:
//!
//! 1. **gRPC sync stub (slice `S-D2`).** The in-tree key-free sync proto is compiled into a
//!    **client-only** stub. The `.proto` is owned by the sync server crate (slice `S-C2`,
//!    SSoT); the SDK references it verbatim rather than forking a copy, so the wire contract
//!    stays single-sourced. Server codegen is disabled — the SDK never serves the feed.
//!
//! 2. **REST client (slice `S-D8`).** `spargen`, our in-house OpenAPI **3.1** generator,
//!    lowers the committed `openapi.json` (dumped from the salvo-oapi server by
//!    `capsule-api`'s `gen_openapi` bin; regenerate with `mise run openapi`) into a typed,
//!    freestanding `reqwest` client emitted into `OUT_DIR` and `include!`d by
//!    [`crate::rest`]. Build-time generation makes the client a pure function of the
//!    committed spec — it is regenerated on every build, so a checked-in client can never
//!    drift from the spec (the meaningful drift, spec vs. live server, is the `openapi-check`
//!    gate's job). spargen never enters the SDK's *runtime* dependency tree: the runtime
//!    support code is embedded verbatim into the generated module. spargen fails the build
//!    loudly and precisely (naming the exact construct + JSON Pointer) on any 3.1 construct
//!    it cannot yet lower — we never downgrade the schema to 3.0 to appease a generator.

use std::path::{Path, PathBuf};

fn main() {
    build_sync_stub();
    build_rest_client();
}

/// Compile the key-free sync proto into a client-only gRPC stub (slice `S-D2`).
fn build_sync_stub() {
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

/// Generate the typed REST client from the committed OpenAPI 3.1 schema (slice `S-D8`).
fn build_rest_client() {
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo"),
    );
    let spec = manifest_dir.join("openapi.json");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo"))
        .join("rest_client.rs");

    // Regenerate whenever the committed schema changes.
    println!("cargo:rerun-if-changed={}", spec.display());

    // spargen speaks `camino::Utf8PathBuf`; our paths are UTF-8 (repo + `OUT_DIR`), so a
    // lossless string hop is the conversion — a non-UTF-8 path here is a broken toolchain.
    let spec = spec
        .into_os_string()
        .into_string()
        .expect("UTF-8 spec path");
    let out = out
        .into_os_string()
        .into_string()
        .expect("UTF-8 OUT_DIR path");

    let report = spargen::generate(&spargen::Config::new(
        spec.clone(),
        spargen::OutputTarget::Module(out.into()),
    ));

    assert!(
        report.outcome == spargen::Outcome::Generated,
        "spargen could not generate the REST client from {spec}: {report:#?}. \
         The schema is OpenAPI 3.1 by contract and is never downgraded to 3.0; if spargen \
         rejects a construct, fix the schema or narrow the surface with `spargen::omit!` — \
         never mutilate the spec."
    );
}
