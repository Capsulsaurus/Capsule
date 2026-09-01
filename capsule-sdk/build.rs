//! Build-time code generation for the SDK's **one** typed transport (slice `S-D8`, `S-Z7`).
//!
//! `spargen`, our in-house OpenAPI generator, lowers the committed
//! `capsule-server/openapi.json` — the **Kynos** document, emitted deterministically from the
//! server's own types and gated by `mise run openapi-check-kynos` — into a typed, freestanding
//! `reqwest` client emitted into `OUT_DIR` and `include!`d by [`crate::rest`]. Build-time
//! generation makes the client a pure function of the committed contract: it is regenerated on
//! every build, so a checked-in client can never drift from the document, and the document
//! cannot drift from the server. spargen never enters the SDK's *runtime* dependency tree — the
//! runtime support is embedded verbatim into the generated module — and it fails the build
//! loudly and precisely (naming the construct and its JSON Pointer) on anything it cannot
//! lower. The schema is never downgraded to appease a generator.
//!
//! **There is no second transport.** The gRPC sync stub this file used to build is gone with
//! the `capsule.sync.v1` service: the feed is `GET /v1/sync` and is a generated operation like
//! every other. One document, one client, one wire.

use std::path::PathBuf;

fn main() {
    build_rest_client();
}

/// Generate the typed REST client from the committed OpenAPI 3.1 schema (slice `S-D8`).
fn build_rest_client() {
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo"),
    );
    // The **Kynos** document, not this crate's own copy. One contract, one file: the SDK
    // generates from exactly the bytes `mise run openapi-check-kynos` gates, so there is no
    // second copy to keep in step and no window in which the client is generated from a
    // document the server no longer serves.
    let spec = manifest_dir.join("../capsule-server/openapi.json");
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
    // **The Salvo document's four narrowings are gone**, and not because they were repaired:
    // Kynos makes both defects unrepresentable. `POST /v1/albums/{album_id}/ops` declared
    // `responses: {}` because its handler chose a status at run time, and status is part of the
    // return type here. The three device routes took a path parameter their template never
    // declared, and `#[kynos::get(..)]` checks at compile time that a path type's fields are
    // exactly the template's variables. All four are generated now.
    //
    // **Four different ones take their place, and for a reason outside the contract** (`S-D28`).
    // spargen's `classify_media` knows JSON, XML, multipart, form-urlencoded, octet-stream,
    // event-stream, NDJSON, JSON sequences and `text/*` — and no `application/cbor`. Capsule
    // serves four operations in that media type, all of them documents that are **signed** and
    // therefore served byte-for-byte, which is exactly why they are not JSON.
    //
    //   POST /v1/auth/devices/directory      the signed device directory in
    //   GET  /v1/auth/devices/directory/{user_id}   …and back out, verbatim
    //   POST /v1/albums/{album_id}/upgrade   the signed upgrade intent
    //   GET  /v1/upload/{id}/receipt         the signed custody receipt
    //
    // Narrowing them is the instruction's own remedy — *narrow the surface, never mutilate the
    // spec* — and the alternative is worse in a way worth naming: re-labelling them
    // `application/octet-stream` to satisfy a generator would tell every client that a document
    // with a schema it knows is opaque bytes, which is the thing the media type exists to deny.
    // `capsule_sdk::directory` already hand-writes two of them for the old reason; the other two
    // have no client yet.
    let omitted = [
        spargen::OmitRule::operation(spargen::OmitMethod::Post, "/v1/auth/devices/directory"),
        spargen::OmitRule::operation(
            spargen::OmitMethod::Get,
            "/v1/auth/devices/directory/{user_id}",
        ),
        spargen::OmitRule::operation(spargen::OmitMethod::Post, "/v1/albums/{album_id}/upgrade"),
        spargen::OmitRule::operation(spargen::OmitMethod::Get, "/v1/upload/{id}/receipt"),
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
