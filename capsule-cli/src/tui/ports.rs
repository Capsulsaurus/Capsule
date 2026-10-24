//! The TUI's side effects, behind one port trait.
//!
//! The reducer names the work it wants as an [`Effect`]; [`perform`] runs it against a
//! [`Ports`] implementation and turns the outcome into the [`Action`] that reports it. The real
//! terminal uses [`LocalPorts`]; tests hand in a mock so the same reducer, keymap and view run
//! without a session file, a library or a network.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use capsule_sdk::events::{TransferEvent, TransferEvents};
use ratatui_image::picker::Picker;
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};

use super::auth::{AuthAction, AuthEffect, LoginResult};
use super::form::Secret;
use super::import::{ImportAction, ImportEffect, ImportRequest};
use super::label_editor::{EditRequest, Edited, LabelAction, LabelEffect, REGION_PREVIEW_EDGE};
use super::library::{
    Catalogue, EncodeRequest, EncodedPreview, LibraryAction, LibraryEffect, NO_IMAGE_PROTOCOL,
    OpenRequest, PREVIEW_EDGE, PreviewImage, PreviewRequest,
};
use super::metadata_repair::{RepairAction, RepairEffect};
use super::push::{PushAction, PushEffect, PushProgress, PushRequest, is_custody_step};
use super::restore::{RestoreAction, RestoreEffect, RestoreRequest};
use super::share::{CreateLinkRequest, IssuedLink, ShareAction, ShareEffect};
use super::state::{Action, Effect, SessionSummary};
use super::sync::{SyncAction, SyncEffect};
use crate::import_report::ImportReport;
use crate::remote::{self, PushSummary, RemoteConfig, SyncSummary};
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

    /// The transfer event handle the transfer screen watches. Every upload, push or fetch the
    /// TUI starts attaches this handle to its client, so the screen sees it. The default
    /// publishes nothing.
    fn transfer_events(&self) -> TransferEvents {
        TransferEvents::disabled()
    }

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
        code: Secret,
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

    /// Import the picked sources into a library, copying them, as `capsule import` does. A run
    /// that reached its files answers `Ok`, failed files included.
    fn import(
        &self,
        request: ImportRequest,
    ) -> impl Future<Output = Result<ImportReport, String>> + Send {
        let _ = request;
        async { Err(NOT_OFFERED.to_owned()) }
    }

    /// Push a library, or the assets `request` names, as `capsule push [--asset ID…]` does,
    /// telling the run's progress to [`push_progress`](Self::push_progress) as it goes and
    /// closing it with [`PushProgress::Closed`] for `request.run` on every way out. A run that
    /// reached its assets answers `Ok`, failed assets included.
    fn push(
        &self,
        request: PushRequest,
    ) -> impl Future<Output = Result<PushSummary, String>> + Send {
        let _ = request;
        async { Err(NOT_OFFERED.to_owned()) }
    }

    /// The next batch of push progress, waiting for at least one item. The default tells no
    /// progress: it answers at once that there is none to come.
    fn push_progress(&self) -> impl Future<Output = Vec<PushProgress>> + Send {
        async { vec![PushProgress::Closed { run: None }] }
    }

    /// Unlock a library, snapshot it for the browser and close it again. Writes nothing but
    /// the default-album resolution every library verb shares on open.
    fn open_library(
        &self,
        request: OpenRequest,
    ) -> impl Future<Output = Result<Catalogue, String>> + Send {
        let _ = request;
        async { Err(NOT_OFFERED.to_owned()) }
    }

    /// Decode an asset's original into a preview no larger than [`PREVIEW_EDGE`] on its long
    /// edge, through `capsule-core::media`. `Err` says why there is none, including a terminal
    /// that draws no images.
    fn preview(
        &self,
        request: PreviewRequest,
    ) -> impl Future<Output = Result<PreviewImage, String>> + Send {
        let _ = request;
        async { Err(NOT_OFFERED.to_owned()) }
    }

    /// Encode a decoded preview in the terminal's graphics protocol, fitted to the pane the
    /// request names. Runs off the UI loop; `Err` (including a panicking encoder) draws the
    /// pane as text.
    fn encode_preview(
        &self,
        request: EncodeRequest,
    ) -> impl Future<Output = Result<EncodedPreview, String>> + Send {
        let _ = request;
        async { Err(NO_IMAGE_PROTOCOL.to_owned()) }
    }

    /// The terminal's image protocol picker, queried once at start-up; `None` (the default)
    /// draws previews as text.
    fn image_picker(&self) -> Option<Picker> {
        None
    }

    /// Unlock the library, write one edit through [`crate::edit::apply`] (what `capsule label`,
    /// `capsule region` and `capsule edit` run), snapshot it and close it again.
    fn apply_edit(
        &self,
        request: EditRequest,
    ) -> impl Future<Output = Result<Edited, String>> + Send {
        let _ = request;
        async { Err(NOT_OFFERED.to_owned()) }
    }

    /// Decode an asset's original into a preview no larger than [`REGION_PREVIEW_EDGE`] for
    /// the label editor to draw regions on as half blocks: unlike
    /// [`preview`](Self::preview), whatever image protocol the terminal speaks.
    fn region_preview(
        &self,
        request: PreviewRequest,
    ) -> impl Future<Output = Result<PreviewImage, String>> + Send {
        let _ = request;
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
        Effect::Import(ImportEffect::Run(request)) => {
            Action::Import(ImportAction::Finished(ports.import(request).await))
        }
        Effect::Push(PushEffect::Run(request)) => {
            Action::Push(PushAction::Finished(ports.push(request).await))
        }
        Effect::Push(PushEffect::Watch) => {
            Action::Push(PushAction::Progress(ports.push_progress().await))
        }
        Effect::Library(LibraryEffect::Open(request)) => {
            Action::Library(LibraryAction::Opened(ports.open_library(request).await))
        }
        Effect::Library(LibraryEffect::Preview(request)) => {
            let asset_id = request.asset_id;
            Action::Library(LibraryAction::PreviewLoaded {
                asset_id,
                result: ports.preview(request).await,
            })
        }
        Effect::Library(LibraryEffect::Encode(request)) => {
            let (generation, size) = (request.generation, request.size);
            Action::Library(LibraryAction::PreviewEncoded {
                generation,
                size,
                result: ports.encode_preview(request).await,
            })
        }
        Effect::Labels(LabelEffect::Apply(request)) => {
            Action::Labels(LabelAction::Applied(ports.apply_edit(request).await))
        }
        Effect::Labels(LabelEffect::Preview(request)) => {
            let asset_id = request.asset_id;
            Action::Labels(LabelAction::PreviewLoaded {
                asset_id,
                result: ports.region_preview(request).await,
            })
        }
        Effect::Repair(RepairEffect::Apply(request)) => {
            Action::Repair(RepairAction::Applied(ports.apply_edit(request).await))
        }
    };
    tracing::debug!(target: "capsule_cli::tui", ?effect, ?action, "effect finished");
    action
}

/// The ports a real `capsule` run uses: the session file, the local store and the server the
/// CLI's own commands use, through the same functions those commands call, and one transfer
/// event channel for every transfer the TUI starts.
#[derive(Debug, Clone)]
pub struct LocalPorts {
    push_progress: Arc<ProgressChannel>,
    session_path: Option<PathBuf>,
    remote: RemoteConfig,
    transfer_events: TransferEvents,
    picker: Option<Picker>,
}

impl LocalPorts {
    /// Ports over the session file at `session_path`, against the server the environment names
    /// ([`RemoteConfig::from_env`]); `None` when no config directory could be resolved, which
    /// every port then reports.
    #[must_use]
    pub fn new(session_path: Option<PathBuf>) -> Self {
        Self {
            push_progress: Arc::new(ProgressChannel::new()),
            session_path,
            remote: RemoteConfig::from_env(),
            transfer_events: TransferEvents::default_channel(),
            picker: None,
        }
    }

    /// These ports with the image protocol picker the terminal answered with
    /// ([`super::library::query_picker`]); `None` keeps previews as text.
    #[must_use]
    pub fn with_picker(mut self, picker: Option<Picker>) -> Self {
        self.picker = picker;
        self
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

/// The most progress items one [`Ports::push_progress`] answer carries, so a burst is folded in
/// a few actions without one answer growing without bound.
pub const PROGRESS_BATCH: usize = 256;

/// Where a push run tells its progress and the push screen's watch reads it: one channel for
/// every run, read by one watch at a time.
#[derive(Debug)]
pub struct ProgressChannel {
    tx: mpsc::UnboundedSender<PushProgress>,
    rx: Mutex<mpsc::UnboundedReceiver<PushProgress>>,
}

impl ProgressChannel {
    /// An empty channel.
    #[must_use]
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            tx,
            rx: Mutex::new(rx),
        }
    }

    /// A sender for a run to tell its progress through.
    #[must_use]
    pub fn sender(&self) -> mpsc::UnboundedSender<PushProgress> {
        self.tx.clone()
    }

    /// Wait for the next item, then take whatever else is already queued, up to
    /// [`PROGRESS_BATCH`] in all.
    pub async fn next_batch(&self) -> Vec<PushProgress> {
        let mut rx = self.rx.lock().await;
        // The channel holds its own sender, so it never closes under the watch.
        let Some(first) = rx.recv().await else {
            return vec![PushProgress::Closed { run: None }];
        };
        let mut batch = vec![first];
        while batch.len() < PROGRESS_BATCH {
            match rx.try_recv() {
                Ok(item) => batch.push(item),
                Err(_) => break,
            }
        }
        batch
    }
}

impl Default for ProgressChannel {
    fn default() -> Self {
        Self::new()
    }
}

/// Tells [`PushProgress::Closed`] for a run when dropped, so the watch hears the run is over on
/// every way out of it: an answer, an error, or a panic unwinding through the port.
struct CloseOnDrop {
    tx: mpsc::UnboundedSender<PushProgress>,
    run: u64,
}

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        let _ = self.tx.send(PushProgress::Closed {
            run: Some(self.run),
        });
    }
}

/// Relay the custody steps of a run's transfer events ([`is_custody_step`]) to `tx` until
/// `run_done` fires (or its sender is dropped) and what was published before it is relayed, or
/// the channel closes. The channel is the TUI's shared one, which outlives every run, so the run
/// ending is what stops the relay. A subscriber that fell behind tells how many it missed.
pub async fn forward_custody_events(
    mut events: broadcast::Receiver<TransferEvent>,
    tx: mpsc::UnboundedSender<PushProgress>,
    mut run_done: oneshot::Receiver<()>,
) {
    use tokio::sync::broadcast::error::{RecvError, TryRecvError};
    loop {
        let received = tokio::select! {
            // What is already published goes first, so nothing the run told is left behind.
            biased;
            received = events.recv() => received,
            _ = &mut run_done => {
                // The run returned, so everything it published is already in the channel:
                // relay that much and stop, even though the channel itself stays open.
                loop {
                    let received = match events.try_recv() {
                        Ok(event) => Ok(event),
                        Err(TryRecvError::Lagged(missed)) => Err(RecvError::Lagged(missed)),
                        Err(TryRecvError::Empty | TryRecvError::Closed) => return,
                    };
                    if !relay(received, &tx) {
                        return;
                    }
                }
            }
        };
        if matches!(received, Err(RecvError::Closed)) || !relay(received, &tx) {
            return;
        }
    }
}

/// Relay one received transfer event to `tx` if it is a custody step; whether `tx` is still
/// read.
fn relay(
    received: Result<TransferEvent, broadcast::error::RecvError>,
    tx: &mpsc::UnboundedSender<PushProgress>,
) -> bool {
    use tokio::sync::broadcast::error::RecvError;
    let progress = match received {
        Ok(event) if is_custody_step(&event) => PushProgress::Transfer(event),
        Ok(_) | Err(RecvError::Closed) => return true,
        Err(RecvError::Lagged(missed)) => {
            tracing::warn!(target: "capsule_cli::tui", missed, "push progress fell behind the transfer events");
            PushProgress::Lagged(missed)
        }
    };
    tx.send(progress).is_ok()
}

/// The library browser's snapshot of `workspace`, with the origin ledger's line beside each
/// asset handed to another device and not yet held by the server.
fn snapshot(workspace: &capsule_library::lifecycle::Workspace) -> Catalogue {
    super::library::catalogue::collect(workspace)
        .with_delegations(crate::peer::delegation_lines_for(workspace.device_id()))
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
    fn transfer_events(&self) -> TransferEvents {
        self.transfer_events.clone()
    }

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

    async fn verify_second_factor(&self, mfa_token: Secret, code: Secret) -> Result<(), String> {
        let (remote, store) = (self.remote.clone(), self.store()?);
        off_loop(move || async move {
            let mfa_token = secrecy::SecretString::from(mfa_token.expose().to_owned());
            remote::auth_verify_totp(&remote, &store, &mfa_token, code.expose().trim())
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

    async fn import(&self, request: ImportRequest) -> Result<ImportReport, String> {
        // Unlocking the library and sealing each file are CPU work: off the async workers.
        off_loop(move || async move {
            let mut workspace =
                crate::open_workspace_with(&request.library, request.passphrase.expose())
                    .map_err(|e| e.to_string())?;
            crate::import_run::import_sources(
                &mut workspace,
                request.kind.provider(),
                &request.sources,
            )
            .map_err(|e| e.to_string())
        })
        .await
    }

    async fn push(&self, request: PushRequest) -> Result<PushSummary, String> {
        let tx = self.push_progress.sender();
        // Closed for this run however the port leaves, before the session store is even read.
        let closed = CloseOnDrop {
            tx: tx.clone(),
            run: request.run,
        };
        let (remote, store) = (self.remote.clone(), self.store()?);
        // The TUI's one transfer channel: the transfer screen watches this run's uploads on it
        // too, and the relay below reads its custody steps for the push screen.
        let events = self.transfer_events.clone();
        off_loop(move || async move {
            let _closed = closed;
            let mut workspace =
                crate::open_workspace_with(&request.library, request.passphrase.expose())
                    .map_err(|e| e.to_string())?;
            let (run_done, done) = oneshot::channel();
            let forwarder = events.subscribe().map(|subscriber| {
                tokio::spawn(forward_custody_events(subscriber, tx.clone(), done))
            });
            let mut observe = |progress: remote::AssetProgress| {
                let _ = tx.send(progress.into());
            };
            let run = remote::PushRun {
                assets: request.assets,
                events,
                observer: Some(&mut observe),
                delegated: crate::peer::delegated_for_push(workspace.device_id()),
            };
            let pushed = remote::push_run(
                &remote,
                &store,
                &mut workspace,
                crate::push_options(false, false, false),
                run,
            )
            .await;
            // The relay passes on what the run published and ends; only then is the run's
            // stream closed.
            let _ = run_done.send(());
            if let Some(forwarder) = forwarder {
                let _ = forwarder.await;
            }
            pushed.map_err(|error| remote_reason(&error))
        })
        .await
    }

    async fn push_progress(&self) -> Vec<PushProgress> {
        self.push_progress.next_batch().await
    }

    async fn open_library(&self, request: OpenRequest) -> Result<Catalogue, String> {
        // Unlocking runs Argon2: off the async workers. The workspace is dropped, and the
        // library's lock with it, as soon as the snapshot is taken.
        off_loop(move || async move {
            let workspace =
                crate::open_workspace_with(&request.library, request.passphrase.expose())
                    .map_err(|e| e.to_string())?;
            Ok(snapshot(&workspace))
        })
        .await
    }

    async fn preview(&self, request: PreviewRequest) -> Result<PreviewImage, String> {
        // Decoding an original the terminal cannot show would be wasted work.
        if self.picker.is_none() {
            return Err(NO_IMAGE_PROTOCOL.to_owned());
        }
        super::library::previewable(&request.ext)?;
        tokio::task::spawn_blocking(move || {
            let bytes = std::fs::read(&request.original)
                .map_err(|e| format!("could not read the original: {e}"))?;
            super::library::decode_preview(
                &capsule_core::media::RawshiftDecoder,
                &bytes,
                &request.ext,
                PREVIEW_EDGE,
            )
        })
        .await
        .map_err(|e| format!("the preview decode did not finish: {e}"))?
    }

    async fn encode_preview(&self, request: EncodeRequest) -> Result<EncodedPreview, String> {
        let Some(picker) = self.picker.clone() else {
            return Err(NO_IMAGE_PROTOCOL.to_owned());
        };
        // Resizing and Sixel quantization take long enough on a large pane to stall input:
        // off the async workers, behind the encode's own unwind guard.
        tokio::task::spawn_blocking(move || super::library::encode_preview(&picker, request))
            .await
            .map_err(|e| format!("the preview encode did not finish: {e}"))?
    }

    fn image_picker(&self) -> Option<Picker> {
        self.picker.clone()
    }

    async fn apply_edit(&self, request: EditRequest) -> Result<Edited, String> {
        // Unlocking runs Argon2 and the edit signs and seals: off the async workers. The
        // workspace, and the library's lock with it, is dropped once the snapshot is taken.
        off_loop(move || async move {
            let EditRequest {
                library,
                passphrase,
                edit,
            } = request;
            let mut workspace = crate::open_workspace_with(&library, passphrase.expose())
                .map_err(|e| e.to_string())?;
            let report = crate::edit::apply(&mut workspace, &edit)
                .map_err(|error| crate::edit::describe_error(&error))?;
            Ok(Edited {
                summary: crate::edit::summary(&report),
                catalogue: snapshot(&workspace),
                library,
            })
        })
        .await
    }

    async fn region_preview(&self, request: PreviewRequest) -> Result<PreviewImage, String> {
        super::library::previewable(&request.ext)?;
        tokio::task::spawn_blocking(move || {
            let bytes = std::fs::read(&request.original)
                .map_err(|e| format!("could not read the original: {e}"))?;
            super::library::decode_preview(
                &capsule_core::media::RawshiftDecoder,
                &bytes,
                &request.ext,
                REGION_PREVIEW_EDGE,
            )
        })
        .await
        .map_err(|e| format!("the preview decode did not finish: {e}"))?
    }
}

#[cfg(test)]
mod tests;
