//! [`assemble`] — the one composition root, and the only place adapters are chosen.
//!
//! # Why this is a library module and not `main`
//!
//! Until this slice the only composition root in the tree was `tests/support/mod.rs`, which
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
//! reached is [`BootError::Valkey`] — the refusal `store/mod.rs` has said since `S-C29` a
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
    InMemoryCapabilities, InMemoryPeers, PeerStore,
};
use crate::gc::memory::InMemoryCollection;
use crate::gc::postgres::PostgresCollection;
use crate::gc::{CollectionContext, CollectionStore};
use crate::index::AssetIndex;
use crate::index::memory::InMemoryAssetIndex;
use crate::index::postgres::PostgresAssetIndex;
use crate::ledger::FilesystemLedger;
use crate::ledger::rebuild::RebuildContext;
use crate::ledger::replay::LedgerKeys;
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
/// Assembled **without any key material**, which is what makes `config`'s claim that
/// `gc`/`purge`/`scrub` need none structural rather than a promise: there is no signing key in
/// scope here to accidentally require. A maintenance host that had to hold the production
/// token-signing key to sweep a directory would be a reason to put the key on a maintenance
/// host. The one key-derived value here is the attestation key's **public** half, which the
/// scrub verifies owner ledgers under (`ledger_keys`); the signing half is never kept.
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
/// fail on key material, because it asks for none.
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
    let maintenance = stores.maintenance(config.grace_window, ledger_keys(config));
    tracing::info!(
        blob_root = %stores.root.display(),
        grace_window = %config.grace_window,
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
}

impl Stores {
    /// The operator workers over these stores.
    fn maintenance(&self, grace_window: jiff::SignedDuration, keys: LedgerKeys) -> Maintenance {
        Maintenance {
            collection: CollectionContext::new(
                self.index.clone(),
                self.blobs.clone(),
                self.marks.clone(),
                self.quotas.clone(),
                self.shares.clone(),
                self.clock.clone(),
                grace_window,
            )
            // The upload-progress view has no lifetime of its own, so `gc --apply` is what
            // keeps it from growing with every abandoned upload (#519).
            .with_progress_sweep(Arc::new(self.uploads.clone())),
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
    /// The capability store **is** the revocation list `revoked-jti` serves: one object, handed
    /// to discovery as the list and to federation as the store (design/federation.md).
    capabilities: Arc<dyn CapabilityStore>,
    peers: Arc<dyn PeerStore>,
    escrow: Arc<dyn EscrowStore>,
    enrollments: Arc<dyn EnrollmentStore>,
    channels: Arc<dyn ChannelStore>,
    moderation: Arc<dyn ModerationStore>,
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
        capabilities: Arc::new(InMemoryCapabilities::new(clock.clone())),
        peers: Arc::new(InMemoryPeers::new()),
        escrow: Arc::new(InMemoryEscrow::new()),
        enrollments: Arc::new(InMemoryEnrollments::with_default_ttl(clock.clone())),
        channels: Arc::new(InMemoryChannels::with_default_ttl(clock.clone())),
        moderation: Arc::new(InMemoryModeration::new()),
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
        capabilities: Arc::new(PostgresCapabilities::new(db(), clock.clone())),
        peers: Arc::new(PostgresPeers::new(db())),
        escrow: Arc::new(PostgresEscrow::new(db())),
        enrollments: valkey.enrollments(),
        channels: valkey.channels(),
        moderation: Arc::new(PostgresModeration::new(db())),
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
    let shares = stores.shares.clone();

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
        capabilities: issued,
        peers,
        escrow,
        enrollments,
        channels,
        moderation,
        drops,
        counters,
        oidc_authorizations,
        oidc_devices,
    } = ports;
    // The production write authority (`S-C19`/`S-C20`), not a permissive double: it reads the
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
            clock.clone(),
            // The window the operator configured, not the crate default: this policy is what
            // the handshake enforces and what every response advertises (`negotiation`), and the
            // discovery record above publishes the same two values. One window, three readers.
            UploadPolicy::default()
                .with_protocol_window(config.protocol_min.clone(), config.protocol_max.clone())
                .with_min_client_build(config.min_client_build.clone()),
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
        // Unlimited, which is what a self-hosted deployment runs. A configurable ceiling is a
        // quota policy this slice does not own; `QuotaLimits` already takes one.
        quota: QuotaContext::new(quotas.clone(), clock.clone(), QuotaLimits::unlimited()),
        attestation: AttestationContext::new(
            receipts.clone(),
            attestation_key,
            // The published key has been active since the epoch, because the seed is derived
            // deterministically and has therefore never *not* been this deployment's key.
            // Publishing a rotation history is `ATTESTATION_KEY_HISTORY`'s job and nobody's yet.
            Timestamp::UNIX_EPOCH,
        )
        .with_ledger(ledger),
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
        maintenance: stores.maintenance(config.grace_window, ledger_keys(config)),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{App, BootError, assemble, assemble_on};
    use crate::config::{Config, Demands, Overrides};
    use crate::store::memory::ManualClock;

    /// A PKCS#8 v1 Ed25519 key, base64. Signs nothing; see `config`'s own tests.
    const EXAMPLE_DER: &str = "MC4CAQAwBQYDK2VwBCIEIN6eTvXEL7xMZWHY8rTk7VbQSGSuRkle5MVfiiYUStLF";

    fn memory_config(root: &std::path::Path) -> Config {
        memory_config_with(root, &[])
    }

    /// The same, with `extra` on top of the environment.
    fn memory_config_with(root: &std::path::Path, extra: &[(&str, &str)]) -> Config {
        let mut environment: BTreeMap<String, String> = [
            ("BLOB_ROOT".to_owned(), root.display().to_string()),
            ("JWT_ED25519_DER".to_owned(), EXAMPLE_DER.to_owned()),
        ]
        .into_iter()
        .collect();
        for (key, value) in extra {
            environment.insert((*key).to_owned(), (*value).to_owned());
        }
        let overrides = Overrides {
            memory: true,
            ..Overrides::default()
        };
        Config::load(&environment, &overrides, Demands::Serve).expect("the configuration loads")
    }

    /// Register the account the auth cases sign in with, through the surface.
    async fn register(client: &kynos::test::TestClient<App>, password: &str) {
        client
            .post("/v1/auth/register")
            .header(
                "x-capsule-protocol",
                capsule_core::crypto::primitives::PROTOCOL_VERSION,
            )
            .header("accept", "application/json")
            .json(&serde_json::json!({ "email": "somebody@example.test", "password": password }))
            .send()
            .await
            .assert_status(kynos::http::StatusCode::OK);
    }

    /// Attempt a sign-in and return the status the route answered with.
    async fn login(
        client: &kynos::test::TestClient<App>,
        password: &str,
    ) -> kynos::http::StatusCode {
        client
            .post("/v1/auth/login")
            .header(
                "x-capsule-protocol",
                capsule_core::crypto::primitives::PROTOCOL_VERSION,
            )
            .header("accept", "application/json")
            .json(&serde_json::json!({ "email": "somebody@example.test", "password": password }))
            .send()
            .await
            .status()
    }

    #[tokio::test]
    async fn the_memory_profile_assembles_a_server_whose_router_builds() {
        // The property nothing in this crate asserted before: seventeen module contexts, every
        // port filled, and a router Kynos will build out of them.
        let root = tempfile::tempdir().expect("a scratch directory");
        let assembled = assemble(&memory_config(root.path()))
            .await
            .expect("it assembles");
        assembled.service().expect("the router builds");
    }

    #[tokio::test]
    async fn the_blob_root_is_created_rather_than_required_to_exist() {
        let parent = tempfile::tempdir().expect("a scratch directory");
        let root = parent.path().join("does/not/exist/yet");
        let assembled = assemble(&memory_config(&root)).await.expect("it assembles");
        assert!(root.join("blobs").is_dir(), "the store's tree is created");
        assert!(
            root.join("ledger").is_dir(),
            "and the owner ledgers live beside the blobs"
        );
        drop(assembled);
    }

    #[tokio::test]
    async fn a_signing_key_that_is_not_ed25519_refuses_the_boot() {
        // `SigningKeyError` has always been documented as a startup failure. This is the startup
        // it fails.
        let root = tempfile::tempdir().expect("a scratch directory");
        let environment: BTreeMap<String, String> = [
            ("BLOB_ROOT".to_owned(), root.path().display().to_string()),
            (
                "JWT_ED25519_DER".to_owned(),
                base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    b"not a PKCS#8 document",
                ),
            ),
        ]
        .into_iter()
        .collect();
        let overrides = Overrides {
            memory: true,
            ..Overrides::default()
        };
        let config =
            Config::load(&environment, &overrides, Demands::Serve).expect("it is well-formed");
        let error = assemble(&config).await.expect_err("it refuses");
        assert!(matches!(error, BootError::SigningKey { .. }), "{error:?}");
    }

    #[tokio::test]
    async fn a_durable_backend_without_a_database_url_refuses_by_name() {
        // `Config::load` is the front door that demands `DATABASE_URL` for a durable boot (#520),
        // so this path's own demand is the backstop for a `Config` that reached it without one —
        // asserted to be made by name rather than discovered as a `None` somewhere further in.
        let root = tempfile::tempdir().expect("a scratch directory");
        let mut config = Config::load(
            &durable_environment(root.path()),
            &Overrides::default(),
            Demands::Serve,
        )
        .expect("it is well-formed");
        config.database_url = None;
        let error = assemble(&config).await.expect_err("it refuses");
        assert!(
            matches!(
                error,
                BootError::Missing {
                    key: "DATABASE_URL"
                }
            ),
            "{error:?}"
        );
    }

    /// The environment a durable `serve` needs, with backend URLs nothing answers on.
    ///
    /// `DATABASE_URL` names a port nothing listens on; a case that reaches a database replaces
    /// it.
    fn durable_environment(root: &std::path::Path) -> BTreeMap<String, String> {
        [
            ("BLOB_ROOT".to_owned(), root.display().to_string()),
            ("JWT_ED25519_DER".to_owned(), EXAMPLE_DER.to_owned()),
            (
                "DATABASE_URL".to_owned(),
                "postgres://127.0.0.1:1/capsule".to_owned(),
            ),
            ("VALKEY_URL".to_owned(), "redis://127.0.0.1:1".to_owned()),
            // A durable deployment supplies its own attestation identity rather than having one
            // derived from the token signer; `config` refuses without it.
            (
                "ATTESTATION_KEY_SEED".to_owned(),
                base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    [9_u8; 64].as_slice(),
                ),
            ),
        ]
        .into_iter()
        .collect()
    }

    /// What a durable boot does once it can actually reach a database.
    mod postgres_conformance {
        use super::{
            BTreeMap, BootError, Config, Demands, Overrides, assemble, durable_environment,
        };
        use crate::postgres::testing;

        /// A config pointing at `url`, with everything else a durable `serve` needs.
        fn config_for(root: &std::path::Path, url: &str) -> Config {
            let mut environment: BTreeMap<String, String> = durable_environment(root);
            environment.insert("DATABASE_URL".to_owned(), url.to_owned());
            Config::load(&environment, &Overrides::default(), Demands::Serve)
                .expect("it is well-formed")
        }

        /// A durable boot clears Postgres and refuses on Valkey rather than falling back.
        ///
        /// The property that matters is *which* refusal: falling back to the in-memory adapters
        /// is the one thing that must never happen, and refusing on the first port anybody
        /// happened to write would hide what is actually missing. Getting as far as the Valkey
        /// probe proves the pool opened and the schema check passed.
        ///
        /// This was two cases before #402 and #403 met. #403 asserted the Valkey refusal as a
        /// unit test, because nothing had yet demanded `DATABASE_URL` ahead of it; #402 asserted
        /// that the arm reached `AdapterUnavailable { key: "VALKEY_URL" }`, which was the
        /// refusal of a port *no adapter read*. Both halves have landed, so the durable arm now
        /// needs a real database before it dials Valkey at all, and the honest error at
        /// `redis://127.0.0.1:1` is [`BootError::Valkey`] — the adapter tried and nothing
        /// answered. One case, both lanes' assertions, and it needs a container to reach.
        #[tokio::test]
        async fn the_durable_arm_clears_postgres_and_refuses_on_an_unreachable_valkey() {
            let Some(database) = testing::start("the durable boot arm").await else {
                return;
            };
            let root = tempfile::tempdir().expect("a scratch directory");
            let config = config_for(root.path(), database.url());
            let error = assemble(&config).await.expect_err("it refuses");
            assert!(matches!(error, BootError::Valkey { .. }), "{error:?}");
            let message = format!("{error}");
            assert!(message.contains("VALKEY_URL"), "{message}");
            assert!(!message.contains("127.0.0.1:1"), "never the URL: {message}");
        }

        /// A database that has not been migrated refuses, and names the command that fixes it.
        ///
        /// The whole reason `serve` reads `seaql_migrations` rather than running
        /// `Migrator::up`: the alternative is a rolling deploy in which every replica races to
        /// apply the same schema change.
        #[tokio::test]
        async fn an_unmigrated_database_refuses_and_names_the_migration_command() {
            let Some(database) = testing::start("an unmigrated durable boot").await else {
                return;
            };
            database.roll_back().await;
            let root = tempfile::tempdir().expect("a scratch directory");
            let config = config_for(root.path(), database.url());
            let error = assemble(&config).await.expect_err("it refuses");
            assert!(matches!(error, BootError::Database { .. }), "{error:?}");
            let rendered = format!("{error}");
            assert!(
                rendered.contains("capsule-server-migration up"),
                "the refusal must name the command that fixes it, got {rendered}"
            );
            assert!(
                !rendered.contains(database.url()),
                "a startup error must never carry the connection URL: {rendered}"
            );
        }

        /// A Valkey for a case that drives the whole durable arm, or `None` — logged — when the
        /// Valkey tier is not admitted.
        ///
        /// The same gate and the same image tag as `tests/valkey.rs`, so one environment admits
        /// both. The container is returned so the caller holds it for the length of the case.
        async fn valkey(
            case: &str,
        ) -> Option<(
            String,
            testcontainers::ContainerAsync<testcontainers_modules::valkey::Valkey>,
        )> {
            use testcontainers::ImageExt as _;
            use testcontainers::runners::AsyncRunner as _;

            if std::env::var("CAPSULE_TEST_VALKEY").ok().as_deref() != Some("1") {
                eprintln!("skipping {case}: set CAPSULE_TEST_VALKEY=1 to admit the Valkey tier");
                return None;
            }
            let mut request =
                testcontainers_modules::valkey::Valkey::default().with_tag(VALKEY_TAG);
            if let Ok(userns) = std::env::var("CAPSULE_TEST_CONTAINER_USERNS") {
                request = request.with_userns_mode(&userns);
            }
            let container = request.start().await.expect("the Valkey container starts");
            let host = container.get_host().await.expect("a host");
            let port = container
                .get_host_port_ipv4(testcontainers_modules::valkey::VALKEY_PORT)
                .await
                .expect("a mapped port");
            Some((format!("redis://{host}:{port}"), container))
        }

        /// The image tag `capsule-server/compose.yaml` runs, as `tests/valkey.rs` pins it.
        const VALKEY_TAG: &str = "9.0.4";

        /// A durable `serve` assembles a whole server on Postgres and Valkey, and what it wrote
        /// is there for the next one.
        ///
        /// The property #446 exists for: the arm used to refuse after both probes because some
        /// ports had no durable adapter. Now it builds every module context, and an account
        /// registered through the first assembly signs in through a second one built over the same
        /// two backends — the in-process half of a restart. The process half is
        /// `tests/binary.rs`'s.
        #[tokio::test]
        async fn the_durable_arm_assembles_a_server_whose_state_outlives_it() {
            let Some(database) = testing::start("the durable boot arm, whole").await else {
                return;
            };
            let Some((valkey_url, _valkey)) = valkey("the durable boot arm, whole").await else {
                return;
            };
            let root = tempfile::tempdir().expect("a scratch directory");
            let mut config = config_for(root.path(), database.url());
            config.valkey_url = Some(valkey_url);

            let first = assemble(&config).await.expect("the durable arm assembles");
            let client = kynos::test::TestClient::new(first.service().expect("the router builds"));
            super::register(&client, "correct horse battery staple").await;
            assert_eq!(
                super::login(&client, "correct horse battery staple").await,
                kynos::http::StatusCode::OK
            );
            drop(client);
            drop(first);

            let second = assemble(&config).await.expect("it assembles again");
            let client = kynos::test::TestClient::new(second.service().expect("the router builds"));
            assert_eq!(
                super::login(&client, "correct horse battery staple").await,
                kynos::http::StatusCode::OK,
                "an account the first assembly registered is the second one's to sign in to"
            );
        }

        /// A durable `serve` that configures OIDC assembles on Postgres and Valkey (#460).
        ///
        /// It used to be refused by name before either probe, because its two stores had
        /// in-memory adapters only. Discovery is lazy, so an issuer nothing answers at still
        /// assembles; what this proves is that the durable arm now builds both OIDC ports.
        #[tokio::test]
        async fn the_durable_arm_assembles_with_a_relying_party() {
            let Some(database) = testing::start("the durable boot arm, with OIDC").await else {
                return;
            };
            let Some((valkey_url, _valkey)) = valkey("the durable boot arm, with OIDC").await
            else {
                return;
            };
            let root = tempfile::tempdir().expect("a scratch directory");
            let mut environment: BTreeMap<String, String> = durable_environment(root.path());
            environment.insert("DATABASE_URL".to_owned(), database.url().to_owned());
            environment.insert("VALKEY_URL".to_owned(), valkey_url);
            environment.insert(
                "OIDC_ISSUER".to_owned(),
                "http://127.0.0.1:9/nothing-listens-here".to_owned(),
            );
            environment.insert("OIDC_CLIENT_ID".to_owned(), "capsule".to_owned());
            let config = Config::load(&environment, &Overrides::default(), Demands::Serve)
                .expect("it is well-formed");
            assert!(config.oidc.is_some(), "the relying party is configured");
            let assembled = assemble(&config).await.expect("the durable arm assembles");
            assembled.service().expect("the router builds");
        }

        /// A durable `gc` keeps its marks between processes, so a later pass can sweep.
        ///
        /// The reason the maintenance arm exists: the collector marks a blob on one pass and
        /// sweeps it on a later one, so a mark store that forgets is a collector that can only
        /// ever mark. Each pass here assembles its own workers, as each `gc` invocation does.
        #[tokio::test]
        async fn a_durable_collection_sweeps_what_an_earlier_pass_marked() {
            use crate::gc::{Mode, collect};

            let Some(database) = testing::start("the durable maintenance arm").await else {
                return;
            };
            let Some((valkey_url, _valkey)) = valkey("the durable maintenance arm").await else {
                return;
            };
            let root = tempfile::tempdir().expect("a scratch directory");
            let address = "b".repeat(64);
            let shard = root.path().join("blobs").join("bb").join("bb");
            std::fs::create_dir_all(&shard).expect("the shard is created");
            let blob = shard.join(format!("{address}.bin"));
            std::fs::write(&blob, b"an orphan a crashed finalization left").expect("written");

            let mut environment: BTreeMap<String, String> = [
                ("BLOB_ROOT".to_owned(), root.path().display().to_string()),
                ("DATABASE_URL".to_owned(), database.url().to_owned()),
                ("VALKEY_URL".to_owned(), valkey_url),
            ]
            .into_iter()
            .collect();
            // No grace at all, so the second pass is the first one past the window.
            environment.insert("GC_GRACE_WINDOW_HOURS".to_owned(), "0".to_owned());
            let config = Config::load(&environment, &Overrides::default(), Demands::Maintenance)
                .expect("it is well-formed");

            let first = super::super::assemble_maintenance(&config)
                .await
                .expect("the durable maintenance arm assembles");
            let report = collect(&first.collection, Mode::Apply)
                .await
                .expect("a pass runs");
            assert_eq!(report.marked.len(), 1, "{report:?}");
            assert!(report.swept.is_empty(), "{report:?}");
            drop(first);

            let second = super::super::assemble_maintenance(&config)
                .await
                .expect("it assembles again");
            let report = collect(&second.collection, Mode::Apply)
                .await
                .expect("a pass runs");
            assert_eq!(
                report.swept.len(),
                1,
                "a later process sweeps what an earlier one marked: {report:?}"
            );
            assert!(!blob.exists(), "the orphan is reclaimed");
        }
    }

    #[tokio::test]
    async fn a_durable_backend_with_oidc_configured_goes_on_to_the_probes() {
        // Until #460 a durable boot with `OIDC_ISSUER` was refused before either probe, naming
        // the adapters it lacked. Both exist now, so it proceeds exactly as one without OIDC
        // does — to the first probe, which refuses here only because no database is named. The
        // name is dropped after loading, because `Config::load` demands it.
        let root = tempfile::tempdir().expect("a scratch directory");
        let environment: BTreeMap<String, String> = [
            ("BLOB_ROOT".to_owned(), root.path().display().to_string()),
            ("JWT_ED25519_DER".to_owned(), EXAMPLE_DER.to_owned()),
            (
                "DATABASE_URL".to_owned(),
                "postgres://127.0.0.1:1/capsule".to_owned(),
            ),
            ("VALKEY_URL".to_owned(), "redis://127.0.0.1:6379".to_owned()),
            (
                "ATTESTATION_KEY_SEED".to_owned(),
                base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    [9_u8; 64].as_slice(),
                ),
            ),
            (
                "OIDC_ISSUER".to_owned(),
                "https://idp.example.test".to_owned(),
            ),
            ("OIDC_CLIENT_ID".to_owned(), "capsule".to_owned()),
        ]
        .into_iter()
        .collect();
        let mut config = Config::load(&environment, &Overrides::default(), Demands::Serve)
            .expect("it is well-formed");
        config.database_url = None;
        assert!(config.oidc.is_some(), "the relying party is configured");
        let error = assemble(&config).await.expect_err("it refuses");
        assert!(
            matches!(
                error,
                BootError::Missing {
                    key: "DATABASE_URL"
                }
            ),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_ca_bundle_is_read_at_boot_and_refused_by_name_when_unusable() {
        let root = tempfile::tempdir().expect("a scratch directory");
        let bundle = root.path().join("idp-ca.pem");

        // Missing: refused, naming the path.
        let config = memory_config_with(
            root.path(),
            &[
                ("OIDC_ISSUER", "https://idp.example.test"),
                ("OIDC_CLIENT_ID", "capsule"),
                ("OIDC_CA_BUNDLE", &bundle.display().to_string()),
            ],
        );
        let error = assemble(&config).await.expect_err("it refuses");
        assert!(matches!(error, BootError::OidcClient { .. }), "{error:?}");
        assert!(format!("{error}").contains("idp-ca.pem"), "{error}");

        // Present and not a certificate: refused, and the contents are not echoed.
        std::fs::write(
            &bundle,
            "this is not a certificate, it is a secret-looking string",
        )
        .expect("writes");
        let error = assemble(&config).await.expect_err("it refuses");
        assert!(matches!(error, BootError::OidcClient { .. }), "{error:?}");
        assert!(!format!("{error}").contains("secret-looking"), "{error}");

        // A real CA certificate: the server boots, trusting it.
        let key = rcgen::KeyPair::generate().expect("a key");
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("params");
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = params.self_signed(&key).expect("a CA");
        let body = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, ca.der());
        let pem = format!("-----BEGIN CERTIFICATE-----\n{body}\n-----END CERTIFICATE-----\n");
        std::fs::write(&bundle, pem).expect("writes");
        let assembled = assemble(&config).await.expect("it assembles");
        assembled.service().expect("the router builds");
    }

    #[tokio::test]
    async fn the_memory_profile_assembles_with_a_relying_party_without_reaching_it() {
        // Discovery is lazy: an issuer nothing answers at is still a server that boots.
        let root = tempfile::tempdir().expect("a scratch directory");
        let config = memory_config_with(
            root.path(),
            &[
                ("OIDC_ISSUER", "http://127.0.0.1:9/nothing-listens-here"),
                ("OIDC_CLIENT_ID", "capsule"),
            ],
        );
        let assembled = assemble(&config).await.expect("it assembles");
        assembled.service().expect("the router builds");
    }

    #[tokio::test]
    async fn the_published_signing_key_is_the_one_the_tokens_verify_under() {
        // Not a coincidence to be re-checked at every deployment: `ServerInfo` is built from
        // `tokens.public_key()`, so there is no second copy for an operator to paste wrongly.
        use crate::auth::SessionTokens;
        use crate::store::SystemClock;

        let root = tempfile::tempdir().expect("a scratch directory");
        let config = memory_config(root.path());
        let assembled = assemble(&config).await.expect("it assembles");
        let expected = SessionTokens::from_pkcs8(
            config
                .signing_key_der
                .as_ref()
                .expect("the key is configured")
                .expose(),
            std::sync::Arc::new(SystemClock),
        )
        .expect("the key parses")
        .public_key()
        .to_vec();

        // Read back the way a client would, through the surface rather than through a field.
        let client = kynos::test::TestClient::new(assembled.service().expect("the router builds"));
        let body: serde_json::Value = client
            .get("/.well-known/capsule/server-info")
            .header("accept", "application/json")
            .send()
            .await
            .assert_status(kynos::http::StatusCode::OK)
            .json();
        let published = body["signing_key"].as_str().expect("it is published");
        assert_eq!(
            published,
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &expected)
        );
    }

    #[tokio::test]
    async fn the_capability_codec_signs_under_the_published_key() {
        // A peer verifies a capability against `server-info`'s `signing_key`. The codec is
        // built from the same DER as the session signer, so the two are one key — asserted
        // through the surface and through the codec, rather than assumed from the wiring.
        let root = tempfile::tempdir().expect("a scratch directory");
        let config = memory_config(root.path());
        let assembled = assemble(&config).await.expect("it assembles");
        let client = kynos::test::TestClient::new(assembled.service().expect("the router builds"));
        let body: serde_json::Value = client
            .get("/.well-known/capsule/server-info")
            .header("accept", "application/json")
            .send()
            .await
            .assert_status(kynos::http::StatusCode::OK)
            .json();
        let published = body["signing_key"].as_str().expect("it is published");
        let codec = crate::federation::CapabilityCodec::from_pkcs8(
            config
                .signing_key_der
                .as_ref()
                .expect("the key is configured")
                .expose(),
            config.server_domain.clone(),
            std::sync::Arc::new(crate::store::SystemClock),
        )
        .expect("the key parses");
        assert_eq!(
            published,
            base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                codec.public_key()
            )
        );
        assert_eq!(codec.server_id(), config.server_domain);
        assert!(
            body.get("federation_url").is_none(),
            "a deployment without FEDERATION_URL publishes no federation endpoint"
        );
    }

    #[tokio::test]
    async fn federation_url_is_published_only_when_federation_is_switched_on() {
        // Opt-in by the switch (#509), and the record is the only way a peer learns it. A URL
        // on its own is a deployment that never decided, so it publishes nothing and mounts a
        // federation surface that answers `404`.
        let root = tempfile::tempdir().expect("a scratch directory");
        for (enabled, published) in [(false, None), (true, Some("https://capsule.example/v1"))] {
            let mut pairs = vec![("FEDERATION_URL", "https://capsule.example/v1")];
            if enabled {
                pairs.push(("FEDERATION_ENABLED", "true"));
            }
            let config = memory_config_with(root.path(), &pairs);
            let assembled = assemble(&config).await.expect("it assembles");
            let client =
                kynos::test::TestClient::new(assembled.service().expect("the router builds"));
            let body: serde_json::Value = client
                .get("/.well-known/capsule/server-info")
                .header("accept", "application/json")
                .send()
                .await
                .assert_status(kynos::http::StatusCode::OK)
                .json();
            assert_eq!(
                body.get("federation_url")
                    .and_then(serde_json::Value::as_str),
                published,
                "FEDERATION_ENABLED={enabled}"
            );

            let report = client
                .post("/v1/federation/reports")
                .header("accept", "application/json")
                .header(
                    "x-capsule-protocol",
                    capsule_core::crypto::primitives::PROTOCOL_VERSION,
                )
                .json(&serde_json::json!({}))
                .send()
                .await;
            assert_eq!(
                report.status() == kynos::http::StatusCode::NOT_FOUND,
                !enabled,
                "FEDERATION_ENABLED={enabled}: report intake answered {}",
                report.status()
            );
        }
    }

    #[tokio::test]
    async fn the_published_protocol_window_is_the_configured_one() {
        let root = tempfile::tempdir().expect("a scratch directory");
        let config = memory_config(root.path());
        let assembled = assemble(&config).await.expect("it assembles");
        let client = kynos::test::TestClient::new(assembled.service().expect("the router builds"));
        let body: serde_json::Value = client
            .get("/.well-known/capsule/server-info")
            .header("accept", "application/json")
            .send()
            .await
            .assert_status(kynos::http::StatusCode::OK)
            .json();
        assert_eq!(body["protocol_version"]["min"], config.protocol_min);
        assert_eq!(body["protocol_version"]["max"], config.protocol_max);
        assert_eq!(body["server_id"], config.server_domain);
        assert_eq!(body["api_base_url"], config.api_base_url);
    }

    /// The window the handshake enforces and advertises is the configured one (issue #404).
    ///
    /// Before this the upload policy was `UploadPolicy::default()` regardless of `PROTOCOL_MIN`
    /// and `PROTOCOL_MAX`, so a deployment that narrowed its window published one range on
    /// `/.well-known/capsule/server-info` and enforced another on `POST /v1/upload`.
    #[tokio::test]
    async fn the_enforced_and_advertised_window_is_the_configured_one() {
        let root = tempfile::tempdir().expect("a scratch directory");
        let config = memory_config(root.path());
        let assembled = assemble(&config).await.expect("it assembles");
        let client = kynos::test::TestClient::new(assembled.service().expect("the router builds"));

        // Every response advertises the window, an exempt read included.
        let response = client
            .get("/v1/version")
            .header("accept", "application/json")
            .send()
            .await;
        response.assert_status(kynos::http::StatusCode::OK);
        assert_eq!(
            response.header("x-capsule-protocol-min"),
            Some(config.protocol_min.as_str())
        );
        assert_eq!(
            response.header("x-capsule-protocol-max"),
            Some(config.protocol_max.as_str())
        );
        assert_eq!(
            response.header("x-capsule-min-client-build"),
            Some(config.min_client_build.as_str())
        );

        // And the gate holds a write to the same window: a version one day below the
        // configured minimum is refused before authentication is even looked at. (A read would
        // be admitted at any date — threat-model/validation.md — which is why this is a `DELETE`.)
        let below = format!(
            "{}",
            config
                .protocol_min
                .parse::<jiff::civil::Date>()
                .expect("the configured minimum is a date")
                .yesterday()
                .expect("there is a day before it")
        );
        let refused = client
            .delete("/v1/upload/anything")
            .header("x-capsule-protocol", &below)
            .send()
            .await;
        refused.assert_status(kynos::http::StatusCode::UPGRADE_REQUIRED);
        assert_eq!(
            refused.header("x-capsule-protocol-min"),
            Some(config.protocol_min.as_str())
        );
    }

    #[tokio::test]
    async fn an_account_can_be_registered_and_signed_in_to() {
        // The whole point of the amended deliverable boundary: `mise run serve-memory` is a
        // server a client developer can point at, not a surface they can only read.
        let root = tempfile::tempdir().expect("a scratch directory");
        let assembled = assemble(&memory_config(root.path()))
            .await
            .expect("it assembles");
        let client = kynos::test::TestClient::new(assembled.service().expect("the router builds"));

        let registered: serde_json::Value = client
            .post("/v1/auth/register")
            .header(
                "x-capsule-protocol",
                capsule_core::crypto::primitives::PROTOCOL_VERSION,
            )
            .header("accept", "application/json")
            .json(&serde_json::json!({
                "email": "somebody@example.test",
                "password": "correct horse battery staple",
            }))
            .send()
            .await
            .assert_status(kynos::http::StatusCode::OK)
            .json();
        assert!(registered["access_token"].is_string(), "{registered}");

        let signed_in: serde_json::Value = client
            .post("/v1/auth/login")
            .header(
                "x-capsule-protocol",
                capsule_core::crypto::primitives::PROTOCOL_VERSION,
            )
            .header("accept", "application/json")
            .json(&serde_json::json!({
                "email": "somebody@example.test",
                "password": "correct horse battery staple",
            }))
            .send()
            .await
            .assert_status(kynos::http::StatusCode::OK)
            .json();
        assert!(signed_in["access_token"].is_string(), "{signed_in}");
    }

    #[tokio::test]
    async fn the_assembled_maintenance_workers_see_the_links_the_application_issues() {
        // #653, Decision 5: the share store lives in `Stores`, so the collector and the scrub
        // read the very store `issue_share` writes. Two stores would mean a collector that sees
        // no links and sweeps every record, which only an assembly-level case can catch.
        use base64::Engine as _;

        let root = tempfile::tempdir().expect("a scratch directory");
        let assembled = assemble(&memory_config(root.path()))
            .await
            .expect("it assembles");
        let client = kynos::test::TestClient::new(assembled.service().expect("the router builds"));

        let registered: serde_json::Value = client
            .post("/v1/auth/register")
            .header(
                "x-capsule-protocol",
                capsule_core::crypto::primitives::PROTOCOL_VERSION,
            )
            .header("accept", "application/json")
            .json(&serde_json::json!({
                "email": "somebody@example.test",
                "password": "correct horse battery staple",
            }))
            .send()
            .await
            .assert_status(kynos::http::StatusCode::OK)
            .json();
        let token = registered["access_token"]
            .as_str()
            .expect("an access token");

        let record = b"a share's sealed metadata record";
        let metadata = capsule_core::crypto::hash::hash_bytes(record).to_hex();
        client
            .post("/v1/shares")
            .header(
                "x-capsule-protocol",
                capsule_core::crypto::primitives::PROTOCOL_VERSION,
            )
            .header("authorization", &format!("Bearer {token}"))
            .header("accept", "application/json")
            .json(&serde_json::json!({
                "opaque_id": "0123456789abcdef0123456789abcdef",
                "metadata_hash": metadata,
                "serves": [metadata],
                "metadata_blob": base64::engine::general_purpose::STANDARD.encode(record),
            }))
            .send()
            .await
            .assert_status(kynos::http::StatusCode::CREATED);

        let address = crate::blob::ContentAddress::parse(&metadata).expect("an address");
        let report = crate::gc::collect(&assembled.maintenance.collection, crate::gc::Mode::Apply)
            .await
            .expect("a collection pass");
        assert!(
            !report.marked.contains(&address),
            "the collector must read the store the link was issued into: {report:?}"
        );
        let scrubbed = crate::scrub::scrub(
            &assembled.maintenance.scrub,
            crate::scrub::Depth::Structural,
        )
        .await
        .expect("a scrub pass");
        assert_eq!(
            scrubbed.count("orphan"),
            0,
            "and so must the scrub: {:?}",
            scrubbed.findings
        );
    }

    #[tokio::test]
    async fn a_locked_account_recovers_through_the_login_route_once_the_window_passes() {
        // Asserted through the **route** rather than against the adapter, because that is where
        // the property actually has to hold: `login` asks the directory first and answers `423`
        // before it verifies anything, so a lockout that did not decay would be an account no
        // request could ever open again — there is no unlock operation on any surface.
        //
        // A one-attempt threshold and a one-second window, crossed by moving an injected clock
        // rather than by sleeping (#784). Both numbers are settings rather than constants so this
        // is expressible: driving the default ten-failure ceiling through the route would cost
        // ten Argon2id verifications. A real wait raced the window on a loaded machine, so time
        // is the one input taken off the composition root under test; everything else is the
        // assembly `serve` runs. The clock starts at the real present because `jsonwebtoken`
        // checks `exp` against the system clock, so a token minted at the epoch would be stale.
        let root = tempfile::tempdir().expect("a scratch directory");
        let config = memory_config_with(
            root.path(),
            &[
                ("LOCKOUT_MAX_ATTEMPTS", "1"),
                ("LOCKOUT_WINDOW_SECONDS", "1"),
            ],
        );
        let clock = ManualClock::new(jiff::Timestamp::now());
        let assembled = assemble_on(&config, std::sync::Arc::new(clock.clone()))
            .await
            .expect("it assembles");
        let client = kynos::test::TestClient::new(assembled.service().expect("the router builds"));

        register(&client, "correct horse battery staple").await;
        assert_eq!(
            login(&client, "wrong").await,
            kynos::http::StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            login(&client, "correct horse battery staple").await,
            kynos::http::StatusCode::LOCKED,
            "the ceiling engages, and a correct password is told so rather than refused"
        );

        clock.advance(jiff::SignedDuration::from_millis(999));
        assert_eq!(
            login(&client, "correct horse battery staple").await,
            kynos::http::StatusCode::LOCKED,
            "inside the window the lockout holds, so the recovery below is the clock's doing"
        );

        clock.advance(jiff::SignedDuration::from_millis(1_001));
        assert_eq!(
            login(&client, "correct horse battery staple").await,
            kynos::http::StatusCode::OK,
            "the window passed, so the account is the owner's again"
        );
    }

    #[tokio::test]
    async fn a_durable_maintenance_command_without_a_database_url_refuses_by_name() {
        // An operator running `capsule-server scrub` without `--memory` is asking for the
        // durable stores, and the first one those workers read is the index. Naming
        // `VALKEY_URL` first would send them to configure the second thing before the first.
        // `Config::load` names both at once (#520); this is the boot path's backstop, reached by
        // a `Config` that lost both after loading.
        let root = tempfile::tempdir().expect("a scratch directory");
        let environment: BTreeMap<String, String> = [
            ("BLOB_ROOT".to_owned(), root.path().display().to_string()),
            (
                "DATABASE_URL".to_owned(),
                "postgres://127.0.0.1:1/capsule".to_owned(),
            ),
            ("VALKEY_URL".to_owned(), "redis://127.0.0.1:1".to_owned()),
        ]
        .into_iter()
        .collect();
        let mut config = Config::load(&environment, &Overrides::default(), Demands::Maintenance)
            .expect("maintenance demands nothing else");
        config.database_url = None;
        config.valkey_url = None;
        let error = super::assemble_maintenance(&config)
            .await
            .expect_err("it refuses");
        assert!(
            matches!(
                error,
                BootError::Missing {
                    key: "DATABASE_URL"
                }
            ),
            "{error:?}"
        );
        let message = format!("{error}");
        assert!(!message.contains("VALKEY_URL"), "{message}");
    }

    #[tokio::test]
    async fn a_wrong_password_is_refused_rather_than_granted() {
        // The property that makes the adapter real rather than permissive: the credential
        // double `tests/support/mod.rs` warns about would accept this.
        let root = tempfile::tempdir().expect("a scratch directory");
        let assembled = assemble(&memory_config(root.path()))
            .await
            .expect("it assembles");
        let client = kynos::test::TestClient::new(assembled.service().expect("the router builds"));
        client
            .post("/v1/auth/register")
            .header(
                "x-capsule-protocol",
                capsule_core::crypto::primitives::PROTOCOL_VERSION,
            )
            .header("accept", "application/json")
            .json(&serde_json::json!({
                "email": "somebody@example.test",
                "password": "correct horse battery staple",
            }))
            .send()
            .await
            .assert_status(kynos::http::StatusCode::OK);
        client
            .post("/v1/auth/login")
            .header(
                "x-capsule-protocol",
                capsule_core::crypto::primitives::PROTOCOL_VERSION,
            )
            .header("accept", "application/json")
            .json(&serde_json::json!({
                "email": "somebody@example.test",
                "password": "the wrong password entirely",
            }))
            .send()
            .await
            .assert_status(kynos::http::StatusCode::UNAUTHORIZED);
    }
}
