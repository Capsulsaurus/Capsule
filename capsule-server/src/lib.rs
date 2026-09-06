//! The Capsule server: one Kynos REST/OpenAPI application.
//!
//! # Why this crate exists
//!
//! The previous server was Salvo, and its wire-contract types were themselves salvo-typed, so
//! replacing it was never a transport swap (`SLICES.md`, the salvo→kynos row). `S-C27` moved the
//! response taxonomy into the framework-free [`capsule_wire`]; this crate is where the surfaces
//! that taxonomy describes get rebuilt.
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
//! upload-session state stay behind separate Capsule-owned ports with Postgres, Valkey and
//! deterministic in-memory adapters; no generic CAS, transfer or TTL abstraction is planned.
//!
//! Each module owns one port and, where it has one, the surface over it. [`routes`] is the only
//! module that knows about HTTP: everything under it — [`album`], [`directory`], [`discovery`],
//! [`enrollment`], [`escrow`], [`gc`], [`index`], [`moderation`], [`quota`], [`scrub`], [`serve`],
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
//! # Every adapter is in-memory
//!
//! Every port in this crate has a deterministic in-memory adapter and a conformance suite, and
//! **no Postgres, Valkey or filesystem adapter is written** except the blob store's. That is a
//! deliberate ordering rather than an omission: the contract and its suite are what a real
//! adapter is written *against*, and a port with two implementations before it has one suite is
//! a port whose two implementations will disagree. It is also why this crate's whole test suite
//! runs without a container.

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
pub mod gc;
pub mod index;
pub mod limits;
pub mod moderation;
mod openapi;
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
use kynos::middleware::limits::BodySize;
use kynos::middleware::stack::Cons;
use kynos::prelude::*;
use kynos::router::service::Service;

pub use self::app::App;

/// Assembles the router.
///
/// One function, used by [`service`], by the OpenAPI emitter and by the conformance tests, so
/// all three describe the same server. A surface reachable in production but absent from the
/// tested router would be a surface nothing proves anything about.
///
/// It takes no context *value* — [`App`] appears only as a type parameter — which is what lets
/// [`openapi`] describe the whole server without a database, a cache or a signing key. The
/// description is a property of the types, not of a running deployment.
pub fn router() -> ServerRouter {
    Router::<App>::new()
        // **Outermost.** Kynos runs a chain head-first, so this sees every problem the rest of
        // the chain produces — the body-size `413` below it, an extractor's `400`/`415`/`422`,
        // and the bearer scheme's `401`/`403` — and fills in the `error.*` code none of those
        // framework-owned types has a seam to carry. See [`problem`], and `S-C36`.
        .intercept(problem::CodedProblems::new())
        // Mounted on the whole router, not on the operations that happen to take a body today:
        // an oversized body is refused wherever it is sent, and the `413` that refusal produces
        // is declared on every operation it covers because Kynos derives the declaration from
        // the interceptor's own type. See [`limits`].
        .intercept(limits::body_size())
        // Seven `mount` calls, not one. Kynos's `EndpointSet` is implemented for tuples up to
        // sixteen and the seventeenth operation is a compile error, so a split is forced — but
        // grouping by surface rather than cutting at the arbitrary boundary is what makes the
        // next addition obvious rather than a puzzle. Each group is well under the cap, so a
        // new operation joins the surface it belongs to instead of wherever there is room.
        // The account: who you are, what devices you have, and how you get your key back.
        .mount(kynos::routes![
            routes::version::get_version,
            routes::auth::register_user,
            routes::auth::login_user,
            routes::auth::refresh_token,
            routes::auth::logout,
            routes::auth::revoke_all_challenge,
            routes::auth::revoke_all,
            routes::devices::list_devices,
            routes::devices::revoke_session,
            routes::directory::publish_device_directory,
            routes::directory::fetch_device_directory,
            routes::escrow::store_escrow,
            routes::escrow::fetch_escrow,
            routes::auth::reauthenticate,
        ])
        // What an account knows about itself, and the credentials it opens sessions with.
        .mount(kynos::routes![
            routes::profile::get_profile,
            routes::profile::update_profile,
            routes::profile::change_password,
            routes::totp::totp_enroll,
            routes::totp::totp_verify_enrollment,
            routes::totp::totp_disable,
            routes::totp::totp_verify_login,
        ])
        // The cross-device add: one code, one channel, and the two devices' mailboxes.
        .mount(kynos::routes![
            routes::enroll::issue_enrollment_code,
            routes::enroll::redeem_enrollment_code,
            routes::enroll::relay_enrollment_payload,
            routes::enroll::drain_enrollment_channel,
            routes::enroll::close_enrollment_channel,
        ])
        // The library's own surfaces, and the public record anybody may read.
        .mount(kynos::routes![
            routes::albums::provision_album,
            routes::upgrade::begin_album_upgrade,
            routes::upgrade::album_upgrade_phase,
            routes::upgrade::abort_album_upgrade,
            routes::quota::get_quota,
            routes::moderation::moderation_record,
            routes::well_known::attestation_keys,
            routes::well_known::server_info,
            routes::well_known::deprecation_announcements,
            routes::well_known::revoked_jti,
        ])
        // The asset surfaces: getting bytes in, changing what they mean, and reading them back.
        .mount(kynos::routes![
            routes::upload::create_upload,
            routes::upload::append_chunk,
            routes::sessions::list_upload_sessions,
            routes::upload::head_upload,
            routes::upload::cancel_upload,
            routes::receipts::get_upload_receipt,
            routes::ops::apply_op,
            routes::sync::sync_feed,
            routes::blob::get_blob,
            routes::storage::verify_storage,
            routes::assets::get_asset_receipts,
        ])
        // Share links: two owner operations, and the one path served without an account.
        .mount(kynos::routes![
            routes::share::issue_share,
            routes::share::revoke_share,
            routes::share::share_metadata,
            routes::share::share_wrapped_secret,
            routes::share::share_blob,
        ])
        // Guest drops: the owner's link, the guest's deposit, and the inbox between them.
        .mount(kynos::routes![
            routes::drop::provision_link,
            routes::drop::revoke_link,
            routes::drop::create_drop,
            routes::drop::append_drop_chunk,
            routes::drop::list_inbox,
            routes::drop::adopt_drop,
            routes::drop::discard_drop,
        ])
}

/// The router's full type, interceptors included.
///
/// Spelled out because a Kynos interceptor is part of the router's *type* — that is what makes
/// two interceptors answering with one status a compile error rather than a runtime surprise —
/// so mounting one changes this signature. That is a feature: the alias is the one place the
/// server's middleware stack is written down.
pub type ServerRouter = Router<App, Propagate, Cons<BodySize, Cons<problem::CodedProblems, ()>>>;

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
/// `capsule-sdk/openapi.json` is checked in and generated from by spargen, so its version must
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
/// [`SPEC_VERSION`] cannot express.
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
    Ok(document)
}
