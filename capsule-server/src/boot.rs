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
//! # The seam, and what #403 still changes
//!
//! Selection is a two-arm `match` on [`Backends`] and not a trait. The `Arc<dyn Port>` fields in
//! [`Modules`] already **are** the abstraction; a second one over the top would abstract the
//! composition root from itself.
//!
//! The [`Backends::Durable`] arm now does its **Postgres half** (#402): it demands
//! `DATABASE_URL`, opens the pool, refuses to continue against a database whose schema is not
//! the one this binary was built for, and builds the durable adapters — the asset index, the
//! account store, the device-cohort map, the quota ledger and the membership store. Then it
//! still refuses, because
//! the Valkey half does not exist: `AuthStateStore`, `UploadSessionStore`, the three ceremony
//! stores and the rate-limit counters are #403's, and five other durable ports are #446's.
//!
//! Refusing there rather than filling those ports with in-memory adapters is the whole point.
//! design/filesystem/server.md is explicit — *"Required means required"* — and a server that came
//! up holding session state it will lose on the next restart is worse than one that does not
//! start. So the arm builds everything it honestly can, says exactly what is missing, and
//! #403 turns the last `Err` into an `Ok`.
//!
//! `store/mod.rs` has said since `S-C29` that *"Valkey is required; the server refuses to boot
//! without `VALKEY_URL`"*, and nothing enforced it until there was a boot path to enforce it in.
//! Now: no `VALKEY_URL` and no `--memory` is a configuration fault naming `VALKEY_URL`
//! ([`Config::load`]), and `VALKEY_URL` set reaches [`BootError::AdapterUnavailable`] naming the
//! issue that will honour it. Neither ever silently becomes an in-memory server.
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
//!   pass and sweeps it on a later pass once the grace window has passed, so a fresh process can
//!   only ever mark. Sweeping needs a durable `CollectionStore`, which is #446's — #402 landed
//!   the durable index the collector *reads*, not the marks it writes.

use std::sync::Arc;

use jiff::Timestamp;

use crate::album::authority::ProvisionedAuthority;
use crate::album::{AlbumContext, InMemoryAlbums};
use crate::app::{App, Modules};
use crate::attestation::{AttestationContext, InMemoryReceipts, LocalAttestationKey};
use crate::auth::{
    AuthCollaborators, AuthContext, Credentials, InMemoryAccounts, InMemoryTotp, SessionTokens,
    TotpCodes, TotpContext,
};
use crate::blob::FilesystemBlobStore;
use crate::config::{Backends, Config};
use crate::counter::{CounterContext, InMemoryCounters};
use crate::directory::{DeviceDirectoryContext, InMemoryDeviceDirectory};
use crate::discovery::revocation::InMemoryRevocations;
use crate::discovery::{DiscoveryContext, ProtocolWindow, ServerInfo};
use crate::drop::{DropContext, InMemoryDrops};
use crate::enrollment::EnrollmentContext;
use crate::escrow::{EscrowContext, InMemoryEscrow};
use crate::gc::CollectionContext;
use crate::gc::memory::InMemoryCollection;
use crate::index::memory::InMemoryAssetIndex;
use crate::membership::{InMemoryMembership, MembershipContext};
use crate::moderation::{InMemoryModeration, ModerationContext};
use crate::quota::{InMemoryQuota, QuotaContext, QuotaLimits};
use crate::scrub::ScrubContext;
use crate::serve::ServeContext;
use crate::share::{InMemoryShares, ShareContext};
use crate::store::SystemClock;
use crate::store::memory::{
    InMemoryAuthState, InMemoryChallenges, InMemoryChannels, InMemoryCohorts, InMemoryEnrollments,
    InMemoryUploadSessions,
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
    /// A durable backend was selected and its adapter is not written yet.
    ///
    /// Named with the issue that will honour it, because "not implemented" without a pointer is
    /// a dead end for whoever reads it.
    #[error("{key} selects a durable adapter that is not implemented yet (see {issue})")]
    AdapterUnavailable {
        /// The setting that selected it.
        key: &'static str,
        /// Where the work is tracked.
        issue: &'static str,
    },
    /// An operator command was run without `--memory` and one of the stores it reads has no
    /// durable adapter.
    ///
    /// Deliberately not [`Self::AdapterUnavailable`]: that one names `VALKEY_URL`, which an
    /// operator running `capsule-server scrub` has typically never set, and pointing them at a
    /// variable that would not have helped is worse than saying nothing.
    ///
    /// The durable **index** exists as of #402, so this no longer says otherwise. What is still
    /// missing is on the worker's own side: the collector marks a blob on one pass and sweeps it
    /// on a later one, so a volatile `CollectionStore` means a process can only ever mark; and
    /// the scrub reads the upload-session store to tell a live transfer apart from an orphan.
    #[error(
        "this command needs `--memory`: it reads stores that have no durable adapter yet — the \
         collector's marks and the upload sessions the scrub reconciles against (see {issue})"
    )]
    MaintenanceNeedsMemory {
        /// Where the work is tracked.
        issue: &'static str,
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

/// The two operator workers' collaborators.
///
/// Assembled **without any key material**, which is what makes `config`'s claim that
/// `gc`/`purge`/`scrub` need none structural rather than a promise: there is no signing key in
/// scope here to accidentally require. A maintenance host that had to hold the production
/// token-signing key to sweep a directory would be a reason to put the key on a maintenance
/// host.
///
/// Neither worker has a wire surface, so neither is reachable through the router — which is why
/// they are a separate assembly rather than fields on [`App`].
#[derive(Debug)]
pub struct Maintenance {
    /// The collector's collaborators (`gc`, `purge`).
    pub collection: CollectionContext,
    /// The integrity scrub's collaborators (`scrub`).
    pub scrub: ScrubContext,
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
    let stores = stores(config).await?;
    match config.backends {
        Backends::Memory => memory(config, stores),
        Backends::Durable => {
            // The Postgres half runs for real — `DATABASE_URL` is demanded, the pool is opened,
            // and a schema that is not the one this binary was built for refuses here — and then
            // the arm still refuses, because the ports it cannot fill are the ones a server
            // loses state without. See the module docs.
            //
            // The connection is dropped rather than handed on. Constructing the adapters
            // and throwing them away would be theatre in production code; that they *do*
            // compose out of exactly what this function has is asserted in
            // `tests::postgres_conformance` instead, which is where an assertion belongs.
            let _connection = open_durable_database(config).await?;
            Err(durable_ports_owed())
        }
    }
}

/// Open the durable pool and refuse a schema this binary was not built for.
///
/// The check is **not** a migration. `capsule-server` cannot link the migrator — see
/// `capsule-server/migration`'s manifest for the `chrono` gate that forces it — and a server that
/// migrated on start would run the migration once per replica during a rolling deploy anyway.
/// What it does instead is read `seaql_migrations` and refuse, naming the command an operator has
/// to run.
///
/// `DATABASE_URL` is demanded here rather than by [`crate::config::Demands`] because it is this
/// *path* that needs it: `gc`, `purge` and `scrub` never reach here, and `serve --memory` does
/// not either.
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
/// Returns [`BootError`] for the blob root or an unimplemented durable backend. It cannot fail
/// on key material, because it asks for none.
pub async fn assemble_maintenance(config: &Config) -> Result<Maintenance, BootError> {
    let stores = stores(config).await?;
    match config.backends {
        Backends::Memory => {
            let maintenance = stores.maintenance(config.grace_window);
            tracing::info!(
                blob_root = %stores.root.display(),
                grace_window = %config.grace_window,
                "assembled the operator workers on the in-memory adapters"
            );
            Ok(maintenance)
        }
        // **Not** `durable_ports_owed()`. A maintenance command reaching here has almost always
        // set no backend variable at all — `gc`/`purge`/`scrub` never demand `VALKEY_URL`, so
        // naming it would send an operator to configure a variable that would not have helped.
        //
        // The durable index exists as of #402, and it is not what is missing. Both workers read
        // a store that has no durable adapter: the collector marks a blob on one pass and sweeps
        // it on a later one, so a `CollectionStore` that forgets is a collector that can only
        // ever mark; and the scrub reconciles the index against the upload sessions, which is
        // how it tells a live transfer apart from an orphan.
        Backends::Durable => Err(BootError::MaintenanceNeedsMemory {
            issue: "#446 (the collector's marks) and #403 (upload sessions)",
        }),
    }
}

/// The refusal `store/mod.rs` documents, now reached only for the ports #402 did not land.
///
/// `Config::load` already turned "no `VALKEY_URL` and no `--memory`" into a configuration fault
/// naming the variable, so reaching here means the operator *did* set it — and the honest answer
/// is that nothing reads it yet. The Postgres half of the arm has run by this point: the pool is
/// open and the schema has been checked, so a durable deployment now fails on the port that is
/// genuinely absent rather than on the first one anybody happened to write.
fn durable_ports_owed() -> BootError {
    BootError::AdapterUnavailable {
        key: "VALKEY_URL",
        issue: "#403 (session, upload-session, ceremony and counter state)",
    }
}

/// The stores every subcommand shares, and the only one of them that is durable.
///
/// A struct rather than six locals because [`assemble`] and [`assemble_maintenance`] must build
/// the *same* stores: two functions each opening their own index is two servers that disagree
/// about what is in it.
#[derive(Debug)]
struct Stores {
    root: std::path::PathBuf,
    clock: Arc<SystemClock>,
    blobs: Arc<FilesystemBlobStore>,
    index: Arc<InMemoryAssetIndex>,
    uploads: Arc<InMemoryUploadSessions>,
    marks: Arc<InMemoryCollection>,
    quotas: Arc<InMemoryQuota>,
}

impl Stores {
    /// The operator workers over these stores.
    fn maintenance(&self, grace_window: jiff::SignedDuration) -> Maintenance {
        Maintenance {
            collection: CollectionContext::new(
                self.index.clone(),
                self.blobs.clone(),
                self.marks.clone(),
                self.quotas.clone(),
                self.clock.clone(),
                grace_window,
            ),
            scrub: ScrubContext::new(self.index.clone(), self.blobs.clone(), self.uploads.clone()),
        }
    }
}

/// Open the blob root and build the stores over it.
///
/// The blob root is the one thing on this path that touches the filesystem, and it is refused
/// rather than deferred: a store that cannot be created now is a store every upload will fail
/// against at write time, and a server that accepts bytes it cannot keep is worse than one that
/// does not start.
async fn stores(config: &Config) -> Result<Stores, BootError> {
    let root = config
        .blob_root
        .clone()
        .ok_or(BootError::Missing { key: "BLOB_ROOT" })?;
    let clock = Arc::new(SystemClock);
    let blobs =
        Arc::new(
            FilesystemBlobStore::open(&root)
                .await
                .map_err(|error| BootError::BlobRoot {
                    root: root.display().to_string(),
                    detail: error.to_string(),
                })?,
        );
    Ok(Stores {
        root,
        index: Arc::new(InMemoryAssetIndex::new()),
        uploads: Arc::new(InMemoryUploadSessions::with_default_ttl(clock.clone())),
        marks: Arc::new(InMemoryCollection::new()),
        quotas: Arc::new(InMemoryQuota::new()),
        blobs,
        clock,
    })
}

/// The development profile: every in-crate adapter, over a real blob store and a real clock.
#[allow(
    clippy::too_many_lines,
    reason = "seventeen module contexts, named once each; splitting it would hide the shape"
)]
fn memory(config: &Config, stores: Stores) -> Result<Assembled, BootError> {
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
    let clock = stores.clock.clone();
    let blobs = stores.blobs.clone();
    let index = stores.index.clone();
    let uploads = stores.uploads.clone();
    let marks = stores.marks.clone();
    let quotas = stores.quotas.clone();

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

    // One verifier, shared: building it costs an Argon2id hash (the timing-equalized miss's
    // decoy), and that is a startup cost rather than a per-request one.
    let credentials = Credentials::new().map_err(|error| BootError::Credentials {
        detail: error.detail,
    })?;
    let accounts = Arc::new(InMemoryAccounts::new(
        credentials,
        clock.clone(),
        config.lockout_attempts,
        config.lockout_window,
    ));

    let albums = Arc::new(InMemoryAlbums::new());
    let members = Arc::new(InMemoryMembership::new());
    let directories = Arc::new(InMemoryDeviceDirectory::new());
    // The production write authority (`S-C19`/`S-C20`), not a permissive double: it reads the
    // album's own pin and the account's published device directory, so invariants 6 and 7 mean
    // what they say even in the development profile.
    let authority = Arc::new(ProvisionedAuthority::new(
        albums.clone(),
        directories.clone(),
        members.clone(),
        clock.clone(),
    ));
    let receipts = Arc::new(InMemoryReceipts::new());
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

    let server_info = Arc::new(ServerInfo::new(
        config.server_domain.clone(),
        config.api_base_url.clone(),
        ProtocolWindow {
            min: config.protocol_min.clone(),
            max: config.protocol_max.clone(),
        },
        tokens.public_key().to_vec(),
    ));

    let app = App::new(Modules {
        auth: AuthContext::new(AuthCollaborators {
            sessions: Arc::new(InMemoryAuthState::with_default_ttl(clock.clone())),
            accounts: accounts.clone(),
            registry: accounts.clone(),
            profiles: accounts.clone(),
            passwords: accounts.clone(),
            challenges: Arc::new(InMemoryChallenges::with_default_ttl(clock.clone())),
            cohorts: Arc::new(InMemoryCohorts::new()),
            tokens: tokens.clone(),
            clock: clock.clone(),
        }),
        totp: TotpContext::new(
            Arc::new(InMemoryTotp::new()),
            // The issuer is what an authenticator app shows beside the code, so it is this
            // deployment's own name rather than a constant every deployment shares.
            Arc::new(TotpCodes::new(config.server_domain.clone())),
        ),
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
        ),
        serve: ServeContext::new(
            index.clone(),
            blobs.clone(),
            marks.clone(),
            uploads.clone(),
            crate::serve::owned_assets(),
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
        ),
        discovery: DiscoveryContext::new(
            server_info,
            Arc::new(InMemoryRevocations::new(clock.clone())),
        ),
        escrow: EscrowContext::new(Arc::new(InMemoryEscrow::new()), clock.clone()),
        enrollment: EnrollmentContext::new(
            Arc::new(InMemoryEnrollments::with_default_ttl(clock.clone())),
            Arc::new(InMemoryChannels::with_default_ttl(clock.clone())),
            clock.clone(),
        ),
        moderation: ModerationContext::new(Arc::new(InMemoryModeration::new())),
        share: ShareContext::new(
            Arc::new(InMemoryShares::new()),
            blobs.clone(),
            clock.clone(),
        ),
        drops: DropContext::new(
            Arc::new(InMemoryDrops::new()),
            uploads.clone(),
            blobs.clone(),
            clock.clone(),
        ),
        counters: CounterContext::new(Arc::new(InMemoryCounters::new()), clock.clone()),
    });

    tracing::info!(
        blob_root = %root.display(),
        server_id = %config.server_domain,
        protocol_min = %config.protocol_min,
        protocol_max = %config.protocol_max,
        "assembled a server on the in-memory adapters"
    );

    Ok(Assembled {
        app,
        // The same stores, so "upload it, then let the collector see it" is a property of the
        // server rather than of two disconnected assemblies.
        maintenance: stores.maintenance(config.grace_window),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{App, BootError, assemble};
    use crate::config::{Config, Demands, Overrides};

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
        // `Config::load` demands `VALKEY_URL` for the durable path and deliberately does not
        // demand `DATABASE_URL` — `gc`, `purge` and `scrub` load the same configuration and need
        // neither. So the demand belongs to this *path*, and this is the assertion that it is
        // made rather than discovered as a `None` somewhere further in.
        let root = tempfile::tempdir().expect("a scratch directory");
        let config = Config::load(
            &durable_environment(root.path()),
            &Overrides::default(),
            Demands::Serve,
        )
        .expect("it is well-formed");
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

    /// The environment a durable `serve` needs, minus the backend URLs a case adds itself.
    fn durable_environment(root: &std::path::Path) -> BTreeMap<String, String> {
        [
            ("BLOB_ROOT".to_owned(), root.display().to_string()),
            ("JWT_ED25519_DER".to_owned(), EXAMPLE_DER.to_owned()),
            ("VALKEY_URL".to_owned(), "redis://127.0.0.1:6379".to_owned()),
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
        use std::sync::Arc;

        use super::{
            BTreeMap, BootError, Config, Demands, Overrides, assemble, durable_environment,
        };
        use crate::auth::{Credentials, PostgresAccounts};
        use crate::index::postgres::PostgresAssetIndex;
        use crate::membership::PostgresMembership;
        use crate::postgres::testing;
        use crate::quota::PostgresQuota;
        use crate::store::{PostgresCohorts, SystemClock};

        /// A config pointing at `url`, with everything else a durable `serve` needs.
        fn config_for(root: &std::path::Path, url: &str) -> Config {
            let mut environment: BTreeMap<String, String> = durable_environment(root);
            environment.insert("DATABASE_URL".to_owned(), url.to_owned());
            Config::load(&environment, &Overrides::default(), Demands::Serve)
                .expect("it is well-formed")
        }

        /// A durable boot gets past Postgres and refuses on the ports that are genuinely absent.
        ///
        /// The property that matters is *which* refusal: falling back to the in-memory adapters
        /// is the one thing that must never happen, and refusing on the first port anybody
        /// happened to write would hide what is actually missing. Reaching
        /// `AdapterUnavailable` proves the pool opened and the schema check passed.
        #[tokio::test]
        async fn the_durable_arm_clears_postgres_and_refuses_on_the_valkey_ports() {
            let Some(database) = testing::start("the durable boot arm").await else {
                return;
            };
            let root = tempfile::tempdir().expect("a scratch directory");
            let config = config_for(root.path(), database.url());
            let error = assemble(&config).await.expect_err("it refuses");
            assert!(
                matches!(
                    error,
                    BootError::AdapterUnavailable {
                        key: "VALKEY_URL",
                        ..
                    }
                ),
                "{error:?}"
            );
            assert!(format!("{error}").contains("#403"), "{error}");
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

        /// The five Postgres adapters compose out of exactly what the boot path has.
        ///
        /// Asserted here rather than by constructing them in `assemble` and throwing them away:
        /// production code that builds something it cannot use is theatre, and what #403 needs
        /// to know is that its one hunk will type-check. Every constructor takes the shared
        /// connection plus values `Config` already carries.
        #[tokio::test]
        async fn every_postgres_adapter_composes_from_the_boot_configuration() {
            let Some(database) = testing::start("the durable adapter set").await else {
                return;
            };
            let root = tempfile::tempdir().expect("a scratch directory");
            let config = config_for(root.path(), database.url());
            let connection = crate::postgres::connect(&config.database_url.clone().expect("set"))
                .await
                .expect("the pool opens");
            let credentials = Credentials::new().expect("the platform hashes");
            let clock = Arc::new(SystemClock);

            let index: Arc<dyn crate::index::AssetIndex> =
                Arc::new(PostgresAssetIndex::new(connection.clone()));
            let accounts = Arc::new(PostgresAccounts::new(
                connection.clone(),
                credentials,
                clock,
                config.lockout_attempts,
                config.lockout_window,
            ));
            let cohorts: Arc<dyn crate::store::CohortStore> =
                Arc::new(PostgresCohorts::new(connection.clone()));
            let quotas: Arc<dyn crate::quota::QuotaStore> =
                Arc::new(PostgresQuota::new(connection.clone()));
            let members: Arc<dyn crate::membership::MembershipStore> =
                Arc::new(PostgresMembership::new(connection));

            // Each one answers through its port, which is what makes this a boot check rather
            // than a compile check: the schema the migration applied is the schema the adapters
            // query.
            let owner = crate::store::OwnerId::new("boot-probe-owner");
            assert_eq!(index.head_seq(&owner).await.expect("the index answers"), 0);
            let user = crate::store::UserId::new("boot-probe-user");
            assert!(
                crate::auth::AccountProfiles::read(accounts.as_ref(), &user)
                    .await
                    .expect("the account store answers")
                    .is_none()
            );
            assert!(
                cohorts
                    .cohorts_for_user(&user)
                    .await
                    .expect("the cohort map answers")
                    .is_empty()
            );
            assert_eq!(
                quotas.usage(&user).await.expect("the ledger answers").used,
                0
            );
            let album = crate::store::AlbumId::new("boot-probe-album");
            assert_eq!(
                members
                    .membership(&album, &user)
                    .await
                    .expect("the membership store answers"),
                crate::membership::Membership::Never
            );
        }
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
    async fn a_locked_account_recovers_through_the_login_route_once_the_window_passes() {
        // Asserted through the **route** rather than against the adapter, because that is where
        // the property actually has to hold: `login` asks the directory first and answers `423`
        // before it verifies anything, so a lockout that did not decay would be an account no
        // request could ever open again — there is no unlock operation on any surface.
        //
        // A one-attempt threshold, a one-second window and a real wait. Both numbers are
        // settings rather than constants precisely so this is expressible: driving the default
        // ten-failure ceiling through the route would cost ten Argon2id verifications, and on a
        // loaded machine the gaps between them can themselves exceed a short window — a test
        // whose setup races its own subject. The alternative was a clock seam through the whole
        // composition root for one case, and the composition root is the thing under test.
        let root = tempfile::tempdir().expect("a scratch directory");
        let config = memory_config_with(
            root.path(),
            &[
                ("LOCKOUT_MAX_ATTEMPTS", "1"),
                ("LOCKOUT_WINDOW_SECONDS", "1"),
            ],
        );
        let assembled = assemble(&config).await.expect("it assembles");
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

        tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
        assert_eq!(
            login(&client, "correct horse battery staple").await,
            kynos::http::StatusCode::OK,
            "the window passed, so the account is the owner's again"
        );
    }

    #[tokio::test]
    async fn a_maintenance_command_is_told_it_needs_memory_and_not_valkey() {
        // An operator running `capsule-server scrub` has typically set no backend variable at
        // all. Naming `VALKEY_URL` would send them to configure something that would not have
        // helped; what is missing is the durable index these workers read.
        let root = tempfile::tempdir().expect("a scratch directory");
        let environment: BTreeMap<String, String> =
            [("BLOB_ROOT".to_owned(), root.path().display().to_string())]
                .into_iter()
                .collect();
        let config = Config::load(&environment, &Overrides::default(), Demands::Maintenance)
            .expect("maintenance demands nothing else");
        let error = super::assemble_maintenance(&config)
            .await
            .expect_err("it refuses");
        assert!(
            matches!(error, BootError::MaintenanceNeedsMemory { .. }),
            "{error:?}"
        );
        let message = format!("{error}");
        assert!(message.contains("--memory"), "{message}");
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
