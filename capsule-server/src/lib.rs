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

pub mod album;
pub mod app;
pub mod auth;
pub mod blob;
pub mod body;
pub mod directory;
pub mod gc;
pub mod index;
pub mod limits;
pub mod quota;
pub mod routes;
pub mod scrub;
pub mod serve;
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
        // Mounted on the whole router, not on the operations that happen to take a body today:
        // an oversized body is refused wherever it is sent, and the `413` that refusal produces
        // is declared on every operation it covers because Kynos derives the declaration from
        // the interceptor's own type. See [`limits`].
        .intercept(limits::body_size())
        .mount(kynos::routes![
            routes::version::get_version,
            routes::auth::login_user,
            routes::auth::refresh_token,
            routes::auth::logout,
            routes::upload::create_upload,
            routes::upload::append_chunk,
            routes::upload::head_upload,
            routes::upload::cancel_upload,
            routes::sync::sync_feed,
            routes::blob::get_blob,
            routes::storage::verify_storage,
            routes::directory::publish_device_directory,
            routes::directory::fetch_device_directory,
            routes::ops::apply_op,
            routes::albums::provision_album,
            routes::quota::get_quota,
        ])
}

/// The router's full type, interceptors included.
///
/// Spelled out because a Kynos interceptor is part of the router's *type* — that is what makes
/// two interceptors answering with one status a compile error rather than a runtime surprise —
/// so mounting one changes this signature. That is a feature: the alias is the one place the
/// server's middleware stack is written down.
pub type ServerRouter = Router<App, Propagate, Cons<BodySize, ()>>;

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
    router().openapi_as(SPEC_VERSION)
}
