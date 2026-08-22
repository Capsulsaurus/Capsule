use environment::Environment;
use eyre::Result;
use salvo::oapi::security::{Http, HttpAuthScheme, SecurityScheme};
use salvo::oapi::{Info, License, OpenApi, Tag};
use salvo::prelude::*;
use sea_orm::DatabaseConnection;

pub mod routes;

/// OpenAPI tag constants
pub mod tags {
    pub const API: &str = "api";
    pub const AUTH: &str = "auth";
    pub const UPLOAD: &str = "upload";
    pub const MEDIA: &str = "media";
    pub const SHARE: &str = "share";
    pub const STORAGE: &str = "storage";
    pub const DROPS: &str = "drops";
    pub const SYNC: &str = "sync";
}

/// Create OpenAPI specification with proper metadata
pub fn create_openapi_spec() -> OpenApi {
    let info = Info::new("Capsule API", "0.1.0")
        .description("Capsule API Documentation")
        .license(
            License::new("GNU Affero General Public License v3.0 or later")
                .url("https://www.gnu.org/licenses/agpl-3.0.html"),
        );

    OpenApi::with_info(info)
        .tags([
            Tag::new(tags::API).description("Capsule API"),
            Tag::new(tags::AUTH).description("Capsule Authentication API"),
            Tag::new(tags::UPLOAD).description("Capsule Upload API"),
            Tag::new(tags::MEDIA).description("Capsule Media Serving API"),
            Tag::new(tags::SHARE).description("Capsule Public Share API"),
            Tag::new(tags::STORAGE).description("Capsule Storage Verification API"),
            Tag::new(tags::DROPS).description("Capsule Web-Upload Drops API"),
            Tag::new(tags::SYNC).description("Capsule Sync API (gRPC)"),
        ])
        .add_security_scheme(
            "bearer",
            SecurityScheme::Http(
                Http::new(HttpAuthScheme::Bearer)
                    .bearer_format("JWT")
                    .description("JWT Bearer token authentication"),
            ),
        )
}

/// Create the main router for the API
pub async fn create_router(conn: DatabaseConnection, env: &Environment) -> Result<Router> {
    let mut v1_router = Router::new();

    #[cfg(feature = "auth")]
    {
        v1_router = v1_router.push(
            Router::with_path("auth").push(auth::get_router(conn.clone(), &env.server).await?),
        );
    }
    #[cfg(feature = "upload")]
    {
        v1_router = v1_router
            .push(
                Router::with_path("upload")
                    .push(upload::get_router(conn.clone(), &env.server).await?),
            )
            // The `/albums` tree lives at the API root, not under `/upload`, per the
            // authorization contract: `POST /albums` provisions a client's derived album id
            // (slice S-C25) and `POST /albums/{album_id}/ops` is the generic lifecycle-write
            // surface (slice S-C16).
            .push(
                Router::with_path("albums")
                    .push(upload::get_ops_router(conn.clone(), &env.server).await?),
            );
    }
    #[cfg(feature = "media")]
    {
        v1_router = v1_router
            .push(
                Router::with_path("s")
                    .push(media::get_share_router(conn.clone(), &env.server).await?),
            )
            // Key-free ranged ciphertext serving by content address (slice S-C10).
            .push(
                Router::with_path("blob")
                    .push(media::get_blob_router(conn.clone(), &env.server).await?),
            )
            // Key-free durability verdicts + signed attestation (slices S-C3 / S-C15).
            .push(
                Router::with_path("storage")
                    .push(media::get_storage_router(conn.clone(), &env.server).await?),
            )
            // Durable custody-receipt log per asset (slice S-C15).
            .push(
                Router::with_path("assets")
                    .push(media::get_receipts_router(conn.clone(), &env.server).await?),
            )
            // Guest drop sessions + owner inbox (slice S-C5 in SLICES.md).
            .push(
                Router::with_path("u")
                    .push(media::get_drop_link_router(conn.clone(), &env.server).await?),
            )
            .push(
                Router::with_path("drops")
                    .push(media::get_drops_router(conn.clone(), &env.server).await?),
            );
    }

    // Add version endpoint
    v1_router = v1_router.push(Router::with_path("version").get(routes::version::get_version));

    // Wrap API routes in /v1 prefix
    let v1_router = Router::with_path("v1").push(v1_router);

    // The attestation-key publication (slice S-C15) lives at the server root, not under /v1,
    // so it sits at the canonical `.well-known/capsule/*` path peers and clients expect.
    #[cfg(feature = "media")]
    let well_known_router = Router::with_path(".well-known").push(
        Router::with_path("capsule")
            .push(media::get_well_known_router(conn.clone(), &env.server).await?),
    );

    // Build the final router
    let root = Router::new().push(v1_router);
    #[cfg(feature = "media")]
    let root = root.push(well_known_router);

    // The gRPC sync service mounts at the ROOT, not under /v1 — gRPC addresses a method by
    // its fully-qualified path (`/capsule.sync.v1.SyncService/Sync`), and tonic's client
    // discards any path on the endpoint URI: `AddOrigin` keeps only the scheme and authority
    // and lets the generated stub write the path. So a prefixed mount is unreachable from
    // every native client, no matter how the endpoint is configured. Versioning is not lost —
    // the proto package already carries it (`capsule.sync.v1`). Exercised end to end by
    // slice S-C2 (SLICES.md).
    #[cfg(feature = "sync")]
    let root = root.push(sync::get_router(conn.clone(), &env.server).await?);

    let router;
    #[cfg(feature = "openapi")]
    {
        // Build OpenAPI documentation (at root level, not under /v1)
        let doc = create_openapi_spec().merge_router(&root);

        router = root
            .push(doc.into_router("/openapi.json"))
            .push(SwaggerUi::new("/openapi.json").into_router("/swagger-ui"))
            .push(Scalar::new("/openapi.json").into_router("/openapi"));
    }
    #[cfg(not(feature = "openapi"))]
    {
        router = root;
    }

    Ok(router)
}

/// Assemble the router used **only** to extract the server's OpenAPI 3.1 schema (slice
/// `S-D8`), with no injected state, so it builds with no database, Valkey, key material,
/// disk, or network — the prerequisite for a deterministic dump that runs anywhere,
/// including the Rust check gate's `openapi-check` drift step.
///
/// It mirrors [`create_router`]'s `/v1` nesting exactly (the sibling above), pushing each
/// crate's state-free route tree (`*::openapi_router`) at the identical sub-path so the
/// operation paths are byte-identical to what the live server serves. The two must be kept
/// in lockstep; each crate single-sources its route *shape* so only the top-level mounting
/// is restated here.
///
/// Deliberately absent (each is invisible to salvo-oapi and so carries no schema):
/// - the gRPC `sync` service (bare `#[handler]` goals),
/// - the media share (`/s`), guest-drop (`/u`, `/drops`), and `.well-known` routers, and
///   the passkey routes — all bare `#[handler]`s (the recorded known limitation).
pub fn openapi_router() -> Router {
    let mut v1_router = Router::new();

    #[cfg(feature = "auth")]
    {
        v1_router = v1_router.push(Router::with_path("auth").push(auth::openapi_router()));
    }
    #[cfg(feature = "upload")]
    {
        v1_router = v1_router
            .push(Router::with_path("upload").push(upload::openapi_router()))
            // `POST /albums` (slice S-C25) and `POST /albums/{album_id}/ops` (slice S-C16)
            // live at the API root, mirroring `create_router`.
            .push(Router::with_path("albums").push(upload::openapi_ops_router()));
    }
    #[cfg(feature = "media")]
    {
        // The media crate's `#[endpoint]`-bearing trees, pre-nested under `blob` /
        // `storage` / `assets` exactly as `create_router` mounts them.
        v1_router = v1_router.push(media::routes::schema_router());
    }

    v1_router = v1_router.push(Router::with_path("version").get(routes::version::get_version));

    Router::new().push(Router::with_path("v1").push(v1_router))
}

// Re-export dependency crates if needed by binaries
#[cfg(feature = "auth")]
pub use auth;
#[cfg(feature = "media")]
pub use media;
#[cfg(feature = "sync")]
pub use sync;
#[cfg(feature = "upload")]
pub use upload;
