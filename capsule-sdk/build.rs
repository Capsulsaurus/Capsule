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

    // spargen 0.3 split the old `Config` into `Spec` (what to read) and `Build` (how to emit),
    // and made `Report::outcome` a method. `Spec::build` is the one-line form the crate prefers.
    //
    // Four operations are narrowed out rather than generated. spargen 0.4 validates the
    // document strictly and rejects each as a structure violation; spargen 0.1.0 accepted them,
    // which is the only reason they were ever in the committed contract.
    //
    //   POST /v1/albums/{album_id}/ops        `responses` is `{}`
    //   GET  /v1/auth/devices/directory/{user_id}
    //   GET  /v1/auth/devices/enroll/channel/{channel_id}
    //   POST /v1/auth/devices/enroll/channel/{channel_id}
    //                                         path template declares no path parameters
    //
    // The lifecycle-ops handler returns `()` and writes into `&mut Response` by hand, choosing
    // its success status at run time (`StatusCode::from_u16(result.status)`) so an idempotent
    // replay returns the stored bytes verbatim — so salvo-oapi has no return type to describe.
    // The three device routes take their parameter from the path template without declaring it,
    // so a generated client would have nowhere to put it.
    //
    // All four are therefore *already* uncallable from a typed client, which is why the SDK
    // hand-writes `capsule_sdk::directory` at all. Narrowed rather than repaired here on
    // purpose: repairing salvo-oapi annotations is work thrown away, and Kynos makes both
    // classes unrepresentable — status is part of the return type, and `#[kynos::get(..)]`
    // checks at compile time that the path type's fields are exactly the template's variables.
    // Recorded against `S-C16`/`S-C28` and as acceptance criteria for the Kynos port; the
    // hand-written directory client goes when the rebuilt schema declares these properly.
    // The instruction below stands — narrow the surface, never mutilate the spec.
    let omitted = [
        spargen::OmitRule::operation(spargen::OmitMethod::Post, "/v1/albums/{album_id}/ops"),
        spargen::OmitRule::operation(
            spargen::OmitMethod::Get,
            "/v1/auth/devices/directory/{user_id}",
        ),
        spargen::OmitRule::operation(
            spargen::OmitMethod::Get,
            "/v1/auth/devices/enroll/channel/{channel_id}",
        ),
        spargen::OmitRule::operation(
            spargen::OmitMethod::Post,
            "/v1/auth/devices/enroll/channel/{channel_id}",
        ),
    ];
    let spec_cfg = omitted
        .into_iter()
        .fold(spargen::Spec::new(spec.clone()), spargen::Spec::omit_rule);
    let report = spargen::generate(&spec_cfg.build(out));

    // `Cached` is a success: spargen verified the already-rendered module against the build
    // cache and had no work to do. Demanding `Generated` alone would fail every incremental
    // build that legitimately had nothing to regenerate. `Rejected` is the failure to catch.
    assert!(
        matches!(
            report.outcome(),
            spargen::Outcome::Generated | spargen::Outcome::Cached
        ),
        "spargen could not generate the REST client from {spec}: {report:#?}. \
         The schema is OpenAPI 3.2 by contract and is never downgraded — not to 3.1, and \
         certainly not to 3.0. If spargen rejects a construct, fix the schema or narrow the \
         surface with `spargen::omit!` — never mutilate the spec."
    );
}
