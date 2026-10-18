//! The Capsule server: one Kynos REST/OpenAPI application.
//!
//! # Why this crate exists
//!
//! The previous server was Salvo, and its wire-contract types were themselves salvo-typed, so
//! replacing it was never a transport swap (`SLICES.md`, the salvo→kynos row). `S-C27` moved the
//! response taxonomy into a framework-free crate so the contract could outlive the transport;
//! that crate retired with the Salvo tree it adapted (ADR-0004), and this crate is where the
//! surfaces the taxonomy described get rebuilt. `problem`, `limits` and `body` own it now.
//!
//! # What the framework buys, and why it was chosen
//!
//! Kynos derives the description from the types the server runs on. There is no second
//! declaration of a status, a path parameter or a body shape, so there is nothing for the
//! description to drift from. That matters here specifically: the Salvo surface had **thirteen
//! response variants that rendered a status the published schema never declared** (`S-C28`) —
//! a `Writer` said `423`, its `EndpointOutRegister` never registered one, and the generated
//! client could not map it. That failure is not expressible in this crate, because the status
//! is part of the return type.
//!
//! # Structure
//!
//! One application composed from cohesive internal modules — not separate public transports or
//! microservices (design/module-map.md, "Planned Server Modules"). Authentication state and
//! upload-session state stay behind separate Capsule-owned ports whose adapters are Valkey and a
//! deterministic in-memory double — **not** Postgres, which design/filesystem/server.md rejects
//! for a session table — while the durable records go to Postgres. No generic CAS, transfer or
//! TTL abstraction is planned.
//!
//! Each module owns one port and, where it has one, the surface over it. [`routes`](mod@routes) is the only
//! module that knows about HTTP: everything under it — [`album`], [`directory`], [`discovery`],
//! [`enrollment`], [`escrow`], [`federation`], [`gc`], [`index`], [`membership`],
//! [`moderation`],
//! [`negotiation`], [`quota`],
//! [`scrub`], [`serve`],
//! [`share`], [`store`],
//! [`sync`], [`upload`],
//! [`verify`] — is framework-free and testable without a router, which is why the operator
//! workers ([`gc`], [`scrub`]) have no wire surface at all and cost nothing to exercise.
//!
//! # How a process is assembled
//!
//! [`config`] reads what an operator decided; [`boot`] turns it into the one [`App`] the router
//! is built with. Both are library modules rather than binary code, because a composition root
//! that lives in `main` is a composition root nothing tests: `boot::assemble` is driven by unit
//! tests here and by the binary identically, so "the server can be built at all" is an
//! assertion rather than something discovered on a deployment.
//!
//! # Every port has a double, and four of them have Postgres besides
//!
//! Every port in this crate has a deterministic in-memory adapter and a conformance suite, and
//! that ordering was deliberate rather than an omission: the contract and its suite are what a
//! real adapter is written *against*, and a port with two implementations before it has one
//! suite is a port whose two implementations will disagree.
//!
//! Every durable port now has the second implementation on Postgres (#402, #446) and every
//! volatile one on Valkey (#403), each passing the same case list as its double — the two OIDC
//! stores included since #460, so a durable boot that configures OIDC runs on them.
//!
//! **The suite still runs without a container.** Every Postgres case is gated on
//! `CAPSULE_TEST_POSTGRES=1` and prints one line naming itself when it is skipped, so
//! `cargo nextest run` on a machine with no container runtime is green and says what it did not
//! prove — which is the acceptance gap design/module-map.md sets for the framework.

pub mod album;
pub mod app;
pub mod attestation;
pub mod auth;
pub mod blob;
pub mod body;
pub mod boot;
pub mod cli;
pub mod config;
pub mod counter;
pub mod directory;
pub mod discovery;
pub mod drop;
pub mod enrollment;
pub mod escrow;
pub mod federation;
pub mod gc;
pub mod index;
pub mod limits;
pub mod membership;
pub mod moderation;
pub mod negotiation;
mod openapi;
pub mod postgres;
pub mod problem;
pub mod quota;
pub mod routes;
pub mod scrub;
pub mod serve;
pub mod share;
pub mod store;
pub mod sync;
pub mod upload;
pub mod verify;

use kynos::middleware::catch_panic::Propagate;
use kynos::middleware::stack::Cons;
use kynos::prelude::*;
use kynos::router::group::Group;
use kynos::router::service::Service;

pub use self::app::App;

/// Assembles the router.
///
/// One function, used by [`service`], by the OpenAPI emitter and by the conformance tests, so
/// all three describe the same server. A surface reachable in production but absent from the
/// tested router would be a surface nothing proves anything about.
///
/// It takes no context *value* — [`App`] appears only as a type parameter — which is what lets
/// [`openapi`](fn@openapi) describe the whole server without a database, a cache or a signing key. The
/// description is a property of the types, not of a running deployment.
pub fn router() -> ServerRouter {
    Router::<App>::new()
        // **Outermost.** Kynos runs a chain head-first, so this sees every problem the rest of
        // the chain produces — the body-size `413` below it, an extractor's `400`/`415`/`422`,
        // and the bearer scheme's `401`/`403` — and fills in the `error.*` code none of those
        // framework-owned types has a seam to carry. See [`problem`], and `S-C36`.
        .intercept(problem::CodedProblems::new())
        // Inside the coder and outside everything that can refuse: the protocol window rides
        // **every** response — the body-size `413` below, an extractor's `400`, the bearer
        // scheme's `401`, the gate's own `426` — because each of those is produced beneath this
        // and passes back up through it. See [`negotiation`].
        .intercept(negotiation::Negotiation::new())
        // The body-size cap is **not** mounted here. It sits on every group below instead — each
        // group carrying the cap its own operations need — because Kynos refuses two `413`
        // producers on one route, so a router-wide cap would leave no group room for a tighter
        // one (#478). Every operation is still covered by exactly one, including the twelve the
        // protocol gate exempts, which get groups holding nothing but the cap. The group's cap
        // is its **first** interceptor, so it stays outermost beneath `Negotiation` and an
        // oversized body is refused before the gate reads anything. See [`limits`].
        // The protocol gate is two `Group`s, not a router interceptor, for two reasons the type
        // system makes concrete. First, the design exempts twelve operations, and a group is how
        // Kynos spells "these and not those": an operation mounted inside declares the handshake
        // parameters and the gate's statuses, one in the cap-only group below does not and still
        // carries the response headers. Second, the design holds a **write** to the window (a
        // grammatical date outside it is `426`) and a **read** to the grammar only ("reads of any
        // past version succeed", threat-model/validation.md) — and since an interceptor's
        // declaration is its type, a read operation must sit behind a gate whose `Short` has no
        // `426` in it, or the document would promise a status the read never renders. So the
        // non-safe operations sit behind `ProtocolGate` and the `GET`/`HEAD` ones behind
        // `ProtocolReadGate`. `tests/conformance.rs` pins both sets and the exempt twelve against
        // the emitted document and walks every operation on the wire, so a route cannot change
        // gate by accident.
        //
        // Several `mount` calls per group, not one. Kynos's `EndpointSet` is implemented for
        // tuples up to sixteen and the seventeenth operation is a compile error, so a split is
        // forced — but grouping by surface rather than cutting at the arbitrary boundary is what
        // makes the next addition obvious rather than a puzzle.
        //
        // The gated writes are two groups split by **cap**, not by surface (#576): everything
        // whose largest legitimate body is bounded carries [`limits::bounded_body_size`], and only
        // the chunk-carrying and protocol-unbounded writes, in the group after it, keep the
        // backstop.
        .group(
            Group::<App>::new("/")
                .intercept(limits::bounded_body_size())
                .intercept(negotiation::ProtocolGate::new())
                // The account: opening sessions and revoking them all. Refreshing and closing one
                // are exempt, in the cap-only group at the end.
                .mount(kynos::routes![
                    routes::auth::register_user,
                    routes::auth::login_user,
                    routes::auth::revoke_all_challenge,
                    routes::auth::revoke_all,
                    routes::auth::reauthenticate,
                    routes::devices::revoke_session,
                    routes::directory::publish_device_directory,
                    routes::escrow::store_escrow,
                ])
                // What an account changes about itself, its second factor, and the other way
                // a session is opened: through an external identity provider (`S-N1`).
                .mount(kynos::routes![
                    routes::profile::update_profile,
                    routes::profile::change_password,
                    routes::totp::totp_enroll,
                    routes::totp::totp_verify_enrollment,
                    routes::totp::totp_disable,
                    routes::totp::totp_verify_login,
                    routes::oidc::begin_oidc_login,
                    routes::oidc::complete_oidc_login,
                ])
                // The cross-device add: one code, one channel, and the writes into it.
                .mount(kynos::routes![
                    routes::enroll::issue_enrollment_code,
                    routes::enroll::redeem_enrollment_code,
                    routes::enroll::relay_enrollment_payload,
                    routes::enroll::close_enrollment_channel,
                ])
                // The library's own bounded writes: albums, upgrades, rosters, verification.
                .mount(kynos::routes![
                    routes::albums::provision_album,
                    routes::upgrade::begin_album_upgrade,
                    routes::upgrade::abort_album_upgrade,
                    routes::roster::publish_album_roster,
                    routes::upload::cancel_upload,
                    routes::storage::verify_storage,
                ])
                // Share links and guest drops: the owner's bounded writes on both.
                .mount(kynos::routes![
                    routes::share::revoke_share,
                    routes::drop::provision_link,
                    routes::drop::revoke_link,
                    routes::drop::adopt_drop,
                    routes::drop::discard_drop,
                ]),
        )
        // The gated writes that keep the backstop. The two chunk routes, so the upload route
        // answers a breach of the protocol's chunk bound itself with
        // `413 error.upload.chunk_too_large`; `apply_op`, whose `metadata-update` carries the
        // encrypted sidecar inline and nothing bounds a sidecar's size; and `issue_share`, whose
        // `serves` enumerates every blob a whole-album link may serve. See [`limits::body_size`].
        .group(
            Group::<App>::new("/")
                .intercept(limits::body_size())
                .intercept(negotiation::ProtocolGate::new())
                .mount(kynos::routes![
                    routes::upload::create_upload,
                    routes::upload::append_chunk,
                    routes::ops::apply_op,
                    routes::share::issue_share,
                ]),
        )
        // Federation's lifecycle: minting, revoking and refreshing the capability a peer pulls
        // with, and the signed report intake. The pull itself is `sync_feed` and `get_blob` in
        // the read group below — federation adds no new data protocol (design/federation.md).
        //
        // Its own group rather than a fifth `mount` on the one above, for two reasons: these four
        // are one surface and the sixteen-operation tuple ceiling is close, and they carry a
        // **tighter body cap** than the rest of the writes — no body here is anywhere near an
        // upload chunk, and `submit_federated_report` is the server's only unauthenticated write.
        // See [`crate::limits::MAX_FEDERATION_BODY_BYTES`].
        .group(
            Group::<App>::new("/")
                .intercept(limits::federation_body_size())
                .intercept(negotiation::ProtocolGate::new())
                .mount(kynos::routes![
                    routes::federation::issue_capability,
                    routes::federation::revoke_capability,
                    routes::federation::refresh_capability,
                    routes::federation::submit_federated_report,
                ]),
        )
        // The reads: every gated `GET` and `HEAD`. Held to the handshake's grammar, admitted at
        // any protocol date, and declaring the `400` alone. None takes a body, so the bounded cap.
        .group(
            Group::<App>::new("/")
                .intercept(limits::bounded_body_size())
                .intercept(negotiation::ProtocolReadGate::new())
                .mount(kynos::routes![
                    routes::devices::list_devices,
                    routes::directory::fetch_device_directory,
                    routes::escrow::fetch_escrow,
                    routes::profile::get_profile,
                    routes::enroll::drain_enrollment_channel,
                    routes::upgrade::album_upgrade_phase,
                    routes::quota::get_quota,
                    routes::moderation::moderation_record,
                    routes::upload::head_upload,
                    routes::sessions::list_upload_sessions,
                    routes::receipts::get_upload_receipt,
                    routes::sync::sync_feed,
                    routes::blob::get_blob,
                    routes::assets::get_asset_receipts,
                    routes::drop::list_inbox,
                ]),
        )
        // **The twelve operations the design exempts from the gate**, in a group of their
        // own so neither gated group above covers them (`api-surfaces.md`, "Negotiation Across
        // Transports"). `GET /v1/version` is the reachability probe a client hits before it
        // knows the window. The four `/.well-known/capsule/*` records are public discovery,
        // read before any handshake. The three `/s/{opaque_id}*` share reads must answer an
        // indistinguishable `404` (share-links.md), which a `426` would turn into a probing
        // oracle. The two `/d/{opaque_id}*` guest deposits have their protocol pinned at link
        // issuance (web-upload.md), so a browser guest has nothing to assert. Refreshing and
        // closing a session (`POST /v1/auth/refresh`, `POST /v1/auth/logout`) are session
        // upkeep, not library writes: reads are admitted at any protocol date, so a `426` on the
        // refresh that keeps the read's access token alive would sign an out-of-window client
        // out of the reads it is still allowed, and a `426` on logout would leave open a session
        // its holder asked to close (#527). Opening one (`login`, `register`) stays gated. All
        // twelve still carry the response headers, because `Negotiation` is the router's.
        //
        // Two groups holding the body-size cap and nothing else: the cap is per-group, and these
        // twelve are still covered by one each. The ten that are not drop routes take the
        // bounded cap — reads, and two session-upkeep writes carrying a token; the two drop
        // routes keep the backstop, because a guest's drop chunk is as large as an upload chunk.
        .group(
            Group::<App>::new("/")
                .intercept(limits::bounded_body_size())
                .mount(kynos::routes![
                    routes::version::get_version,
                    routes::well_known::attestation_keys,
                    routes::well_known::server_info,
                    routes::well_known::deprecation_announcements,
                    routes::well_known::revoked_jti,
                    routes::share::share_metadata,
                    routes::share::share_wrapped_secret,
                    routes::share::share_blob,
                    routes::auth::refresh_token,
                    routes::auth::logout,
                ]),
        )
        .group(
            Group::<App>::new("/")
                .intercept(limits::body_size())
                .mount(kynos::routes![
                    routes::drop::create_drop,
                    routes::drop::append_drop_chunk,
                ]),
        )
}

/// The router's full type, interceptors included.
///
/// Spelled out because a Kynos interceptor is part of the router's *type* — that is what makes
/// two interceptors answering with one status a compile error rather than a runtime surprise —
/// so mounting one changes this signature. That is a feature: the alias is the one place the
/// server's router-wide middleware stack is written down. The body-size cap is not in it: each
/// group carries its own (see [`router`] and [`limits`]).
pub type ServerRouter =
    Router<App, Propagate, Cons<negotiation::Negotiation, Cons<problem::CodedProblems, ()>>>;

/// Builds the service the server and the in-process tests both drive.
///
/// Kynos's `TestClient` drives a built `Service` directly — no socket, no port, no runtime
/// flavour — which is why the test suite needs no container for anything above the storage
/// ports. That is the "test harnesses without live infrastructure" acceptance gap
/// design/module-map.md sets for the framework.
///
/// # Errors
///
/// Returns an error if the router cannot be built — a route whose declared parameters do not
/// match its path template, two operations claiming the same method and path, or a guarded
/// operation whose context cannot authenticate the scheme guarding it.
pub fn service(context: App) -> kynos::Result<Service<App>> {
    router().build(context)
}

/// The specification version Capsule's document targets.
///
/// **3.2, pinned deliberately.** `Router::openapi()` would emit the *lowest* version that
/// expresses the API without loss — a good default, and not what a committed contract wants.
/// `capsule-server/openapi.json` is checked in and generated from by spargen, so its version must
/// be a decision rather than a consequence: left to follow the API it would flip 3.1 → 3.2 the
/// day the first streamed response lands, churning the schema gate and regenerating the client
/// for a change nobody asked for. Kynos names this exact case — "reach for this when a
/// consumer's toolchain pins a version" — and `openapi_as` targets rather than downgrades, so a
/// construct 3.2 cannot express is an error listing what blocks it, never a document with
/// operations quietly missing.
const SPEC_VERSION: kynos::openapi::SpecVersion = kynos::openapi::SpecVersion::V3_2;

/// Emits the OpenAPI document for the assembled router.
///
/// This is the only path from the code to a description: there is no document to hand-edit and
/// therefore none to forget. `capsule-sdk` consumes the committed result, so it is a contract,
/// not a report.
///
/// # Errors
///
/// Returns an error if the router's types cannot be described, or if the API uses a construct
/// `SPEC_VERSION` cannot express.
pub fn openapi() -> kynos::Result<kynos::openapi::Document> {
    let mut document = router().openapi_as(SPEC_VERSION)?;
    // `S-C38`: Kynos attaches `#[problem(extension)]` members at run time and describes none of
    // them, so every problem response would otherwise point at one generic `Problem` with
    // `additionalProperties: true` — and a generated client would lose the `code` the i18n
    // contract tells it to localize. See [`openapi`](crate::openapi) the module.
    openapi::describe_problem_extensions(&mut document);
    // `S-Z7`: Kynos describes a raw-byte body as the empty schema, which is true and useless to
    // a generator. Filled in with the binary marker so the SDK's client can be generated from
    // the whole document instead of most of it.
    openapi::describe_raw_byte_payloads(&mut document);
    // Issue #404: Kynos describes an interceptor's response headers on success responses only,
    // while [`negotiation::Negotiation`] attaches them to every response it forwards — errors
    // included. The walk files the same three declarations under every other response, so the
    // document promises exactly what the wire carries.
    openapi::describe_negotiation_headers(&mut document);
    Ok(document)
}
