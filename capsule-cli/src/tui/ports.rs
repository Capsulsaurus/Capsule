//! The TUI's side effects, behind one port trait.
//!
//! The reducer names the work it wants as an [`Effect`]; [`perform`] runs it against a
//! [`Ports`] implementation and turns the outcome into the [`Action`] that reports it. The real
//! terminal uses [`LocalPorts`]; tests hand in a mock so the same reducer, keymap and view run
//! without a session file, a library or a network.

use std::future::Future;
use std::path::PathBuf;

use super::auth::{AuthAction, AuthEffect, LoginResult};
use super::form::Secret;
use super::restore::{RestoreAction, RestoreEffect, RestoreRequest};
use super::share::{CreateLinkRequest, IssuedLink, ShareAction, ShareEffect};
use super::state::{Action, Effect, SessionSummary};
use super::sync::{SyncAction, SyncEffect};
use crate::remote::{self, RemoteConfig, SyncSummary};
use crate::restore::RestoreSummary;
use crate::session::SessionStore;
use crate::status::AuthStatus;

/// The reason a port gives for work its implementation does not offer.
pub const NOT_OFFERED: &str = "this build of the TUI cannot do that";

/// Everything the TUI asks of the outside world.
///
/// Each method returns its failure as the English reason the screen shows: the TUI is
/// English-only, and a port error is something a person reads, not something the reducer
/// branches on.
///
/// Every method but [`session_summary`](Self::session_summary) has a default answering
/// [`NOT_OFFERED`], so a test double implements only what its test drives; [`LocalPorts`]
/// implements them all.
pub trait Ports: Send + Sync + 'static {
    /// The local session summary, as `capsule auth status` reads it: never a network call.
    fn session_summary(&self) -> impl Future<Output = Result<SessionSummary, String>> + Send;

    /// Sign in with a password, storing the session on success, as `capsule auth login` does.
    fn login(
        &self,
        email: String,
        password: Secret,
    ) -> impl Future<Output = Result<LoginResult, String>> + Send {
        let _ = (email, password);
        async { Err(NOT_OFFERED.to_owned()) }
    }

    /// Answer a second-factor challenge with an authenticator code, storing the session.
    fn verify_second_factor(
        &self,
        mfa_token: Secret,
        code: String,
    ) -> impl Future<Output = Result<(), String>> + Send {
        let _ = (mfa_token, code);
        async { Err(NOT_OFFERED.to_owned()) }
    }

    /// Sign out, as `capsule auth logout` does: whether a session was signed out.
    fn logout(&self) -> impl Future<Output = Result<bool, String>> + Send {
        async { Err(NOT_OFFERED.to_owned()) }
    }

    /// Drain the sync feed into the local store, as `capsule sync [--force]` does.
    fn sync(&self, from_start: bool) -> impl Future<Output = Result<SyncSummary, String>> + Send {
        let _ = from_start;
        async { Err(NOT_OFFERED.to_owned()) }
    }

    /// Restore originals, as `capsule restore` does. A run that reached its assets answers
    /// `Ok`, failed assets included; `Err` is a run that could not start.
    fn restore(
        &self,
        request: RestoreRequest,
    ) -> impl Future<Output = Result<RestoreSummary, String>> + Send {
        let _ = request;
        async { Err(NOT_OFFERED.to_owned()) }
    }

    /// Issue a share link, as `capsule share create` does.
    fn create_link(
        &self,
        request: CreateLinkRequest,
    ) -> impl Future<Output = Result<IssuedLink, String>> + Send {
        let _ = request;
        async { Err(NOT_OFFERED.to_owned()) }
    }

    /// Revoke a share link by its opaque id, as `capsule share revoke` does: the id revoked.
    fn revoke_link(
        &self,
        opaque_id: String,
    ) -> impl Future<Output = Result<String, String>> + Send {
        let _ = opaque_id;
        async { Err(NOT_OFFERED.to_owned()) }
    }
}

/// Perform `effect` through `ports` and return the action that reports its outcome.
pub async fn perform<P: Ports>(ports: &P, effect: Effect) -> Action {
    tracing::debug!(target: "capsule_cli::tui", ?effect, "performing effect");
    let action = match effect.clone() {
        Effect::LoadSession => Action::SessionLoaded(ports.session_summary().await),
        Effect::Auth(AuthEffect::Login { email, password }) => Action::Auth(
            AuthAction::LoginFinished(ports.login(email, password).await),
        ),
        Effect::Auth(AuthEffect::VerifySecondFactor { mfa_token, code }) => Action::Auth(
            AuthAction::SecondFactorFinished(ports.verify_second_factor(mfa_token, code).await),
        ),
        Effect::Auth(AuthEffect::Logout) => {
            Action::Auth(AuthAction::SignOutFinished(ports.logout().await))
        }
        Effect::Sync(SyncEffect::Run { from_start }) => {
            Action::Sync(SyncAction::Finished(ports.sync(from_start).await))
        }
        Effect::Restore(RestoreEffect::Run(request)) => {
            Action::Restore(RestoreAction::Finished(ports.restore(request).await))
        }
        Effect::Share(ShareEffect::Create(request)) => {
            Action::Share(ShareAction::Created(ports.create_link(request).await))
        }
        Effect::Share(ShareEffect::Revoke { opaque_id }) => {
            Action::Share(ShareAction::Revoked(ports.revoke_link(opaque_id).await))
        }
    };
    tracing::debug!(target: "capsule_cli::tui", ?effect, ?action, "effect finished");
    action
}

/// The ports a real `capsule` run uses: the session file, the local store and the server the
/// CLI's own commands use, through the same functions those commands call.
#[derive(Debug, Clone)]
pub struct LocalPorts {
    session_path: Option<PathBuf>,
    remote: RemoteConfig,
}

impl LocalPorts {
    /// Ports over the session file at `session_path`, against the server the environment names
    /// ([`RemoteConfig::from_env`]); `None` when no config directory could be resolved, which
    /// every port then reports.
    #[must_use]
    pub fn new(session_path: Option<PathBuf>) -> Self {
        Self {
            session_path,
            remote: RemoteConfig::from_env(),
        }
    }

    fn session_path(&self) -> Result<PathBuf, String> {
        self.session_path
            .clone()
            .ok_or_else(|| "could not resolve the config directory".to_owned())
    }

    fn store(&self) -> Result<SessionStore, String> {
        self.session_path().map(SessionStore::new)
    }
}

/// The English reason for a networked failure, as the CLI's own lines word it.
fn remote_reason(error: &remote::RemoteError) -> String {
    if matches!(error, remote::RemoteError::NotAuthenticated) {
        return crate::NOT_AUTHENTICATED.to_owned();
    }
    crate::describe_remote_error(error)
}

/// Run `work` on a blocking thread that drives it to completion on the current runtime.
///
/// The CLI's library flows hold a `Workspace` (an SQLite connection, not `Sync`) across awaits,
/// so their futures are not `Send` and cannot be spawned; unlocking a library also runs Argon2,
/// which must not sit on an async worker. Either way the input loop is never behind it.
async fn off_loop<T, F, Fut>(work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, String>>,
{
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || handle.block_on(work()))
        .await
        .map_err(|e| format!("the task did not finish: {e}"))?
}

impl Ports for LocalPorts {
    async fn session_summary(&self) -> Result<SessionSummary, String> {
        let path = self.session_path()?;
        // A file read: off the async worker so the input loop is never behind it.
        let status =
            tokio::task::spawn_blocking(move || AuthStatus::check(&SessionStore::new(path)))
                .await
                .map_err(|e| format!("the session read did not finish: {e}"))?
                .map_err(|e| format!("could not read the session: {e}"))?;
        Ok(if status.signed_in {
            SessionSummary::SignedIn {
                access_token_fresh: status.access_token_fresh,
            }
        } else {
            SessionSummary::SignedOut
        })
    }

    async fn login(&self, email: String, password: Secret) -> Result<LoginResult, String> {
        let (remote, store) = (self.remote.clone(), self.store()?);
        off_loop(move || async move {
            match remote::auth_login(&remote, &store, &email, password.expose()).await {
                Ok(remote::LoginStep::Complete) => Ok(LoginResult::SignedIn),
                Ok(remote::LoginStep::SecondFactorRequired { mfa_token }) => {
                    use secrecy::ExposeSecret as _;
                    Ok(LoginResult::SecondFactor {
                        mfa_token: Secret::new(mfa_token.expose_secret()),
                    })
                }
                Err(error) => Err(remote_reason(&error)),
            }
        })
        .await
    }

    async fn verify_second_factor(&self, mfa_token: Secret, code: String) -> Result<(), String> {
        let (remote, store) = (self.remote.clone(), self.store()?);
        off_loop(move || async move {
            let mfa_token = secrecy::SecretString::from(mfa_token.expose().to_owned());
            remote::auth_verify_totp(&remote, &store, &mfa_token, code.trim())
                .await
                .map_err(|error| remote_reason(&error))
        })
        .await
    }

    async fn logout(&self) -> Result<bool, String> {
        let (remote, store) = (self.remote.clone(), self.store()?);
        off_loop(move || async move {
            remote::auth_logout(&remote, &store)
                .await
                .map_err(|error| remote_reason(&error))
        })
        .await
    }

    async fn sync(&self, from_start: bool) -> Result<SyncSummary, String> {
        let (remote, store) = (self.remote.clone(), self.store()?);
        off_loop(move || async move {
            let db = crate::db::init_sqlite()
                .await
                .map_err(|e| format!("could not open the local store: {e}"))?;
            remote::sync(
                &remote,
                &store,
                &db,
                remote::DEFAULT_SYNC_PAGE_SIZE,
                false,
                from_start,
            )
            .await
            .map_err(|error| remote_reason(&error))
        })
        .await
    }

    async fn restore(&self, request: RestoreRequest) -> Result<RestoreSummary, String> {
        let (remote, store) = (self.remote.clone(), self.store()?);
        off_loop(move || async move {
            let workspace =
                crate::open_workspace_with(&request.library, request.passphrase.expose())
                    .map_err(|e| e.to_string())?;
            let options = crate::restore_options(request.scope, request.out);
            remote::restore(&remote, &store, &workspace, &options)
                .await
                .map_err(|error| remote_reason(&error))
        })
        .await
    }

    async fn create_link(&self, request: CreateLinkRequest) -> Result<IssuedLink, String> {
        let (remote, store) = (self.remote.clone(), self.store()?);
        off_loop(move || async move {
            let mut workspace =
                crate::open_workspace_with(&request.library, request.passphrase.expose())
                    .map_err(|e| e.to_string())?;
            let create = crate::share::CreateRequest {
                scope: request.scope,
                expires_at: request.expires_at,
                passphrase: request.link_passphrase.map(|p| p.expose().to_owned()),
            };
            crate::share::create(
                &remote,
                &store,
                &mut workspace,
                &create,
                jiff::Timestamp::now(),
            )
            .await
            .map(IssuedLink::from)
            .map_err(|error| crate::share::describe_error(&error))
        })
        .await
    }

    async fn revoke_link(&self, opaque_id: String) -> Result<String, String> {
        let (remote, store) = (self.remote.clone(), self.store()?);
        off_loop(move || async move {
            crate::share::revoke(&remote, &store, &opaque_id)
                .await
                .map_err(|error| crate::share::describe_error(&error))
        })
        .await
    }
}

#[cfg(test)]
mod tests;
