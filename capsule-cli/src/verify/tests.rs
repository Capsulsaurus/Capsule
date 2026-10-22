use std::path::{Path, PathBuf};
use std::sync::Mutex;

use capsule_core::crypto::keys::HybridSigningKey;
use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::crypto::storage_attestation::{
    ATTESTATION_NONCE_LEN, AttestedAsset, AttestedBlob, DeclaredAsset, STORAGE_ATTESTATION_VERSION,
    StorageAttestation, StorageAttestationCore, verify_storage_attestation,
};
use capsule_library::library::{BlobVerdict, attestations_path, load_attestation_log};

use super::*;

/// A fast Argon2id cost: these tests exercise the verify, not the unlock.
const FAST_KDF: Argon2Params = Argon2Params {
    mem_kib: 64,
    t_cost: 1,
    p_cost: 1,
};

/// A directory removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "capsule-cli-verify-{name}-{}-{}",
            std::process::id(),
            Uuid::now_v7().simple()
        ));
        std::fs::create_dir_all(&path).expect("a scratch directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A library holding `count` imported assets.
struct Fixture {
    _dirs: (Scratch, Scratch),
    workspace: Workspace,
    assets: Vec<Uuid>,
}

fn fixture(name: &str, count: u8) -> Fixture {
    let lib = Scratch::new(&format!("{name}-lib"));
    let src = Scratch::new(&format!("{name}-src"));
    let mut workspace = Workspace::create_with_params(lib.path(), b"passphrase", FAST_KDF)
        .expect("a fresh library");
    let album = workspace.create_album("Trip").expect("an album");
    let mut assets: Vec<Uuid> = (0..count)
        .map(|i| {
            let path = src.path().join(format!("photo-{i}.jpg"));
            std::fs::write(
                &path,
                [b"\xFF\xD8\xFF verify fixture ".as_slice(), &[i]].concat(),
            )
            .expect("the source writes");
            workspace.import_asset(album, &path).expect("the import")
        })
        .collect();
    // The order a whole-library run reports in.
    assets.sort_unstable();
    Fixture {
        _dirs: (lib, src),
        workspace,
        assets,
    }
}

/// How the stand-in server answers.
enum Behaviour {
    /// Every declared blob is held.
    Honest,
    /// Every declared blob is held except those of this asset, which is reported lost.
    Lost(Uuid),
    /// The answer leaves this asset out.
    Omits(Uuid),
    /// The attestation is signed under a key the client never pinned.
    ForeignKey,
    /// The request is refused with this status and code.
    Refuses(u16, &'static str),
    /// Every asset is reported `durable`, but each verdict re-checks only its first `n` declared
    /// blobs — `0` is a hollow verdict with no blob list at all.
    DurableOverFirst(usize),
}

/// A stand-in for the SDK's storage-verify client. It signs real attestations, and checks them
/// the way `StorageVerifyClient::verify_signed` does — under the pinned key, for the nonce it
/// drew, against the assets asked about — so a forged one is refused here exactly as there.
struct FakeServer {
    behaviour: Behaviour,
    server_key: HybridSigningKey,
    foreign_key: HybridSigningKey,
    /// The asset count of each request, in order.
    requests: Mutex<Vec<usize>>,
}

impl FakeServer {
    fn new(behaviour: Behaviour) -> Self {
        Self {
            behaviour,
            server_key: HybridSigningKey::from_seed64(&[1; 64]),
            foreign_key: HybridSigningKey::from_seed64(&[2; 64]),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<usize> {
        self.requests.lock().unwrap().clone()
    }

    fn answer(&self, assets: &[AssetQuery]) -> Result<Vec<AttestedAsset>, VerifyError> {
        self.requests.lock().unwrap().push(assets.len());
        if let Behaviour::Refuses(status, code) = self.behaviour {
            return Err(VerifyError::Status {
                status,
                code: Some(code.to_owned()),
            });
        }
        Ok(assets
            .iter()
            .filter(|q| !matches!(self.behaviour, Behaviour::Omits(id) if id == q.asset_id))
            .map(|q| {
                let held = !matches!(self.behaviour, Behaviour::Lost(id) if id == q.asset_id);
                let answered = match self.behaviour {
                    Behaviour::DurableOverFirst(n) => n,
                    _ => q.blob_hashes.len(),
                };
                AttestedAsset {
                    asset_id: q.asset_id.to_string(),
                    durable: held,
                    blobs: q
                        .blob_hashes
                        .iter()
                        .take(answered)
                        .enumerate()
                        .map(|(i, hash)| AttestedBlob {
                            hash: *hash,
                            role: if i == 0 { "original" } else { "metadata" }.to_owned(),
                            stored: held,
                            indexed: true,
                            retrievable: held,
                            deep: None,
                        })
                        .collect(),
                }
            })
            .collect())
    }
}

fn to_verdict(asset: &AttestedAsset, checked_at: &str) -> StorageVerdict {
    StorageVerdict {
        asset_id: Uuid::parse_str(&asset.asset_id).unwrap(),
        durable: asset.durable,
        blobs: asset
            .blobs
            .iter()
            .map(|blob| BlobVerdict {
                hash: blob.hash,
                role: if blob.role == "original" {
                    BlobRole::Original
                } else {
                    BlobRole::Metadata
                },
                stored: blob.stored,
                indexed: blob.indexed,
                retrievable: blob.retrievable,
            })
            .collect(),
        checked_at: checked_at.to_owned(),
    }
}

const CHECKED_AT: &str = "2026-10-22T00:00:00Z";

impl VerdictSource for FakeServer {
    async fn verdicts(
        &self,
        assets: &[AssetQuery],
        _deep: bool,
    ) -> Result<Vec<StorageVerdict>, VerifyError> {
        Ok(self
            .answer(assets)?
            .iter()
            .map(|a| to_verdict(a, CHECKED_AT))
            .collect())
    }

    async fn attestation(
        &self,
        assets: &[AssetQuery],
        _deep: bool,
    ) -> Result<VerifiedAttestation, VerifyError> {
        let verdicts = self.answer(assets)?;
        // A fresh nonce per request, as the coordinator draws.
        let mut nonce = [0u8; ATTESTATION_NONCE_LEN];
        nonce[0] = self.requests().len() as u8;
        let core = StorageAttestationCore {
            version: STORAGE_ATTESTATION_VERSION.to_owned(),
            server_id: "capsule.example".to_owned(),
            server_key_id: Hash32([0x11; 32]),
            account: Uuid::from_u128(0xACC).to_string(),
            nonce,
            checked_at: CHECKED_AT.to_owned(),
            verdicts,
        };
        let signer = match self.behaviour {
            Behaviour::ForeignKey => &self.foreign_key,
            _ => &self.server_key,
        };
        let attestation = StorageAttestation {
            server_sig: signer.sign(&core.signing_bytes()),
            core,
        };
        let declared: Vec<DeclaredAsset> = assets
            .iter()
            .map(|q| DeclaredAsset {
                asset_id: q.asset_id.to_string(),
                blob_hashes: q.blob_hashes.clone(),
            })
            .collect();
        verify_storage_attestation(
            &attestation,
            &[self.server_key.verifying_key()],
            &nonce,
            &declared,
        )
        .map_err(VerifyError::Attestation)?;
        Ok(VerifiedAttestation {
            cbor: attestation.to_canonical_cbor(),
            verdicts: attestation
                .core
                .verdicts
                .iter()
                .map(|a| to_verdict(a, CHECKED_AT))
                .collect(),
            attestation,
        })
    }
}

fn signed() -> VerifyOptions {
    VerifyOptions {
        signed: true,
        ..VerifyOptions::default()
    }
}

fn log_of(f: &Fixture, asset: Uuid) -> Vec<Vec<u8>> {
    let capture = f.workspace.asset(&asset).map(|a| a.capture_utc);
    load_attestation_log(f.workspace.root(), &asset, capture).expect("the log reads")
}

#[tokio::test]
async fn a_signed_run_keeps_a_verified_attestation_for_every_asset() {
    let f = fixture("signed", 2);
    let server = FakeServer::new(Behaviour::Honest);

    let summary = verify_assets(&f.workspace, &server, &signed()).await;

    assert_eq!(summary.assets, 2);
    assert_eq!(summary.durable_assets, 2);
    assert_eq!(summary.attestations_stored, 2);
    assert!(summary.signed);
    assert_eq!(
        server.requests(),
        vec![2],
        "one signed request for the batch"
    );
    let first = log_of(&f, f.assets[0]);
    assert_eq!(first.len(), 1);
    assert_eq!(
        first,
        log_of(&f, f.assets[1]),
        "one attestation answers both"
    );
    let kept = StorageAttestation::from_canonical_cbor(&first[0]).unwrap();
    assert!(
        kept.verify_under(&server.server_key.verifying_key()),
        "what is kept still verifies under the server's key"
    );
    assert!(failed_run_reason(&summary).is_none());

    // Each asset declared its original and its metadata blob, in that order.
    let outcome = &summary.outcomes[0];
    assert_eq!(outcome.asset_id, f.assets[0].to_string());
    let roles: Vec<&str> = outcome.blobs.iter().map(|b| b.role).collect();
    assert_eq!(roles, vec!["original", "metadata"]);
    assert!(outcome.attestation_stored);

    // A second run asks again under a new nonce, and the log grows.
    let again = verify_assets(&f.workspace, &server, &signed()).await;
    assert_eq!(again.attestations_stored, 2);
    assert_eq!(log_of(&f, f.assets[0]).len(), 2);
}

#[tokio::test]
async fn an_attestation_that_does_not_verify_is_refused_and_never_kept() {
    let f = fixture("foreign", 2);
    let server = FakeServer::new(Behaviour::ForeignKey);

    let summary = verify_assets(&f.workspace, &server, &signed()).await;

    assert_eq!(summary.failed_assets, 2);
    assert_eq!(summary.attestations_stored, 0);
    for (outcome, asset) in summary.outcomes.iter().zip(&f.assets) {
        assert_eq!(outcome.status, AssetVerifyStatus::Failed);
        assert_eq!(outcome.error.as_ref().unwrap().kind, "attestation_rejected");
        let capture = f.workspace.asset(asset).map(|a| a.capture_utc);
        assert!(
            !attestations_path(f.workspace.root(), asset, capture).exists(),
            "nothing is written for a refused attestation"
        );
    }
    assert!(failed_run_reason(&summary).is_some());
}

#[tokio::test]
async fn an_unsigned_run_reports_verdicts_and_keeps_nothing() {
    let f = fixture("unsigned", 2);
    let lost = f.assets[1];
    let server = FakeServer::new(Behaviour::Lost(lost));

    let summary = verify_assets(&f.workspace, &server, &VerifyOptions::default()).await;

    assert_eq!(summary.durable_assets, 1);
    assert_eq!(summary.not_durable_assets, 1);
    assert_eq!(summary.attestations_stored, 0);
    assert!(log_of(&f, f.assets[0]).is_empty());
    let problems: Vec<_> = summary.problems().collect();
    assert_eq!(problems.len(), 1);
    assert_eq!(problems[0].asset_id, lost.to_string());
    assert_eq!(problems[0].status, AssetVerifyStatus::NotDurable);
    assert_eq!(
        problem_line(problems[0]),
        format!("asset {lost} is not durably held (not safely stored: original, metadata)")
    );
    assert_eq!(
        failed_run_reason(&summary).as_deref(),
        Some("1 of 2 assets are not confirmed durable on the server")
    );
}

#[tokio::test]
async fn a_signed_report_of_loss_is_kept_as_evidence() {
    // Proof of loss rests on "a signed attestation reporting H not stored": the run that finds
    // a blob lost keeps exactly that.
    let f = fixture("loss", 1);
    let asset = f.assets[0];
    let server = FakeServer::new(Behaviour::Lost(asset));

    let summary = verify_assets(&f.workspace, &server, &signed()).await;

    assert_eq!(summary.not_durable_assets, 1);
    assert_eq!(summary.attestations_stored, 1);
    let kept = StorageAttestation::from_canonical_cbor(&log_of(&f, asset)[0]).unwrap();
    assert!(kept.core.verdicts[0].blobs.iter().all(|b| !b.stored));
}

#[tokio::test]
async fn an_unknown_asset_fails_alone() {
    let f = fixture("unknown", 1);
    let stranger = Uuid::now_v7();
    let server = FakeServer::new(Behaviour::Honest);
    let options = VerifyOptions {
        assets: vec![stranger, f.assets[0]],
        ..signed()
    };

    let summary = verify_assets(&f.workspace, &server, &options).await;

    assert_eq!(summary.assets, 2);
    assert_eq!(summary.outcomes[0].asset_id, stranger.to_string());
    assert_eq!(
        summary.outcomes[0].error.as_ref().unwrap().kind,
        "unknown_asset"
    );
    assert_eq!(summary.outcomes[1].status, AssetVerifyStatus::Durable);
    assert_eq!(
        server.requests(),
        vec![1],
        "only the known asset is asked about"
    );
}

#[tokio::test]
async fn an_answer_that_omits_an_asset_fails_that_asset() {
    let f = fixture("omits", 2);
    let server = FakeServer::new(Behaviour::Omits(f.assets[0]));

    let summary = verify_assets(&f.workspace, &server, &VerifyOptions::default()).await;

    assert_eq!(
        summary.outcomes[0].error.as_ref().unwrap().kind,
        "missing_verdict"
    );
    assert_eq!(summary.outcomes[1].status, AssetVerifyStatus::Durable);
}

/// A verdict that says `durable` but does not re-check every declared blob — no blob list at
/// all, or only the original — has not confirmed the asset, so it is not reported durable and
/// the run exits non-zero. The aggregate alone is never trusted.
#[tokio::test]
async fn a_durable_verdict_that_skips_a_declared_blob_is_not_durable() {
    for answered in [0, 1] {
        let f = fixture(&format!("hollow-{answered}"), 1);
        let server = FakeServer::new(Behaviour::DurableOverFirst(answered));

        let summary = verify_assets(&f.workspace, &server, &VerifyOptions::default()).await;

        let outcome = &summary.outcomes[0];
        assert_eq!(
            outcome.status,
            AssetVerifyStatus::NotDurable,
            "{answered} of 2 declared blobs re-checked: {outcome:?}"
        );
        assert_eq!(outcome.blobs.len(), answered);
        assert_eq!(summary.durable_assets, 0);
        assert!(failed_run_reason(&summary).is_some());
        assert_eq!(
            problem_line(outcome),
            format!(
                "asset {} is not durably held (not safely stored: the server's answer does not \
                 confirm every declared blob)",
                f.assets[0]
            )
        );
    }
}

#[tokio::test]
async fn a_refused_request_fails_its_batch_with_the_servers_code() {
    let f = fixture("throttled", 1);
    let server = FakeServer::new(Behaviour::Refuses(429, "error.storage.deep_rate_limited"));

    let summary = verify_assets(&f.workspace, &server, &signed()).await;

    let error = summary.outcomes[0].error.as_ref().unwrap();
    assert_eq!(error.kind, "verify_unavailable");
    assert_eq!(
        error.code.as_deref(),
        Some("error.storage.deep_rate_limited")
    );
    assert!(log_of(&f, f.assets[0]).is_empty());
}

#[tokio::test]
async fn assets_are_asked_about_in_batches_each_keeping_its_own_attestation() {
    let f = fixture("batches", 3);
    let server = FakeServer::new(Behaviour::Honest);

    let summary = verify_assets_in_batches(&f.workspace, &server, &signed(), 2).await;

    assert_eq!(server.requests(), vec![2, 1]);
    assert_eq!(summary.durable_assets, 3);
    assert_eq!(log_of(&f, f.assets[0]), log_of(&f, f.assets[1]));
    assert_ne!(
        log_of(&f, f.assets[0]),
        log_of(&f, f.assets[2]),
        "the second batch has its own attestation"
    );
}

#[test]
fn the_summary_line_counts_what_was_kept_on_a_signed_run() {
    let summary = VerifySummary {
        assets: 3,
        durable_assets: 2,
        not_durable_assets: 1,
        signed: true,
        attestations_stored: 3,
        ..VerifySummary::default()
    };
    assert_eq!(
        summary_line(&summary),
        "2 of 3 assets durably held by the server; 1 not durable, 0 failed. \
         3 signed attestations stored in the library."
    );
}

#[test]
fn the_json_report_names_each_status_in_snake_case() {
    let outcome = AssetVerifyOutcome::failed(
        Uuid::from_u128(1),
        AssetVerifyFailure::new("unknown_asset", "the library holds no such asset"),
    );
    let json = serde_json::to_value(&outcome).unwrap();
    assert_eq!(json["status"], "failed");
    assert_eq!(json["error"]["kind"], "unknown_asset");
    assert_eq!(
        serde_json::to_value(AssetVerifyStatus::NotDurable).unwrap(),
        "not_durable"
    );
}
