//! [`App`] — the application context every operation resolves its dependencies from.
//!
//! # A type, not a registry
//!
//! Kynos resolves state by trait selection rather than by a runtime map: a handler asking for
//! `Inject<T>` against a context that provides no `T` does not typecheck, and the error lands at
//! the `mount` call. That is the whole reason this struct exists instead of a Salvo `Depot`,
//! whose `obtain::<AppState>()` returned an `Option` that every one of the old handlers turned
//! into `.expect("AppState is injected by middleware")` — twelve panics-in-waiting, one per
//! route, guarding an invariant nothing checked.
//!
//! # One field per module
//!
//! [`AuthContext`] is one field, not four, and `S-C1`'s [`UploadContext`] is the second field
//! beside it rather than five more loose collaborators. `#[derive(Provider)]` emits one
//! `Provides<T>` per field, and two fields of the same type is a derive error — so the shape
//! here is "one bundle per cohesive module", which is also the shape design/module-map.md
//! describes.
//!
//! [`App::new`] is the **only** constructor, and that is deliberate. There was a `with_auth`
//! convenience taking the auth module's four collaborators inline beside the other bundles,
//! which the two test fixtures were its only callers of. It grew by one argument per ported
//! surface and was at eight when it was removed: a signature that lengthens every time a module
//! is added is a signature that will eventually be got wrong positionally, and it was undoing
//! the one-bundle-per-module shape this type exists to hold. A caller builds
//! [`AuthContext`] like every other module's bundle.

use kynos::prelude::*;
use kynos::security::Authenticates;

use crate::album::AlbumContext;
use crate::attestation::AttestationContext;
use crate::auth::oidc::OidcContext;
use crate::auth::{AccessToken, AuthContext, TotpContext};
use crate::counter::CounterContext;
use crate::directory::DeviceDirectoryContext;
use crate::discovery::DiscoveryContext;
use crate::drop::DropContext;
use crate::enrollment::EnrollmentContext;
use crate::escrow::EscrowContext;
use crate::moderation::ModerationContext;
use crate::quota::QuotaContext;
use crate::serve::ServeContext;
use crate::share::ShareContext;
use crate::sync::SyncContext;
use crate::upload::UploadContext;
use crate::verify::VerifyContext;

/// Everything the server was built with.
///
/// Handed to [`Router::build`](kynos::Router::build) once and borrowed per request. It holds
/// handles, never connections: acquisition that can fail belongs in a handler body, where the
/// failure lands in the return type and therefore in the description.
///
/// `Clone` is cheap and shares everything: every field is a context whose own fields are `Arc`s,
/// so a clone is a handful of refcount bumps and both copies drive the *same* stores. That is
/// the property a test needs when it serves the assembled context on a socket while keeping a
/// handle on what is behind it (`S-D28`).
#[derive(Debug, Clone, Provider)]
pub struct App {
    /// The authentication module's collaborators.
    auth: AuthContext,
    /// The second factor's collaborators (`S-C55`).
    totp: TotpContext,
    /// The OIDC relying party's collaborators (`S-N1`).
    oidc: OidcContext,
    /// The upload module's collaborators.
    upload: UploadContext,
    /// The sync feed's collaborators.
    sync: SyncContext,
    /// The media serving module's collaborators.
    serve: ServeContext,
    /// The storage-verification module's collaborators.
    verify: VerifyContext,
    /// The device-directory module's collaborators.
    directories: DeviceDirectoryContext,
    /// The album-provisioning module's collaborators.
    albums: AlbumContext,
    /// The quota module's collaborators.
    quota: QuotaContext,
    /// The custody-receipt module's collaborators.
    attestation: AttestationContext,
    /// The public discovery record's collaborators.
    discovery: DiscoveryContext,
    /// The master-key escrow's collaborators.
    escrow: EscrowContext,
    /// The cross-device add's collaborators.
    enrollment: EnrollmentContext,
    /// The moderation record's collaborators.
    moderation: ModerationContext,
    /// The share-link surface's collaborators.
    share: ShareContext,
    /// The guest-drop surface's collaborators.
    drops: DropContext,
    /// The rate-limit counters (`S-C32`).
    counters: CounterContext,
}

/// The modules an [`App`] is assembled from.
///
/// One argument rather than eight positional ones, and named rather than ordered. `App::new`
/// grew by a parameter per ported surface until clippy refused it, which is the same failure
/// the removed `with_auth` had: a signature that lengthens every time a module is added will
/// eventually be got wrong positionally, and two contexts of similar shape swapped at a call
/// site is a compile error only by luck. Adding a module is now a field.
#[derive(Debug)]
pub struct Modules {
    /// The authentication module's collaborators.
    pub auth: AuthContext,
    /// The second factor's collaborators (`S-C55`).
    pub totp: TotpContext,
    /// The OIDC relying party's collaborators (`S-N1`).
    pub oidc: OidcContext,
    /// The upload module's collaborators.
    pub upload: UploadContext,
    /// The sync feed's collaborators.
    pub sync: SyncContext,
    /// The media serving module's collaborators.
    pub serve: ServeContext,
    /// The storage-verification module's collaborators.
    pub verify: VerifyContext,
    /// The device-directory module's collaborators.
    pub directories: DeviceDirectoryContext,
    /// The album-provisioning module's collaborators.
    pub albums: AlbumContext,
    /// The quota module's collaborators.
    pub quota: QuotaContext,
    /// The custody-receipt module's collaborators.
    pub attestation: AttestationContext,
    /// The public discovery record's collaborators.
    pub discovery: DiscoveryContext,
    /// The master-key escrow's collaborators.
    pub escrow: EscrowContext,
    /// The cross-device add's collaborators.
    pub enrollment: EnrollmentContext,
    /// The moderation record's collaborators.
    pub moderation: ModerationContext,
    /// The share-link surface's collaborators.
    pub share: ShareContext,
    /// The guest-drop surface's collaborators.
    pub drops: DropContext,
    /// The rate-limit counters (`S-C32`).
    pub counters: CounterContext,
}

impl App {
    /// Assembles the application from its modules.
    pub fn new(modules: Modules) -> Self {
        let Modules {
            auth,
            totp,
            oidc,
            upload,
            sync,
            serve,
            verify,
            directories,
            albums,
            quota,
            attestation,
            discovery,
            escrow,
            enrollment,
            moderation,
            share,
            drops,
            counters,
        } = modules;
        Self {
            auth,
            totp,
            oidc,
            upload,
            sync,
            serve,
            verify,
            directories,
            albums,
            quota,
            attestation,
            discovery,
            escrow,
            enrollment,
            moderation,
            share,
            drops,
            counters,
        }
    }
}

/// The auth module verifies the bearer credentials the auth module issued.
///
/// This association is what a router mounting an `Auth<AccessToken>` operation is checked
/// against: a context that cannot authenticate the scheme cannot build the router, so an
/// operation can never be guarded by a scheme nothing verifies.
impl Authenticates<AccessToken> for App {
    type Authenticator = AuthContext;

    fn authenticator(&self) -> &Self::Authenticator {
        &self.auth
    }
}
