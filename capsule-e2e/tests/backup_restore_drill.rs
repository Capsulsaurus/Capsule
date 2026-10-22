//! **E2E backup drill** — a durable deployment backed up, restored onto a fresh host, and its
//! index rebuilt from the owner ledgers alone (issue #676).
//!
//! The procedure under test is the one the self-hosting guide gives an operator
//! (`capsule-docs/src/content/docs/guides/self-hosting.md`, "Backups" and "Restoring onto a new
//! host"), run against the real durable profile: a Postgres 18 and a Valkey in containers, the
//! filesystem blob store, the composition root `capsule-server serve` runs, the real SDK and a
//! real library on the client side.
//!
//! 1. **Host A** serves a library: two assets pushed with the CLI's own `capsule push`.
//! 2. **A hot backup**, in the guide's order: `pg_dump` first, then a third asset is pushed, and
//!    only then is `BLOB_ROOT` copied. The copy therefore holds a blob and a ledger entry the
//!    dump has no row for, which is the gap a hot backup always has.
//! 3. **Host B** is fresh: an empty Postgres the dump is `pg_restore`d into, a Valkey with
//!    nothing in it (Valkey is deliberately not restored), and the copied blob directory.
//!    - the session issued by host A is refused: its session record lived in Valkey;
//!    - `capsule-server rebuild` (dry run) names exactly the third asset as the row to restore,
//!      and `--apply` restores it;
//!    - a deep scrub finds nothing;
//!    - the account signs in again, the feed lists all three assets at the sequence numbers host
//!      A gave them, every blob the library uploaded is served byte for byte as host A stored
//!      it, and `capsule restore` writes every original back byte for byte.
//! 4. **The index-rebuild drill**: a database that never saw a backup, migrated and empty,
//!    rebuilt from the same blob directory's owner ledgers, holds the same rows as host B's
//!    restored index.
//!
//! Gated on `CAPSULE_TEST_POSTGRES=1` and `CAPSULE_TEST_VALKEY=1`, like every container tier: it
//! prints why it skipped and passes otherwise. The test lives under a `postgres_conformance`
//! module so `.config/nextest.toml` runs it in the one-thread `containers` group and
//! `mise run test-rust-containers` selects it.

#![cfg(unix)]

mod postgres_conformance {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    use capsule_cli::remote::{self, PushOptions, RemoteConfig};
    use capsule_cli::restore::{
        AssetRestoreStatus, RESTORE_RANGE_WINDOW, RestoreOptions, RestoreScope,
    };
    use capsule_cli::session::SessionStore;
    use capsule_e2e::fixtures::{large_synthetic_jpeg, synthetic_jpeg, synthetic_jpeg_sized};
    use capsule_e2e::{
        ATTESTATION_KEY_SEED, Device, DurableBackends, PROTOCOL_VERSION, Server, entry_for,
    };
    use capsule_sdk::auth::Session;
    use capsule_sdk::fetch::{BlobSource as _, HttpBlobSource, RangeOutcome};
    use capsule_sdk::net::ConnectionClass;
    use capsule_sdk::push::bundle_blobs;
    use capsule_server::blob::address::{ContentAddress, blob_path};
    use capsule_server::boot;
    use capsule_server::config::{Config, Demands, Overrides};
    use capsule_server::gc::Mode;
    use capsule_server::index::postgres::PostgresAssetIndex;
    use capsule_server::index::{AssetIndex as _, AssetRow};
    use capsule_server::ledger::rebuild::{RebuildReport, rebuild};
    use capsule_server::scrub::{Depth, scrub};
    use server_migration::MigratorTrait as _;
    use testcontainers::core::{CmdWaitFor, ExecCommand};
    use testcontainers::runners::AsyncRunner as _;
    use testcontainers::{ContainerAsync, ImageExt as _};
    use testcontainers_modules::postgres::Postgres;
    use testcontainers_modules::valkey::{VALKEY_PORT, Valkey};
    use uuid::Uuid;

    /// The tags `capsule-server/compose.yaml` runs, as the server's own harnesses pin them.
    const POSTGRES_TAG: &str = "18";
    /// See [`POSTGRES_TAG`].
    const VALKEY_TAG: &str = "9.0.4";
    /// The rootless-podman user-namespace mode, when the host needs one
    /// (`capsule_server::postgres::testing`).
    const USERNS_MODE: &str = "CAPSULE_TEST_CONTAINER_USERNS";
    /// Where the dump sits inside a container: written there by `pg_dump` on host A, copied
    /// in there for `pg_restore` on host B.
    const DUMP_PATH: &str = "/tmp/capsule.dump";
    /// The testcontainers module's own superuser and database.
    const PG_USER: &str = "postgres";
    /// The database the server runs on.
    const PG_DATABASE: &str = "postgres";
    /// The database the index-rebuild drill rebuilds into, beside host B's restored one.
    const REBUILT_DATABASE: &str = "rebuilt";

    /// Whether both container tiers are admitted.
    fn admitted(case: &str) -> bool {
        let gate = |name: &str| std::env::var(name).ok().as_deref() == Some("1");
        if gate("CAPSULE_TEST_POSTGRES") && gate("CAPSULE_TEST_VALKEY") {
            return true;
        }
        eprintln!(
            "skipping {case}: the backup drill needs CAPSULE_TEST_POSTGRES=1 and \
             CAPSULE_TEST_VALKEY=1 with a reachable container runtime"
        );
        false
    }

    fn userns() -> Option<String> {
        std::env::var(USERNS_MODE)
            .ok()
            .filter(|mode| !mode.is_empty())
    }

    /// One host's two external services.
    struct Host {
        postgres: ContainerAsync<Postgres>,
        _valkey: ContainerAsync<Valkey>,
        backends: DurableBackends,
    }

    impl Host {
        /// A Postgres and a Valkey, both empty. `dump`, when given, is copied into the Postgres
        /// container at [`DUMP_PATH`] before it starts, for a restore.
        async fn start(dump: Option<Vec<u8>>) -> Self {
            let mut postgres = Postgres::default()
                .with_tag(POSTGRES_TAG)
                // Off the image's declared volume, for the reason `postgres::testing` gives.
                .with_env_var("PGDATA", "/tmp/pgdata");
            if let Some(mode) = userns() {
                postgres = postgres.with_userns_mode(&mode);
            }
            if let Some(dump) = dump {
                postgres = postgres.with_copy_to(DUMP_PATH, dump);
            }
            let postgres = postgres
                .start()
                .await
                .expect("the Postgres container starts");
            let host = postgres.get_host().await.expect("a host");
            let port = postgres.get_host_port_ipv4(5432).await.expect("a port");
            let database_url = format!("postgres://{PG_USER}:postgres@{host}:{port}");

            let mut valkey = Valkey::default().with_tag(VALKEY_TAG);
            if let Some(mode) = userns() {
                valkey = valkey.with_userns_mode(&mode);
            }
            let valkey = valkey.start().await.expect("the Valkey container starts");
            let host = valkey.get_host().await.expect("a host");
            let port = valkey
                .get_host_port_ipv4(VALKEY_PORT)
                .await
                .expect("a port");

            Self {
                postgres,
                _valkey: valkey,
                backends: DurableBackends {
                    database_url: format!("{database_url}/{PG_DATABASE}"),
                    valkey_url: format!("redis://{host}:{port}"),
                },
            }
        }

        /// The connection string of `database` on this host's Postgres.
        fn database_url(&self, database: &str) -> String {
            let server = self
                .backends
                .database_url
                .strip_suffix(&format!("/{PG_DATABASE}"))
                .expect("the URL names the default database");
            format!("{server}/{database}")
        }

        /// Run `command` inside the Postgres container, returning its stdout; panics with its
        /// stderr when it exits non-zero.
        async fn psql_exec(&self, command: &[&str]) -> Vec<u8> {
            let mut result = self
                .postgres
                .exec(
                    ExecCommand::new(command.iter().copied())
                        .with_cmd_ready_condition(CmdWaitFor::exit()),
                )
                .await
                .unwrap_or_else(|error| panic!("{command:?} could not run: {error}"));
            let stdout = result.stdout_to_vec().await.expect("stdout reads");
            let stderr = result.stderr_to_vec().await.expect("stderr reads");
            let code = result.exit_code().await.expect("the exit code reads");
            assert_eq!(
                code,
                Some(0),
                "{command:?} failed: {}",
                String::from_utf8_lossy(&stderr)
            );
            stdout
        }

        /// Apply the server's schema to `database`: what `capsule-server-migration up` does.
        async fn migrate(&self, database: &str) {
            let connection = capsule_server::postgres::connect(&self.database_url(database))
                .await
                .expect("the database accepts a connection");
            server_migration::Migrator::up(&connection, None)
                .await
                .expect("the schema applies");
        }
    }

    /// Copy the tree at `from` into `to`, as `cp -a` or `rsync -a` would for an operator.
    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).expect("the target directory exists");
        for entry in std::fs::read_dir(from).expect("the source directory reads") {
            let entry = entry.expect("a directory entry");
            let target = to.join(entry.file_name());
            if entry.file_type().expect("a file type").is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), &target).expect("the file copies");
            }
        }
    }

    /// Every file under `root`, by path relative to it, with its bytes.
    fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(root: &Path, dir: &Path, into: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(dir).expect("the directory reads") {
                let path = entry.expect("a directory entry").path();
                if path.is_dir() {
                    walk(root, &path, into);
                } else {
                    let relative = path.strip_prefix(root).expect("under the root").to_owned();
                    into.insert(relative, std::fs::read(&path).expect("the file reads"));
                }
            }
        }
        let mut files = BTreeMap::new();
        walk(root, root, &mut files);
        files
    }

    /// The CLI's endpoints on `server`.
    fn remote_config(server: &Server) -> RemoteConfig {
        RemoteConfig {
            auth_endpoint: server.auth_base(),
            sync_endpoint: server.base_url().to_owned(),
            upload_endpoint: server.upload_base(),
            albums_endpoint: server.albums_base(),
            protocol_version: PROTOCOL_VERSION.to_owned(),
        }
    }

    /// The session `capsule auth login` would have persisted, in a fresh CLI home.
    async fn persisted(session: &Session) -> (tempfile::TempDir, SessionStore) {
        let home = tempfile::tempdir().expect("a temp CLI home");
        let store = SessionStore::new(home.path().join("session.json"));
        let exported = session.export().await.expect("a live session exports");
        store.save(&exported).expect("the session persists");
        (home, store)
    }

    /// `capsule push` over `session`, requiring every asset to reach verified custody.
    async fn push(server: &Server, device: &Device, session: &Session) {
        let (_home, store) = persisted(session).await;
        let summary = remote::push(
            &remote_config(server),
            &store,
            &device.workspace,
            PushOptions::default(),
        )
        .await
        .expect("the push completes");
        assert_eq!(summary.failed_assets, 0, "{summary:#?}");
        assert_eq!(summary.verified_assets, summary.assets, "{summary:#?}");
    }

    /// The comparable facts of every row of the index at `database_url`, by asset.
    ///
    /// What a rebuild reconstructs: identity, placement, state, numbering, chain head and blob
    /// references. Timestamps are excluded: a rebuilt row is stamped when it is rebuilt.
    async fn index_rows(database_url: &str) -> BTreeMap<String, String> {
        let connection = capsule_server::postgres::connect(database_url)
            .await
            .expect("the database accepts a connection");
        let index = PostgresAssetIndex::new(connection);
        let rows: Vec<AssetRow> = index.rows(None, 1024).await.expect("the rows read");
        rows.into_iter()
            .map(|row| {
                let blobs: BTreeSet<String> = row
                    .blobs
                    .iter()
                    .map(|blob| format!("{:?}:{}", blob.role, blob.address.as_str()))
                    .collect();
                (
                    row.asset_id.to_string(),
                    format!(
                        "owner={} album={} state={:?} first_seq={:?} sync_seq={:?} \
                         chain_head={:?} suite={} protocol={} blobs={blobs:?}",
                        row.owner_id,
                        row.album_id,
                        row.state,
                        row.first_seq,
                        row.sync_seq,
                        row.chain_head,
                        row.crypto_suite_id,
                        row.protocol_version,
                    ),
                )
            })
            .collect()
    }

    /// One `capsule-server rebuild` pass over `blob_root` into `database_url`.
    async fn rebuild_pass(
        blob_root: &Path,
        database_url: &str,
        valkey_url: &str,
        mode: Mode,
    ) -> RebuildReport {
        let mut env = BTreeMap::new();
        env.insert("BLOB_ROOT".to_owned(), blob_root.display().to_string());
        env.insert("DATABASE_URL".to_owned(), database_url.to_owned());
        env.insert("VALKEY_URL".to_owned(), valkey_url.to_owned());
        // The restored host keeps its secrets: with the seed, the ledgers are verified by
        // signature rather than by links alone.
        env.insert(
            "ATTESTATION_KEY_SEED".to_owned(),
            ATTESTATION_KEY_SEED.to_owned(),
        );
        let config = Config::load(&env, &Overrides::default(), Demands::Maintenance)
            .expect("the maintenance configuration loads");
        let maintenance = boot::assemble_maintenance(&config)
            .await
            .expect("the maintenance workers assemble");
        let report = rebuild(&maintenance.rebuild, mode)
            .await
            .expect("the rebuild finishes");
        assert!(!report.links_only, "the ledgers verify by signature");
        report
    }

    /// The assets a rebuild report would restore, or did, across owners.
    fn restored(report: &RebuildReport) -> BTreeSet<String> {
        report
            .owners
            .iter()
            .flat_map(|owner| owner.restored.iter().map(ToString::to_string))
            .collect()
    }

    #[tokio::test]
    async fn a_hot_backup_restores_onto_a_fresh_host_and_the_index_rebuilds_from_the_ledgers() {
        if !admitted("the backup and restore drill") {
            return;
        }

        // ── 1. Host A serves a library ────────────────────────────────────────────────────
        let host_a = Host::start(None).await;
        host_a.migrate(PG_DATABASE).await;
        let server_a =
            Server::boot_durable(tempfile::tempdir().expect("a blob root"), &host_a.backends).await;
        let mut device = Device::register(&server_a, "backup-drill").await;
        let mut originals: BTreeMap<Uuid, Vec<u8>> = BTreeMap::new();
        for (name, bytes) in [
            ("first.jpg", synthetic_jpeg()),
            ("second.jpg", large_synthetic_jpeg()),
        ] {
            let id = device.import_file(name, &bytes);
            originals.insert(id, bytes);
        }
        let session_a = device.login_again(&server_a).await;
        push(&server_a, &device, &session_a).await;
        let before_dump: BTreeSet<String> = originals.keys().map(ToString::to_string).collect();

        // ── 2. A hot backup: the database first, then the blob directory ─────────────────
        host_a
            .psql_exec(&[
                "pg_dump",
                "--format=custom",
                "--username",
                PG_USER,
                "--dbname",
                PG_DATABASE,
                "--file",
                DUMP_PATH,
            ])
            .await;
        let dump: Vec<u8> = host_a
            .postgres
            .copy_file_from(DUMP_PATH, Vec::new())
            .await
            .expect("the dump copies out of the container");
        assert!(!dump.is_empty(), "pg_dump wrote a dump");

        // Written after the dump and before the blob copy: the row is in no backup of the
        // database, while its blobs and its ledger entry are in the blob directory's.
        let late_bytes = synthetic_jpeg_sized(24, 16);
        let late = device.import_file("late.jpg", &late_bytes);
        originals.insert(late, late_bytes);
        push(&server_a, &device, &session_a).await;

        let host_a_feed = device.feed(&server_a).await;
        let mut sequences: BTreeMap<Uuid, u64> = BTreeMap::new();
        for id in originals.keys() {
            let entry = entry_for(&host_a_feed, id).expect("host A publishes every asset");
            sequences.insert(*id, entry.sync_seq);
        }

        let blob_backup = tempfile::tempdir().expect("a backup directory");
        copy_tree(server_a.blob_root.path(), blob_backup.path());
        assert!(
            blob_backup.path().join("ledger").is_dir(),
            "the blob directory carries the owner ledgers"
        );

        // Host A is gone.
        drop(server_a);
        drop(host_a);

        // ── 3. Host B: the dump restored, the blob directory copied, Valkey empty ────────
        let host_b = Host::start(Some(dump)).await;
        host_b
            .psql_exec(&[
                "pg_restore",
                "--username",
                PG_USER,
                "--dbname",
                PG_DATABASE,
                "--no-owner",
                "--exit-on-error",
                DUMP_PATH,
            ])
            .await;
        let blob_root_b = tempfile::tempdir().expect("host B's blob root");
        copy_tree(blob_backup.path(), blob_root_b.path());
        assert_eq!(
            tree(blob_root_b.path()),
            tree(blob_backup.path()),
            "the blob directory restores byte for byte"
        );

        // The rebuild, with `serve` stopped: the dry run names exactly the row the dump
        // missed, and changes nothing; `--apply` restores it.
        let dry = rebuild_pass(
            blob_root_b.path(),
            &host_b.backends.database_url,
            &host_b.backends.valkey_url,
            Mode::DryRun,
        )
        .await;
        assert!(!dry.has_faults(), "{dry:#?}");
        assert_eq!(
            restored(&dry),
            BTreeSet::from([late.to_string()]),
            "{dry:#?}"
        );
        let applied = rebuild_pass(
            blob_root_b.path(),
            &host_b.backends.database_url,
            &host_b.backends.valkey_url,
            Mode::Apply,
        )
        .await;
        assert!(!applied.has_faults(), "{applied:#?}");
        assert_eq!(restored(&applied), BTreeSet::from([late.to_string()]));
        assert!(
            applied.owners.iter().all(|owner| owner.epoch.is_none()),
            "every rebuilt row kept the number the ledger proved, so no epoch moves: {applied:#?}"
        );
        let again = rebuild_pass(
            blob_root_b.path(),
            &host_b.backends.database_url,
            &host_b.backends.valkey_url,
            Mode::DryRun,
        )
        .await;
        assert!(
            restored(&again).is_empty(),
            "a second pass restores nothing"
        );
        let agreed: BTreeSet<String> = again
            .owners
            .iter()
            .flat_map(|owner| owner.agreed.iter().map(ToString::to_string))
            .collect();
        let every: BTreeSet<String> = originals.keys().map(ToString::to_string).collect();
        assert_eq!(
            agreed, every,
            "the index and the ledgers agree on every row"
        );
        assert!(before_dump.is_subset(&agreed));

        let server_b = Server::boot_durable(blob_root_b, &host_b.backends).await;
        let deep = Depth::Deep { budget: u64::MAX };
        let scrubbed = scrub(&server_b.assembled.maintenance.scrub, deep)
            .await
            .expect("the scrub finishes");
        assert!(!scrubbed.budget_exhausted, "every blob was re-hashed");
        // The rows from the dump were written by the upload path and the third by the rebuild:
        // both must agree with the manifests they publish, so any finding is a defect.
        assert!(scrubbed.findings.is_empty(), "{scrubbed:#?}");

        // Host A's session lived in Valkey, which was not restored: it is refused.
        let refused = device
            .generated(&server_b)
            .get_profile(PROTOCOL_VERSION, None)
            .await;
        assert!(
            refused.is_err(),
            "a session from before the restore is refused"
        );

        // The account itself was in the database: it signs in again, and the feed lists every
        // asset at the number host A gave it.
        let session_b = device.login_again(&server_b).await;
        let feed_b = capsule_e2e::feed_from_start(&server_b, session_b.clone()).await;
        for (id, seq) in &sequences {
            let entry = entry_for(&feed_b, id).expect("host B publishes every asset");
            assert_eq!(entry.sync_seq, *seq, "asset {id} keeps its sequence number");
        }

        // Every blob the library uploaded, served by host B over the blob route byte for byte
        // as host A stored it.
        let source = HttpBlobSource::new(session_b.clone(), server_b.v1());
        for id in originals.keys() {
            let bundle = device
                .workspace
                .upload_bundle(id)
                .expect("the asset still bundles");
            for (_, hash) in bundle_blobs(&bundle) {
                let served = match source.get_range(&hash, 0, None).await {
                    RangeOutcome::Complete { bytes } => bytes,
                    other => panic!("host B does not serve blob {hash} of {id}: {other:?}"),
                };
                let address = ContentAddress::parse(&hash).expect("a content address");
                let stored = std::fs::read(blob_path(blob_backup.path(), &address))
                    .expect("host A stored the blob");
                assert_eq!(
                    served, stored,
                    "blob {hash} of {id} is served byte for byte"
                );
            }
        }

        // `capsule restore` from host B: every original, decrypted and written back, is the
        // file that was imported on host A, byte for byte. That includes the asset only the
        // rebuild restored.
        let (_restore_home, store) = persisted(&session_b).await;
        let out = tempfile::tempdir().expect("a restore directory");
        let summary = remote::restore(
            &remote_config(&server_b),
            &store,
            &device.workspace,
            &RestoreOptions {
                scope: RestoreScope::Library,
                out: out.path().to_path_buf(),
                connection: ConnectionClass::Unmetered,
                range_window: RESTORE_RANGE_WINDOW,
            },
        )
        .await
        .expect("the restore runs");
        assert_eq!(summary.failed, 0, "{summary:#?}");
        assert_eq!(summary.restored, originals.len(), "{summary:#?}");
        for outcome in &summary.outcomes {
            assert_eq!(outcome.status, AssetRestoreStatus::Restored, "{outcome:#?}");
            let id = Uuid::parse_str(&outcome.asset_id).expect("an asset id");
            let path = outcome.path.as_ref().expect("a restored asset has a path");
            let written = std::fs::read(path).expect("the restored original reads");
            assert_eq!(
                &written,
                originals.get(&id).expect("an asset this drill imported"),
                "asset {id} restores byte for byte"
            );
        }

        // ── 4. The index-rebuild drill: no database backup at all ─────────────────────────
        host_b
            .psql_exec(&[
                "psql",
                "--username",
                PG_USER,
                "--dbname",
                PG_DATABASE,
                "--set",
                "ON_ERROR_STOP=1",
                "--command",
                &format!("CREATE DATABASE {REBUILT_DATABASE}"),
            ])
            .await;
        host_b.migrate(REBUILT_DATABASE).await;
        let rebuilt_url = host_b.database_url(REBUILT_DATABASE);
        let from_ledgers = rebuild_pass(
            server_b.blob_root.path(),
            &rebuilt_url,
            &host_b.backends.valkey_url,
            Mode::Apply,
        )
        .await;
        assert!(!from_ledgers.has_faults(), "{from_ledgers:#?}");
        assert_eq!(restored(&from_ledgers), every, "{from_ledgers:#?}");
        assert_eq!(
            index_rows(&rebuilt_url).await,
            index_rows(&host_b.backends.database_url).await,
            "an index rebuilt from the ledgers alone holds the rows the restored backup holds"
        );
    }
}
