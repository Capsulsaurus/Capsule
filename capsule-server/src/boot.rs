//! [`assemble`] — the one composition root, and the only place adapters are chosen.
//!
//! # Why this is a library module and not `main`
//!
//! Until this change the only composition root in the tree was `tests/support/mod.rs`, which
//! assembles seventeen module contexts out of test doubles. Nothing assembled the server. A
//! composition root that lives in `main` is a composition root nothing tests: "the router
//! builds", "every port has an adapter" and "the published signing key is the one the tokens
//! verify under" are all properties of *this function*, and they are asserted below rather than
//! discovered on a deployment.
//!
//! # The seam
//!
//! Selection is a two-arm `match` on [`Backends`] and not a trait. The `Arc<dyn Port>` fields in
//! [`Modules`] already **are** the abstraction; a second one over the top would abstract the
//! composition root from itself. Each arm fills one `Ports` value and one `Stores` value, and
//! `compose` builds the application out of them identically, so the two profiles cannot drift
//! into serving different applications.
//!
//! Both halves of the [`Backends::Durable`] arm are real. `open_durable_database` demands
//! `DATABASE_URL`, opens the pool, and refuses to continue against a database whose schema is not
//! the one this binary was built for — naming the command that fixes it, never the URL.
//! `valkey` then connects to `VALKEY_URL` and proves it answers `PING`; a server that cannot be
//! reached is [`BootError::Valkey`] — the refusal `store/mod.rs` has always said a
//! required service earns: *"Valkey is required; the server refuses to boot without
//! `VALKEY_URL`"*. Every durable port is then on one of the two (#402, #403, #446): Postgres for
//! every record a restart must not lose, Valkey for the volatile state.
//!
//! OIDC is no exception since #460: the pending ceremonies are on Valkey and the federated
//! accounts are rows of the Postgres account table, so a durable boot that configures
//! `OIDC_ISSUER` runs on the same two backends as everything else and holds no state a restart,
//! or a callback landing on another replica, would lose.
//!
//! No `DATABASE_URL` or no `VALKEY_URL`, and no `--memory`, is a configuration fault naming every
//! missing variable ([`Config::load`]). Nothing here ever silently becomes an in-memory server.
//!
//! # What the memory profile is, precisely
//!
//! Every deterministic in-crate adapter, over a **real** [`FilesystemBlobStore`] and a real
//! [`SystemClock`]. Two consequences worth stating because an operator will meet both:
//!
//! - **The blobs survive a restart and the index does not.** That is not a bug to route around,
//!   it is the shape of a profile whose durable half is exactly the one adapter that has been
//!   written. It also makes the profile useful to `scrub`, which compares those two halves and
//!   will honestly report every blob as an orphan.
//! - **The collector's marks do not survive either.** [`crate::gc::collect`] marks a blob on one
//!   pass and sweeps it on a later pass once the grace window has passed, so a fresh `--memory`
//!   process can only ever mark. The durable profile keeps the marks in Postgres beside the index,
//!   which is what lets `gc` run as one process per pass.

use std::sync::Arc;

use jiff::Timestamp;
use sea_orm::DatabaseConnection;

use crate::album::authority::ProvisionedAuthority;
use crate::album::postgres::PostgresAlbums;
use crate::album::{AlbumContext, AlbumStore, InMemoryAlbums};
use crate::app::{App, Modules};
use crate::attestation::postgres::PostgresReceipts;
use crate::attestation::{AttestationContext, InMemoryReceipts, LocalAttestationKey, ReceiptLog};
use crate::auth::oidc::{
    FederatedAccounts, HttpIdentityProvider, IdentityProvider, OidcCollaborators, OidcContext,
    OidcSettings,
};
use crate::auth::totp::postgres::PostgresTotp;
use crate::auth::{
    AccountDirectory, AccountProfiles, AccountRegistry, AuthCollaborators, AuthContext,
    Credentials, InMemoryAccounts, InMemoryTotp, PasswordChange, PostgresAccounts, SessionTokens,
    TotpCodes, TotpContext, TotpStore,
};
use crate::blob::FilesystemBlobStore;
use crate::config::{Backends, Config};
use crate::counter::valkey::ValkeyCounters;
use crate::counter::{CounterContext, CounterStore, InMemoryCounters};
use crate::directory::postgres::PostgresDeviceDirectory;
use crate::directory::{DeviceDirectoryContext, DeviceDirectoryStore, InMemoryDeviceDirectory};
use crate::discovery::revocation::RevocationList;
use crate::discovery::{DiscoveryContext, ProtocolWindow, ServerInfo};
use crate::drop::postgres::PostgresDrops;
use crate::drop::{DropContext, DropStore, InMemoryDrops};
use crate::enrollment::EnrollmentContext;
use crate::escrow::postgres::PostgresEscrow;
use crate::escrow::{EscrowContext, EscrowStore, InMemoryEscrow};
use crate::federation::postgres::{PostgresCapabilities, PostgresPeers};
use crate::federation::{
    CapabilityCodec, CapabilityStore, FederationCollaborators, FederationContext,
    InMemoryCapabilities, InMemoryPeers, PeerAdministration, PeerStore,
};
use crate::gc::memory::InMemoryCollection;
use crate::gc::postgres::PostgresCollection;
use crate::gc::{CollectionContext, CollectionStore};
use crate::index::AssetIndex;
use crate::index::memory::InMemoryAssetIndex;
use crate::index::postgres::PostgresAssetIndex;
use crate::ledger::rebuild::RebuildContext;
use crate::ledger::replay::LedgerKeys;
use crate::ledger::{AnchoredLedger, FilesystemLedger};
use crate::membership::{
    InMemoryMembership, MembershipContext, MembershipStore, PostgresMembership,
};
use crate::moderation::postgres::PostgresModeration;
use crate::moderation::{InMemoryModeration, ModerationContext, ModerationStore};
use crate::quota::{InMemoryQuota, PostgresQuota, QuotaContext, QuotaLimits, QuotaStore};
use crate::scrub::ScrubContext;
use crate::serve::ServeContext;
use crate::share::postgres::PostgresShares;
use crate::share::{InMemoryShares, ShareContext, ShareStore};
use crate::store::memory::{
    InMemoryAuthState, InMemoryChallenges, InMemoryChannels, InMemoryCohorts, InMemoryEnrollments,
    InMemoryOidcAuthorizations, InMemoryOidcDevices, InMemoryUploadSessions,
};
use crate::store::valkey::ValkeyStores;
use crate::store::{
    AuthStateStore, ChallengeStore, ChannelStore, Clock, CohortStore, EnrollmentStore,
    OidcAuthorizationStore, OidcDeviceStore, PostgresCohorts, SystemClock, UploadSessionStore,
};
use crate::sync::{CursorCodec, SyncContext};
use crate::upload::{UploadContext, UploadPolicy};
use crate::verify::VerifyContext;

/// Why a process could not be assembled.
///
/// Every variant is a **startup** failure. There is deliberately no variant for a degraded boot:
/// a server that came up with one port missing would answer some requests and 500 on others,
/// which is harder to diagnose than a process that refused to start and said why.
#[derive(Debug, thiserror::Error)]
pub enum BootError {
    /// The configuration itself is not usable.
    #[error(transparent)]
    Configuration(#[from] crate::config::ConfigError),
    /// A setting [`Config::load`] treats as optional is required by this path.
    ///
    /// The backstop behind [`crate::config::Demands`], which is the aggregating front door an
    /// operator reads. This variant fires only when the two disagree — a subcommand asking for
    /// more than it declared — which is a programming error rather than a deployment one, and
    /// failing loudly beats an `expect`.
    #[error("{key} is required to assemble this server and is not set")]
    Missing {
        /// The setting.
        key: &'static str,
    },
    /// The blob root could not be opened.
    ///
    /// Refused rather than deferred: a store that cannot be created now is a store every upload
    /// will fail against at write time, and a server that accepts bytes it cannot keep is worse
    /// than one that does not start.
    #[error("the blob store at {root} could not be opened: {detail}")]
    BlobRoot {
        /// The path that was tried.
        root: String,
        /// The filesystem's own description.
        detail: String,
    },
    /// The token-signing key could not be read.
    #[error("the server's token-signing key could not be loaded: {detail}")]
    SigningKey {
        /// What was wrong with it. Never the key.
        detail: String,
    },
    /// The credential verifier could not be built.
    #[error("the credential verifier could not be built: {detail}")]
    Credentials {
        /// The algorithm's own description.
        detail: String,
    },
    /// The durable backend could not be opened, or is not the schema this binary expects.
    ///
    /// Never carries the connection URL: a `DATABASE_URL` holds a password, and a startup error
    /// is the most-copied line in any incident channel.
    #[error("the durable backend could not be opened: {detail}")]
    Database {
        /// The driver's own description, or which migration is missing. Never the URL.
        detail: String,
    },
    /// The relying party's outbound HTTP client could not be built — including a CA bundle
    /// (`OIDC_CA_BUNDLE`) that could not be read or holds no certificate.
    #[error("the OIDC relying party's HTTP client could not be built: {detail}")]
    OidcClient {
        /// What went wrong; names the bundle's path, never its contents.
        detail: String,
    },
    /// The Valkey at `VALKEY_URL` could not be reached, or did not answer `PING`.
    ///
    /// Required means required: a server that came up without its session store would answer
    /// nothing an authenticated client sent, and saying so at boot beats discovering it per
    /// request. `detail` never carries the URL, which carries the password.
    #[error("the Valkey at VALKEY_URL could not be reached: {detail}")]
    Valkey {
        /// The adapter's own description of the failure.
        detail: String,
    },
    /// The router's own types do not describe a buildable server.
    ///
    /// Unreachable in practice — the conformance suite builds the same router on every test run
    /// — and kept because the alternative is an `expect` in the composition root.
    #[error("the router could not be built: {detail}")]
    Router {
        /// Kynos's own description.
        detail: String,
    },
}

/// The operator workers' collaborators, and the quota ledger the operator suspends through.
///
/// Assembled **without demanding any key material**, which is what makes `config`'s claim that
/// `gc`/`purge`/`scrub` need none structural rather than a promise: there is no token-signing
/// key in scope here to accidentally require. A maintenance host that had to hold the
/// production token-signing key to sweep a directory would be a reason to put the key on a
/// maintenance host. The key-derived values here come from `ATTESTATION_KEY_SEED` only when the
/// host was given one: its **public** half, which the scrub verifies owner ledgers under
/// (`ledger_keys`), and its signing half, held only by the collector to append the purge entries
/// an applied retention purge must ledger (`purge_attestation`, #767). Without the seed,
/// `purge --apply` refuses and everything else runs.
///
/// None of them has a wire surface, so none is reachable through the router — which is why
/// they are a separate assembly rather than fields on [`App`].
#[derive(Debug)]
pub struct Maintenance {
    /// The collector's collaborators (`gc`, `purge`).
    pub collection: CollectionContext,
    /// The integrity scrub's collaborators (`scrub`).
    pub scrub: ScrubContext,
    /// The index rebuild's collaborators (`rebuild`): the same index, blobs, owner ledgers and
    /// quota ledger the server writes.
    pub rebuild: RebuildContext,
    /// The quota ledger, for the operator's suspension (`quota suspend`, `quota unsuspend`).
    ///
    /// The same ledger the server charges. Its limits are the unlimited default because a
    /// suspension reads none: it is set by an operator, not reached by bytes.
    pub quota: QuotaContext,
    /// The operator's acts on the peer list (`peer pin`, `peer block`, `peer unblock`,
    /// `peer show`), over the peer and capability stores the server reads (#476).
    pub peers: PeerAdministration,
    /// The moderation store, for the operator's report queue (`moderation reports`, #476).
    pub moderation: Arc<dyn ModerationStore>,
}

/// A server, ready to serve.
///
/// Carries [`Maintenance`] as well, over the **same** stores: one index, one blob store and one
/// mark store behind all three, which is what makes "upload it, then let the collector see it" a
/// property of the server rather than of three disconnected assemblies.
#[derive(Debug)]
pub struct Assembled {
    /// The application context every operation resolves its dependencies from.
    pub app: App,
    /// The operator workers, over the same stores.
    pub maintenance: Maintenance,
}

impl Assembled {
    /// Build the service the listener drives.
    ///
    /// # Errors
    ///
    /// Returns [`BootError::Router`] if the router's types do not describe a buildable server.
    pub fn service(&self) -> Result<kynos::router::service::Service<App>, BootError> {
        crate::service(self.app.clone()).map_err(|error| BootError::Router {
            detail: error.to_string(),
        })
    }
}

/// Assemble a server from `config`.
///
/// # Errors
///
/// Returns [`BootError`] for any of the startup failures above. Nothing is left half-built: the
/// blob root is the only side effect, and it is idempotent.
pub async fn assemble(config: &Config) -> Result<Assembled, BootError> {
    assemble_on(config, Arc::new(SystemClock)).await
}

/// [`assemble`] on `clock` rather than the system clock.
///
/// The seam exists for the tests: a property that depends on time passing — a lockout window
/// decaying — is asserted by moving a [`crate::store::memory::ManualClock`] rather than by
/// sleeping across the window on a loaded machine (#784). Production reaches it only through
/// [`assemble`], with the [`SystemClock`].
async fn assemble_on(config: &Config, clock: Arc<dyn Clock>) -> Result<Assembled, BootError> {
    let base = base(config, clock).await?;
    match config.backends {
        Backends::Memory => {
            warn_in_memory();
            let receipts = Arc::new(InMemoryReceipts::new());
            let stores = base.in_memory(receipts.clone());
            let ports = memory_ports(config, &stores.clock, receipts)?;
            compose(config, stores, ports, "the in-memory adapters")
        }
        Backends::Durable => {
            // Both probes run for real, in the order the ports are declared: `DATABASE_URL` is
            // demanded and the pool opened first — a schema that is not the one this binary was
            // built for refuses here — and then Valkey must answer `PING`. Only then is any
            // module assembled, and every port it is assembled from is on one of the two.
            let connection = open_durable_database(config).await?;
            let valkey = valkey(config).await?;
            let stores = base.durable(&connection, &valkey);
            let ports = durable_ports(config, &stores.clock, &connection, &valkey)?;
            compose(config, stores, ports, "the Postgres and Valkey adapters")
        }
    }
}

/// Say, at `WARN` and on every start, that this process runs on the in-memory adapters.
///
/// `--memory` is a development act and never a deployment profile, so a process that comes up on
/// it says so loudly enough to be seen under a production log filter: everything it holds apart
/// from the blob bytes is gone when it exits (#521).
fn warn_in_memory() {
    tracing::warn!(
        backends = "memory",
        "running on the in-memory adapters (--memory): a development profile, never a \
         deployment; all state but the blob bytes is lost when this process exits"
    );
}

/// Open the durable pool and refuse a schema this binary was not built for.
///
/// The check is **not** a migration. `capsule-server` cannot link the migrator — see
/// `capsule-server/migration`'s manifest for the `chrono` gate that forces it — and a server that
/// migrated on start would run the migration once per replica during a rolling deploy anyway.
/// What it does instead is read `seaql_migrations` and refuse, naming the command an operator has
/// to run.
///
/// `DATABASE_URL` is demanded by [`crate::config::Demands`] for every durable subcommand, so an
/// operator is refused with the configuration exit code and every missing variable at once; the
/// demand here is the [`BootError::Missing`] backstop behind it.
async fn open_durable_database(config: &Config) -> Result<sea_orm::DatabaseConnection, BootError> {
    let database_url = config.database_url.as_ref().ok_or(BootError::Missing {
        key: "DATABASE_URL",
    })?;
    let connection = crate::postgres::connect(database_url)
        .await
        .map_err(|error| BootError::Database {
            detail: error.to_string(),
        })?;
    crate::postgres::assert_schema_current(&connection)
        .await
        .map_err(|error| BootError::Database {
            detail: error.to_string(),
        })?;
    tracing::info!("the durable database is open and its schema is current");
    Ok(connection)
}

/// Assemble only what `gc`, `purge` and `scrub` read.
///
/// # Errors
///
/// Returns [`BootError`] for the blob root or a durable backend that cannot be opened. It cannot
/// fail on key material, because it demands none: an `ATTESTATION_KEY_SEED` the host holds is
/// used, and one it lacks only leaves `purge --apply` refusing.
pub async fn assemble_maintenance(config: &Config) -> Result<Maintenance, BootError> {
    let base = base(config, Arc::new(SystemClock)).await?;
    let (stores, adapters) = match config.backends {
        Backends::Memory => {
            warn_in_memory();
            // A log of its own: the maintenance workers neither finalize nor serve receipts.
            (
                base.in_memory(Arc::new(InMemoryReceipts::new())),
                "the in-memory adapters",
            )
        }
        // The same two probes, in the same order, as `serve`'s durable arm — and the same
        // stores, so a `gc` pass reads the index the server writes and the marks the previous
        // pass left. That last part is the reason the arm exists: the collector marks a blob on
        // one pass and sweeps it on a later one, so a mark store that forgets is a collector that
        // can only ever mark.
        //
        // `VALKEY_URL` is demanded too, after `DATABASE_URL`. The scrub reads the upload-session
        // store to tell a live transfer from an orphan, and that store is Valkey's (#403); a
        // scrub over the durable index that consulted an empty in-memory session table instead
        // would report every in-flight upload as an orphan.
        Backends::Durable => {
            let connection = open_durable_database(config).await?;
            let valkey = valkey(config).await?;
            (
                base.durable(&connection, &valkey),
                "the Postgres and Valkey adapters",
            )
        }
    };
    let purge_signer = purge_attestation(config, &stores);
    let ledgers_purges = purge_signer.is_some();
    let maintenance = stores.maintenance(config.grace_window, ledger_keys(config), purge_signer);
    tracing::info!(
        blob_root = %stores.root.display(),
        grace_window = %config.grace_window,
        ledgers_purges,
        adapters,
        "assembled the operator workers"
    );
    Ok(maintenance)
}

/// Connect to `VALKEY_URL` and prove it answers, before any module is assembled.
///
/// Straight after the durable pool, and before a socket is bound: a Valkey that cannot be
/// reached is the fault an operator most needs named early. Every Valkey store — sessions,
/// upload sessions and the three ceremonies — comes back on one connection and the real clock;
/// the counter adapter shares that connection.
async fn valkey(config: &Config) -> Result<ValkeyStores, BootError> {
    let url = config
        .valkey_url
        .as_deref()
        .ok_or(BootError::Missing { key: "VALKEY_URL" })?;
    let stores = ValkeyStores::connect(url, Arc::new(SystemClock))
        .await
        .map_err(|error| BootError::Valkey {
            detail: error.to_string(),
        })?;
    tracing::info!("VALKEY_URL answers; the volatile state ports are on Valkey");
    Ok(stores)
}

/// The trust anchors `OIDC_CA_BUNDLE` names, read once at boot.
///
/// Refused rather than deferred, like the blob root: a bundle that cannot be read now is a
/// provider every sign-in will fail against at handshake time, with a less legible error. An
/// empty bundle is refused too — an operator who named a file meant it to hold something.
fn oidc_roots(path: &std::path::Path) -> Result<Vec<reqwest::Certificate>, BootError> {
    let shown = path.display();
    let pem = std::fs::read(path).map_err(|error| BootError::OidcClient {
        detail: format!("OIDC_CA_BUNDLE `{shown}` could not be read: {error}"),
    })?;
    let roots =
        reqwest::Certificate::from_pem_bundle(&pem).map_err(|error| BootError::OidcClient {
            detail: format!("OIDC_CA_BUNDLE `{shown}` is not a PEM certificate bundle: {error}"),
        })?;
    if roots.is_empty() {
        return Err(BootError::OidcClient {
            detail: format!("OIDC_CA_BUNDLE `{shown}` holds no certificate"),
        });
    }
    tracing::info!(bundle = %shown, roots = roots.len(), "trusting additional roots for the identity provider");
    Ok(roots)
}

/// The stores every subcommand shares.
///
/// A struct rather than six locals because [`assemble`] and [`assemble_maintenance`] must build
/// the *same* stores: two functions each opening their own index is two servers that disagree
/// about what is in it. The fields are ports rather than adapters, so the two backend arms fill
/// one shape and everything downstream of it is shared.
#[derive(Debug)]
struct Stores {
    root: std::path::PathBuf,
    clock: Arc<dyn Clock>,
    blobs: Arc<FilesystemBlobStore>,
    /// The owner ledgers under the blob root, on both backends.
    ledger: Arc<FilesystemLedger>,
    index: Arc<dyn AssetIndex>,
    uploads: Arc<dyn UploadSessionStore>,
    marks: Arc<dyn CollectionStore>,
    quotas: Arc<dyn QuotaStore>,
    /// The share links: read by the application, and by the collector, which must not sweep a
    /// blob a live link serves (#653). Here rather than in [`Ports`] so both are one store.
    shares: Arc<dyn ShareStore>,
    /// The capability store, which **is** the revocation list `revoked-jti` serves: one object,
    /// handed to discovery as the list, to federation as the store, and to the operator's peer
    /// commands as what a block cascades over (#476). Here so all three are one store.
    capabilities: Arc<dyn CapabilityStore>,
    /// The peers: read by federation at every boundary and written by `capsule-server peer …`.
    peers: Arc<dyn PeerStore>,
    /// The moderation store: written by report intake, read by `capsule-server moderation …`.
    moderation: Arc<dyn ModerationStore>,
}

impl Stores {
    /// The operator workers over these stores.
    ///
    /// `purge_signer` is what an applied retention purge appends its ledger entries through
    /// (#767); `None` leaves `purge --apply` refusing rather than purging unrecorded.
    fn maintenance(
        &self,
        grace_window: jiff::SignedDuration,
        keys: LedgerKeys,
        purge_signer: Option<AttestationContext>,
    ) -> Maintenance {
        let mut collection = CollectionContext::new(
            self.index.clone(),
            self.blobs.clone(),
            self.marks.clone(),
            self.quotas.clone(),
            self.shares.clone(),
            self.clock.clone(),
            grace_window,
        )
        // The upload-progress view has no lifetime of its own, so `gc --apply` is what keeps
        // it from growing with every abandoned upload (#519).
        .with_progress_sweep(Arc::new(self.uploads.clone()));
        if let Some(attestation) = purge_signer {
            collection = collection.with_attestation(attestation);
        }
        Maintenance {
            collection,
            // The same share store as the collector's, so a record the collector keeps for a
            // live link is not one the scrub reports as an orphan (#653).
            //
            // Check 6 reads the owner ledgers under the same blob root the server appends to.
            scrub: ScrubContext::new(
                self.index.clone(),
                self.blobs.clone(),
                self.uploads.clone(),
                self.shares.clone(),
                self.clock.clone(),
            )
            .with_ledger(self.ledger.clone(), keys.clone()),
            rebuild: RebuildContext::new(
                self.index.clone(),
                self.blobs.clone(),
                self.ledger.clone(),
                self.quotas.clone(),
                keys,
                self.clock.clone(),
            ),
            quota: QuotaContext::new(
                self.quotas.clone(),
                self.clock.clone(),
                QuotaLimits::unlimited(),
            ),
            peers: PeerAdministration::new(
                self.peers.clone(),
                self.capabilities.clone(),
                self.clock.clone(),
            ),
            moderation: self.moderation.clone(),
        }
    }
}

/// The published attestation key the operator workers verify owner ledgers under.
///
/// Derived from `ATTESTATION_KEY_SEED` when the host was given one, and only the **public** half
/// leaves this function: the signing key is built to compute it and dropped, so the maintenance
/// assembly still holds nothing that signs. A host given no seed verifies ledger links and
/// positions only, and its scrub report says so.
fn ledger_keys(config: &Config) -> LedgerKeys {
    config
        .attestation_key_seed
        .map_or_else(LedgerKeys::default, |seed| {
            let key = LocalAttestationKey::new(
                config.server_domain.clone(),
                capsule_core::crypto::keys::HybridSigningKey::from_seed64(&seed),
            );
            LedgerKeys::new(vec![crate::attestation::PublishedKey {
                key_id: crate::attestation::ReceiptSigner::key_id(&key),
                public: key.verifying_key(),
                active_from: jiff::Timestamp::UNIX_EPOCH,
                active_to: None,
            }])
        })
}

/// The attestation context an applied retention purge signs its owner-ledger entries with
/// (#767), when the host was given `ATTESTATION_KEY_SEED`.
///
/// The one place the maintenance assembly keeps a signing half, and only to append purge
/// entries to the same ledgers under the blob root the server appends to. A host given no seed
/// gets `None`, and its `purge --apply` refuses rather than purging what no ledger records;
/// `gc`, `scrub`, `rebuild` and a dry-run purge sign nothing.
///
/// The receipt log is a log of its own, as the in-memory arm's is: a purge issues no receipt.
fn purge_attestation(config: &Config, stores: &Stores) -> Option<AttestationContext> {
    let seed = config.attestation_key_seed?;
    let key = LocalAttestationKey::new(
        config.server_domain.clone(),
        capsule_core::crypto::keys::HybridSigningKey::from_seed64(&seed),
    );
    Some(
        AttestationContext::new(
            Arc::new(InMemoryReceipts::new()),
            Arc::new(key),
            Timestamp::UNIX_EPOCH,
        )
        // Through the anchor, as the server's own producers append (#757): a purge entry moves
        // the owner's recorded head like any other, and a ledger cut below its anchor refuses
        // the purge rather than extending the cut chain.
        .with_ledger(Arc::new(AnchoredLedger::new(
            stores.ledger.clone(),
            stores.index.clone(),
        ))),
    )
}

/// The half of [`Stores`] that is the same on both arms: the blob root and the clock.
#[derive(Debug)]
struct Base {
    root: std::path::PathBuf,
    clock: Arc<dyn Clock>,
    blobs: Arc<FilesystemBlobStore>,
    ledger: Arc<FilesystemLedger>,
}

impl Base {
    /// The shared stores on the deterministic in-crate adapters.
    ///
    /// The index appends finalization's custody receipts to `receipts` under its own lock
    /// (#693), so the attestation context must be built over the same log.
    fn in_memory(self, receipts: Arc<InMemoryReceipts>) -> Stores {
        Stores {
            index: Arc::new(InMemoryAssetIndex::with_receipts(receipts)),
            uploads: Arc::new(InMemoryUploadSessions::with_default_ttl(self.clock.clone())),
            marks: Arc::new(InMemoryCollection::new()),
            quotas: Arc::new(InMemoryQuota::new()),
            shares: Arc::new(InMemoryShares::new()),
            capabilities: Arc::new(InMemoryCapabilities::new(self.clock.clone())),
            peers: Arc::new(InMemoryPeers::new()),
            moderation: Arc::new(InMemoryModeration::new()),
            root: self.root,
            clock: self.clock,
            blobs: self.blobs,
            ledger: self.ledger,
        }
    }

    /// The shared stores on Postgres, and the upload sessions on Valkey.
    fn durable(self, connection: &DatabaseConnection, valkey: &ValkeyStores) -> Stores {
        Stores {
            index: Arc::new(PostgresAssetIndex::new(connection.clone())),
            uploads: valkey.uploads(),
            marks: Arc::new(PostgresCollection::new(connection.clone())),
            quotas: Arc::new(PostgresQuota::new(connection.clone())),
            shares: Arc::new(PostgresShares::new(connection.clone())),
            capabilities: Arc::new(PostgresCapabilities::new(
                connection.clone(),
                self.clock.clone(),
            )),
            peers: Arc::new(PostgresPeers::new(connection.clone())),
            moderation: Arc::new(PostgresModeration::new(connection.clone())),
            root: self.root,
            clock: self.clock,
            blobs: self.blobs,
            ledger: self.ledger,
        }
    }
}

/// Open the blob root.
///
/// The blob root is the one thing on this path that touches the filesystem, and it is refused
/// rather than deferred: a store that cannot be created now is a store every upload will fail
/// against at write time, and a server that accepts bytes it cannot keep is worse than one that
/// does not start.
async fn base(config: &Config, clock: Arc<dyn Clock>) -> Result<Base, BootError> {
    let root = config
        .blob_root
        .clone()
        .ok_or(BootError::Missing { key: "BLOB_ROOT" })?;
    let blobs =
        Arc::new(
            FilesystemBlobStore::open(&root)
                .await
                .map_err(|error| BootError::BlobRoot {
                    root: root.display().to_string(),
                    detail: error.to_string(),
                })?,
        );
    // The owner ledger lives under the blob root, beside the blobs it accounts for, on both
    // backends: it is what makes the index rebuildable, so a deployment without it would be one
    // whose backup of `BLOB_ROOT` is not enough. Refused like the blob root itself.
    let ledger =
        Arc::new(
            FilesystemLedger::open(&root)
                .await
                .map_err(|error| BootError::BlobRoot {
                    root: root.display().to_string(),
                    detail: error.to_string(),
                })?,
        );
    Ok(Base {
        root,
        clock,
        blobs,
        ledger,
    })
}

/// The five account ports, which every adapter family implements on one object.
///
/// One object is the point (#460): the federated accounts are rows of the same table as the
/// password accounts, so the OIDC `409` and the federated profile are answered from the rows the
/// password ports read. Built from one `Arc`, the five cannot be wired to two stores.
#[derive(Debug, Clone)]
struct AccountPorts {
    accounts: Arc<dyn AccountDirectory>,
    registry: Arc<dyn AccountRegistry>,
    profiles: Arc<dyn AccountProfiles>,
    passwords: Arc<dyn PasswordChange>,
    federated: Arc<dyn FederatedAccounts>,
}

impl AccountPorts {
    /// All five, out of one store.
    fn of<A>(store: Arc<A>) -> Self
    where
        A: AccountDirectory
            + AccountRegistry
            + AccountProfiles
            + PasswordChange
            + FederatedAccounts
            + 'static,
    {
        Self {
            accounts: store.clone(),
            registry: store.clone(),
            profiles: store.clone(),
            passwords: store.clone(),
            federated: store,
        }
    }
}

/// Every application port beyond [`Stores`], chosen by one backend arm.
///
/// The two arms differ **only** in what they put here; [`compose`] builds the seventeen module
/// contexts out of it identically. That is what makes "the durable profile serves the same
/// application as the memory profile" a property of one function rather than of two copies.
#[derive(Debug)]
struct Ports {
    accounts: AccountPorts,
    sessions: Arc<dyn AuthStateStore>,
    challenges: Arc<dyn ChallengeStore>,
    cohorts: Arc<dyn CohortStore>,
    totp: Arc<dyn TotpStore>,
    albums: Arc<dyn AlbumStore>,
    members: Arc<dyn MembershipStore>,
    directories: Arc<dyn DeviceDirectoryStore>,
    receipts: Arc<dyn ReceiptLog>,
    escrow: Arc<dyn EscrowStore>,
    enrollments: Arc<dyn EnrollmentStore>,
    channels: Arc<dyn ChannelStore>,
    drops: Arc<dyn DropStore>,
    counters: Arc<dyn CounterStore>,
    /// The pending OIDC ceremonies. Built on both arms whether or not `OIDC_ISSUER` is set, and
    /// read only when it is.
    oidc_authorizations: Arc<dyn OidcAuthorizationStore>,
    /// The pending OIDC device authorizations (issue #635), on the same terms.
    oidc_devices: Arc<dyn OidcDeviceStore>,
}

/// The credential verifier, built once.
///
/// Building it costs an Argon2id hash (the timing-equalized miss's decoy), and that is a startup
/// cost rather than a per-request one.
fn credentials() -> Result<Credentials, BootError> {
    Credentials::new().map_err(|error| BootError::Credentials {
        detail: error.detail,
    })
}

/// The development profile's ports: every deterministic in-crate adapter.
///
/// `receipts` is the log the in-memory index appends to, so the receipt route reads what
/// finalization wrote.
fn memory_ports(
    config: &Config,
    clock: &Arc<dyn Clock>,
    receipts: Arc<InMemoryReceipts>,
) -> Result<Ports, BootError> {
    Ok(Ports {
        accounts: AccountPorts::of(Arc::new(InMemoryAccounts::new(
            credentials()?,
            clock.clone(),
            config.lockout_attempts,
            config.lockout_window,
        ))),
        sessions: Arc::new(InMemoryAuthState::with_default_ttl(clock.clone())),
        challenges: Arc::new(InMemoryChallenges::with_default_ttl(clock.clone())),
        cohorts: Arc::new(InMemoryCohorts::new()),
        totp: Arc::new(InMemoryTotp::new()),
        albums: Arc::new(InMemoryAlbums::new()),
        members: Arc::new(InMemoryMembership::new()),
        directories: Arc::new(InMemoryDeviceDirectory::new()),
        receipts,
        escrow: Arc::new(InMemoryEscrow::new()),
        enrollments: Arc::new(InMemoryEnrollments::with_default_ttl(clock.clone())),
        channels: Arc::new(InMemoryChannels::with_default_ttl(clock.clone())),
        drops: Arc::new(InMemoryDrops::new()),
        counters: Arc::new(InMemoryCounters::new()),
        oidc_authorizations: Arc::new(InMemoryOidcAuthorizations::with_default_ttl(clock.clone())),
        oidc_devices: Arc::new(InMemoryOidcDevices::with_default_ttl(clock.clone())),
    })
}

/// The durable profile's ports: Postgres for every record a restart must not lose, Valkey for
/// the volatile state design/filesystem/server.md assigns it.
///
/// Nothing here is in-memory, the OIDC ports included (#460): the federated accounts are the
/// account table's rows and the pending ceremonies are Valkey's.
fn durable_ports(
    config: &Config,
    clock: &Arc<dyn Clock>,
    connection: &DatabaseConnection,
    valkey: &ValkeyStores,
) -> Result<Ports, BootError> {
    let db = || connection.clone();
    Ok(Ports {
        accounts: AccountPorts::of(Arc::new(PostgresAccounts::new(
            db(),
            credentials()?,
            clock.clone(),
            config.lockout_attempts,
            config.lockout_window,
        ))),
        sessions: valkey.auth(),
        challenges: valkey.challenges(),
        // Postgres rather than `valkey.cohorts()`: the cohort map has no expiry and is the one
        // auth record design/filesystem/server.md puts beside the accounts (#402).
        cohorts: Arc::new(PostgresCohorts::new(db())),
        totp: Arc::new(PostgresTotp::new(db())),
        albums: Arc::new(PostgresAlbums::new(db())),
        members: Arc::new(PostgresMembership::new(db())),
        directories: Arc::new(PostgresDeviceDirectory::new(db())),
        receipts: Arc::new(PostgresReceipts::new(db())),
        escrow: Arc::new(PostgresEscrow::new(db())),
        enrollments: valkey.enrollments(),
        channels: valkey.channels(),
        drops: Arc::new(PostgresDrops::new(db())),
        counters: Arc::new(ValkeyCounters::new(valkey.valkey().clone())),
        oidc_authorizations: valkey.oidc_authorizations(),
        oidc_devices: valkey.oidc_devices(),
    })
}

/// Build the application out of `stores` and `ports`, whichever arm chose them.
///
/// `adapters` names the family for the one log line an operator reads at boot.
#[allow(
    clippy::too_many_lines,
    reason = "seventeen module contexts, named once each; splitting it would hide the shape"
)]
fn compose(
    config: &Config,
    stores: Stores,
    ports: Ports,
    adapters: &'static str,
) -> Result<Assembled, BootError> {
    let der = config.signing_key_der.as_ref().ok_or(BootError::Missing {
        key: "JWT_ED25519_DER",
    })?;
    let cursor_key = config.sync_cursor_mac_key.ok_or(BootError::Missing {
        key: "SYNC_CURSOR_MAC_KEY",
    })?;
    let seed = config.attestation_key_seed.ok_or(BootError::Missing {
        key: "ATTESTATION_KEY_SEED",
    })?;

    // Cloned rather than moved: `stores` is handed to `Stores::maintenance` at the end, so the
    // application and the two operator workers are built over the *same* stores. Every clone
    // here is an `Arc` refcount bump.
    let root = stores.root.clone();
    let ledger = stores.ledger.clone();
    let clock = stores.clock.clone();
    let blobs = stores.blobs.clone();
    let index = stores.index.clone();
    let uploads = stores.uploads.clone();
    let marks = stores.marks.clone();
    let quotas = stores.quotas.clone();
    // Unlimited, which is what a self-hosted deployment runs. A configurable ceiling is a
    // quota policy this change does not own; `QuotaLimits` already takes one. One context, so
    // the upload module restores a reservation under the same limits the quota module charges.
    let quota = QuotaContext::new(quotas.clone(), clock.clone(), QuotaLimits::unlimited());
    let shares = stores.shares.clone();
    let issued = stores.capabilities.clone();
    let peers = stores.peers.clone();
    let moderation = stores.moderation.clone();

    // The signer is built from the private key alone and derives its own public half, which is
    // what lets `ServerInfo` below publish the key tokens actually verify under rather than one
    // an operator pasted beside it.
    let tokens = Arc::new(
        SessionTokens::from_pkcs8(der.expose(), clock.clone()).map_err(|error| {
            BootError::SigningKey {
                detail: error.detail,
            }
        })?,
    );

    let Ports {
        accounts,
        sessions,
        challenges,
        cohorts,
        totp,
        albums,
        members,
        directories,
        receipts,
        escrow,
        enrollments,
        channels,
        drops,
        counters,
        oidc_authorizations,
        oidc_devices,
    } = ports;
    // The production write authority, not a permissive double: it reads the
    // album's own pin and the account's published device directory, so invariants 6 and 7 mean
    // what they say even in the development profile.
    let authority = Arc::new(ProvisionedAuthority::new(
        albums.clone(),
        directories.clone(),
        members.clone(),
        clock.clone(),
    ));
    // Distinct from the token signer, as the design requires: a receipt that verified under the
    // operational key would let anything holding that key manufacture custody evidence.
    //
    // Which is why a durable deployment has to **supply** `ATTESTATION_KEY_SEED` rather than
    // have it derived (`config`, the key-material section). A different HKDF `info` over the
    // same input is not a separation at all — anyone holding `JWT_ED25519_DER` recomputes it —
    // and it read as one, which is worse than no comment. The derivation survives only under
    // `Backends::Memory`, where the server is a development act whose state is discarded.
    let attestation_key = Arc::new(LocalAttestationKey::new(
        config.server_domain.clone(),
        capsule_core::crypto::keys::HybridSigningKey::from_seed64(&seed),
    ));

    // The capability codec signs with the **same** key: a peer verifies a capability against
    // the key `server-info` publishes, and that key is read out of the session signer. Built
    // from the same bytes rather than handed the signer, so the two stay one key by
    // construction; `tests::the_capability_codec_signs_under_the_published_key` asserts it.
    let capabilities = Arc::new(
        CapabilityCodec::from_pkcs8(der.expose(), config.server_domain.clone(), clock.clone())
            .map_err(|error| BootError::SigningKey {
                detail: error.detail,
            })?,
    );

    let mut server_info = ServerInfo::new(
        config.server_domain.clone(),
        config.api_base_url.clone(),
        ProtocolWindow {
            min: config.protocol_min.clone(),
            max: config.protocol_max.clone(),
        },
        tokens.public_key().to_vec(),
    );
    if config.oidc.is_some() {
        // Endpoints only; the issuer and client id stay the server's.
        server_info = server_info.with_oidc();
    }
    // Published only when the deployment federates (#509): a `FEDERATION_URL` without
    // `FEDERATION_ENABLED` is an endpoint nothing behind it would answer.
    if let Some(url) = config.federation_endpoint() {
        server_info = server_info.with_federation(url.to_owned());
    } else if config.federation_url.is_some() {
        tracing::warn!(
            "FEDERATION_URL is set but FEDERATION_ENABLED is not; this server does not federate \
             and publishes no federation endpoint"
        );
    }
    let server_info = Arc::new(server_info);
    // The same object as `issued`, seen through the port discovery publishes it under.
    let revocations: Arc<dyn RevocationList> = issued.clone();

    // The relying party, or the null object. Discovery is lazy: nothing here reaches the
    // provider, so an identity provider that is down does not stop a server from serving local
    // auth.
    let oidc = match &config.oidc {
        Some(oidc) => {
            let roots = match &oidc.ca_bundle {
                Some(path) => oidc_roots(path)?,
                None => Vec::new(),
            };
            let http = HttpIdentityProvider::http_client(&roots).map_err(|error| {
                BootError::OidcClient {
                    detail: error.to_string(),
                }
            })?;
            let provider: Arc<dyn IdentityProvider> = Arc::new(HttpIdentityProvider::new(
                OidcSettings {
                    issuer: oidc.issuer.clone(),
                    client_id: oidc.client_id.clone(),
                    client_secret: oidc.client_secret.clone(),
                    redirects: oidc.redirects.clone(),
                },
                http,
                clock.clone(),
            ));
            tracing::info!(issuer = %oidc.issuer, "the OIDC relying party is configured");
            OidcContext::new(OidcCollaborators {
                provider,
                authorizations: oidc_authorizations,
                devices: oidc_devices,
                accounts: accounts.federated.clone(),
                clock: clock.clone(),
            })
        }
        None => OidcContext::disabled(clock.clone()),
    };

    // One context for the application and the purge worker, so a purge signs under the key the
    // server's receipts name and appends to the ledgers the server's writes append to (#767).
    let attestation = AttestationContext::new(
        receipts.clone(),
        attestation_key,
        // The published key has been active since the epoch, because the seed is derived
        // deterministically and has therefore never *not* been this deployment's key.
        // Publishing a rotation history is `ATTESTATION_KEY_HISTORY`'s job and nobody's yet.
        Timestamp::UNIX_EPOCH,
    )
    // Every producer appends through the anchor, so each owner's head is recorded in the
    // index the rebuild and the scrub verify the ledger against (#757).
    .with_ledger(Arc::new(AnchoredLedger::new(ledger, index.clone())));

    let app = App::new(Modules {
        auth: AuthContext::new(AuthCollaborators {
            sessions,
            accounts: accounts.accounts,
            registry: accounts.registry,
            profiles: accounts.profiles,
            passwords: accounts.passwords,
            challenges,
            cohorts,
            tokens: tokens.clone(),
            clock: clock.clone(),
        }),
        totp: TotpContext::new(
            totp,
            // The issuer is what an authenticator app shows beside the code, so it is this
            // deployment's own name rather than a constant every deployment shares.
            Arc::new(TotpCodes::new(config.server_domain.clone())),
        ),
        oidc,
        upload: UploadContext::new(
            uploads.clone(),
            blobs.clone(),
            index.clone(),
            authority.clone(),
            quota.clone(),
            clock.clone(),
            // The window the operator configured, not the crate default: this policy is what
            // the handshake enforces and what every response advertises (`negotiation`), and the
            // discovery record above publishes the same two values. One window, three readers.
            UploadPolicy::default()
                .with_protocol_window(config.protocol_min.clone(), config.protocol_max.clone())
                .with_min_client_build(config.min_client_build.clone())
                // The chunk-body read deadline (#717), fixed for the life of the process.
                .with_chunk_read_deadline(Arc::new(config.chunk_read_timeout)),
        ),
        sync: SyncContext::new(
            index.clone(),
            blobs.clone(),
            Arc::new(CursorCodec::new(&cursor_key)),
            albums.clone(),
            members.clone(),
        ),
        serve: ServeContext::new(
            index.clone(),
            blobs.clone(),
            marks.clone(),
            uploads.clone(),
            crate::serve::membership_reads(members.clone()),
        ),
        verify: VerifyContext::new(index.clone(), blobs.clone(), marks.clone(), clock.clone()),
        directories: DeviceDirectoryContext::new(directories.clone(), clock.clone()),
        albums: AlbumContext::new(albums.clone(), clock.clone()),
        membership: MembershipContext::new(members, clock.clone()),
        quota,
        attestation: attestation.clone(),
        discovery: DiscoveryContext::new(server_info, revocations),
        escrow: EscrowContext::new(escrow, clock.clone()),
        federation: FederationContext::new(FederationCollaborators {
            codec: capabilities,
            capabilities: issued,
            peers,
            clock: clock.clone(),
            enabled: Arc::new(config.federation_endpoint().is_some()),
        }),
        enrollment: EnrollmentContext::new(enrollments, channels, clock.clone()),
        moderation: ModerationContext::new(moderation),
        share: ShareContext::new(shares, blobs.clone(), clock.clone()),
        drops: DropContext::new(drops, uploads.clone(), blobs.clone(), clock.clone()),
        counters: CounterContext::new(counters, clock.clone()),
    });

    tracing::info!(
        blob_root = %root.display(),
        server_id = %config.server_domain,
        protocol_min = %config.protocol_min,
        protocol_max = %config.protocol_max,
        federation = config.federation_endpoint().is_some(),
        adapters,
        "assembled a server"
    );

    Ok(Assembled {
        app,
        // The same stores, so "upload it, then let the collector see it" is a property of the
        // server rather than of two disconnected assemblies.
        maintenance: stores.maintenance(
            config.grace_window,
            ledger_keys(config),
            Some(attestation),
        ),
    })
}

#[cfg(test)]
mod tests;
