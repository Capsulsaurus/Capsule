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

use std::sync::Arc;

use kynos::prelude::*;
use kynos::security::Authenticates;

use crate::auth::{AccessToken, AccountDirectory, AuthContext, SessionTokens};
use crate::serve::ServeContext;
use crate::store::{AuthStateStore, Clock};
use crate::sync::SyncContext;
use crate::upload::UploadContext;

/// Everything the server was built with.
///
/// Handed to [`Router::build`](kynos::Router::build) once and borrowed per request. It holds
/// handles, never connections: acquisition that can fail belongs in a handler body, where the
/// failure lands in the return type and therefore in the description.
#[derive(Debug, Provider)]
pub struct App {
    /// The authentication module's collaborators.
    auth: AuthContext,
    /// The upload module's collaborators.
    upload: UploadContext,
    /// The sync feed's collaborators.
    sync: SyncContext,
    /// The media serving module's collaborators.
    serve: ServeContext,
}

impl App {
    /// Assembles the application from its modules.
    ///
    /// Takes already-built module bundles rather than their parts, so that adding a
    /// collaborator to a module is not a change to this signature.
    pub fn new(
        auth: AuthContext,
        upload: UploadContext,
        sync: SyncContext,
        serve: ServeContext,
    ) -> Self {
        Self {
            auth,
            upload,
            sync,
            serve,
        }
    }

    /// Assembles the application from the auth module's four collaborators and the upload
    /// module's own bundle.
    ///
    /// The convenience form, for a caller that is wiring the whole server rather than composing
    /// modules.
    pub fn with_auth(
        sessions: Arc<dyn AuthStateStore>,
        accounts: Arc<dyn AccountDirectory>,
        tokens: Arc<SessionTokens>,
        clock: Arc<dyn Clock>,
        upload: UploadContext,
        sync: SyncContext,
        serve: ServeContext,
    ) -> Self {
        Self::new(
            AuthContext::new(sessions, accounts, tokens, clock),
            upload,
            sync,
            serve,
        )
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
