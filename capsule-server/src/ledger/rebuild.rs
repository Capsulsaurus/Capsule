//! `capsule-server rebuild`: reconstruct the asset index from the owner ledgers and the blobs.
//!
//! [Server Filesystem § Rebuild] is the contract. Per owner:
//!
//! 1. Verify the ledger chain ([`LedgerKeys::verify`]) against the head the index recorded
//!    ([`AssetIndex::ledger_head`]), so a tail cut at an entry boundary is a break too. A break
//!    stops that owner's replay at the last good entry and is reported; other owners continue.
//!    The owners walked are those with a ledger and those the index anchored, so an owner whose
//!    ledger lost every entry is reported too. Under `--apply` the recorded head is then raised
//!    to the verified one, or, after a cut (or a rewrite the operator accepted with
//!    [`RebuildContext::accepting_rewrites`]), moved down to it ([`super`]'s "Where the anchor
//!    lives").
//! 2. Replay the verified entries in ledger order: each custody entry the index did not refuse
//!    (an upload or a drop adoption) and each lifecycle op (a delete, a restore, a metadata
//!    update, a derivative, a retention purge). Read each named blob's presence, decode each
//!    manifest through the one manifest parser ([`super::manifest`]), and assemble the row the
//!    entries imply, the way the index's own writes would have.
//! 3. Compare each row with the index. A row the index lacks is restored with the `sync_seq`
//!    the ledger proved ([`AssetIndex::restore`]); a row it holds is compared and never
//!    overwritten. Quota attribution is recomputed for every restored blob; reference counts are
//!    a query over rows, so restoring the rows is recomputing them.
//! 4. Report blobs nothing names, entries naming a missing blob, and provenance blobs that fail
//!    structural validation (moved to `quarantine/` with their reason, never dropped).
//!
//! When the ledger cannot prove an owner's sequence — the chain breaks, a custody entry was
//! never resolved because the process stopped between it and the index write, or a lifecycle
//! op that still chains has no entry recording its position — the rows it
//! cannot number are given numbers above every number the owner has used, and the owner moves to
//! a new sync epoch ([`AssetIndex::bump_sync_epoch`]), so a client's cursor from before the
//! rebuild is refused rather than resumed against a numbering it never saw.
//!
//! Those numbers come from the owner's allocator inside the restore
//! ([`AssetIndex::restore`]), after every row the ledger did number has raised it, so a server
//! still accepting writes cannot mint the same number for another asset. A row the ledger did
//! number keeps that number; restoring it into an index a running server has kept writing to
//! since the backup was taken can meet a number the server has already re-issued, which is why
//! the self-hosting guide restores with the server stopped.
//!
//! # Dry run by default
//!
//! [`Mode::DryRun`] reads everything and writes nothing: no row, no quota charge, no epoch, no
//! quarantine move. The report is the same shape either way, so an operator reads what a rebuild
//! would do before doing it.
//!
//! # Idempotent
//!
//! Every write is conditional on the index not holding the row already, and the quota charge is
//! a no-op for an attributed address, so a second `--apply` over the same ledger restores
//! nothing. The epoch is the one exception worth naming: it bumps again whenever the ledger
//! still cannot prove the sequence, which is harmless (a cursor from the previous epoch is
//! refused, and a client resyncs once more) and is reported.
//!
//! # What a rebuild trusts
//!
//! The ledger's own signatures when the host holds the attestation key's public half, its links
//! otherwise (the report says which). It does not re-verify a manifest's device signature
//! against the directory version the ledger recorded (`LedgerEvent::Directory`): the server
//! verified the write when it accepted it, the entry that says so is signed, and checking it
//! again needs the published directory documents, which live in the database the rebuild may
//! not have.
//!
//! An op with no recorded position is re-checked against invariant 17 only (it must chain onto
//! the head the replay has reached). Invariant 18 is album-wide and is not re-run.
//!
//! [Server Filesystem § Rebuild]: https://github.com/Capsulsaurus/Capsule/blob/master/capsule-docs/src/content/docs/design/filesystem/server.md#rebuild

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::provenance::ManifestCore;
use capsule_i18n::error_codes;
use jiff::Timestamp;

use super::manifest::{self, ManifestFault};
use super::replay::{self, Custody, LedgerKeys, Resolution};
use super::{ChainFault, LedgerError, OwnerLedger};
use crate::blob::{BlobError, BlobStore, ContentAddress, QuarantineReason};
use crate::gc::Mode;
use crate::index::{AssetIndex, AssetRow, AssetState, BlobRef, OpAction, RestoreOutcome};
use crate::quota::{QuotaLimits, QuotaStore};
use crate::store::{AlbumId, AssetId, BlobRole, Clock, OwnerId, StoreError, UserId};

/// Why a rebuild could not finish.
///
/// Distinct from what it *found*: a broken chain or a missing blob is a report line, and the
/// pass continues. These are the failures that leave the pass unable to compare at all.
#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    /// The ledger could not be read.
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    /// The blob store could not be read or written.
    #[error(transparent)]
    Blobs(#[from] BlobError),
    /// The index or the quota store could not answer.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The rebuild's collaborators.
#[derive(Debug, Clone)]
pub struct RebuildContext {
    index: Arc<dyn AssetIndex>,
    blobs: Arc<dyn BlobStore>,
    ledger: Arc<dyn OwnerLedger>,
    quotas: Arc<dyn QuotaStore>,
    keys: LedgerKeys,
    clock: Arc<dyn Clock>,
    /// Whether `--apply` also re-anchors an owner whose ledger the anchor caught rewritten.
    accept_rewrites: bool,
}

impl RebuildContext {
    /// Assembles the rebuild over the stores it reads and writes.
    pub fn new(
        index: Arc<dyn AssetIndex>,
        blobs: Arc<dyn BlobStore>,
        ledger: Arc<dyn OwnerLedger>,
        quotas: Arc<dyn QuotaStore>,
        keys: LedgerKeys,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            index,
            blobs,
            ledger,
            quotas,
            keys,
            clock,
            accept_rewrites: false,
        }
    }

    /// Under [`Mode::Apply`], also re-anchor an owner whose ledger holds another entry at its
    /// anchored position ([`ChainFault::AnchorMismatch`]), accepting the ledger as it stands.
    ///
    /// The operator's way out of a caught rewrite (`rebuild --apply --accept-rewrites`). Never
    /// the default: the mismatch is the only evidence of the rewrite, so it is kept until an
    /// operator who has read the report says otherwise. The pass still reports the mismatch,
    /// stops that owner's replay before it and moves the owner's epoch; only the anchor moves,
    /// which lets the server append again and the next pass verify the ledger as it now is.
    #[must_use]
    pub fn accepting_rewrites(mut self) -> Self {
        self.accept_rewrites = true;
        self
    }
}

/// A row the index holds that disagrees with the one the ledger implies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowDifference {
    /// The asset.
    pub asset: AssetId,
    /// The facts that disagree, by name.
    pub facts: Vec<&'static str>,
}

/// What a rebuild found, and did, for one owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerReport {
    /// The owner.
    pub owner: OwnerId,
    /// How many ledger entries, from the first, verified.
    pub verified: usize,
    /// Why the chain stopped verifying before its end, if it did.
    pub chain_fault: Option<ChainFault>,
    /// Rows the index lacked: restored under `--apply`, and what would be in a dry run.
    pub restored: Vec<AssetId>,
    /// Rows the index holds that agree with the ledger.
    pub agreed: Vec<AssetId>,
    /// Rows the index holds that disagree with the ledger. Never overwritten.
    pub differs: Vec<RowDifference>,
    /// Rows whose `sync_seq` the ledger cannot prove, numbered above the owner's head.
    pub unprovable: Vec<AssetId>,
    /// Bundles the ledger records without a decodable manifest and metadata blob: nothing a
    /// client could see, so nothing to restore.
    pub incomplete: Vec<AssetId>,
    /// Tombstones a retention purge emptied whose manifests the collector has since swept:
    /// nothing in the blob root names their album any more, so only the database backup holds
    /// their rows. Not a fault: it is what a completed purge leaves.
    pub purged: Vec<AssetId>,
    /// Entries naming a blob the store does not hold.
    pub dangling: Vec<(AssetId, ContentAddress)>,
    /// Provenance blobs that are not a usable manifest, and why. Under `--apply` a malformed one
    /// is moved to `quarantine/` with its reason unless a row still references it; a record a
    /// newer client wrote ([`ManifestFault::NewerVersion`]) is never moved.
    pub unusable_manifests: Vec<(AssetId, ContentAddress, ManifestFault)>,
    /// Provenance blobs whose manifest names another asset than the entry does.
    pub foreign_manifests: Vec<(AssetId, ContentAddress)>,
    /// Whether the owner must move to a new sync epoch: the sequence is not provable.
    pub epoch_bump_needed: bool,
    /// The epoch the owner moved to, under `--apply`.
    pub epoch: Option<u64>,
    /// The position the owner's ledger was re-anchored at after a reported cut or an accepted
    /// rewrite, under `--apply`: `Some(0)` when nothing of the ledger is left. `None` when the
    /// anchor was only raised, or left alone.
    pub reanchored: Option<u64>,
}

impl OwnerReport {
    /// Whether anything here needs an operator: a fault, as opposed to a row to restore.
    pub fn has_faults(&self) -> bool {
        self.chain_fault.is_some()
            || !self.differs.is_empty()
            || !self.dangling.is_empty()
            || !self.unusable_manifests.is_empty()
            || !self.foreign_manifests.is_empty()
    }
}

/// What one rebuild pass found and did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebuildReport {
    /// Whether anything was written.
    pub applied: bool,
    /// Whether the ledgers were verified by links only (no attestation key on this host).
    pub links_only: bool,
    /// One report per owner with a ledger, in owner order.
    pub owners: Vec<OwnerReport>,
    /// Blobs no verified ledger entry names and no index row references: left for GC.
    pub orphans: Vec<ContentAddress>,
}

impl RebuildReport {
    /// Whether any owner's report needs an operator.
    pub fn has_faults(&self) -> bool {
        self.owners.iter().any(OwnerReport::has_faults)
    }
}

/// The lifecycle action a retention purge is ledgered under (#767).
const PURGE: &str = "purge";

/// Run one rebuild pass.
///
/// # Errors
///
/// [`RebuildError`] when a store cannot be read or written. What the pass found is never an
/// error; see [`RebuildReport`].
#[tracing::instrument(skip(context), fields(mode = ?mode))]
pub async fn rebuild(context: &RebuildContext, mode: Mode) -> Result<RebuildReport, RebuildError> {
    let mut report = RebuildReport {
        applied: mode == Mode::Apply,
        links_only: !context.keys.checks_signatures(),
        ..RebuildReport::default()
    };
    let mut named: BTreeSet<ContentAddress> = BTreeSet::new();

    let owners = super::owners_to_verify(
        context.ledger.owners().await?,
        context.index.anchored_owners().await?,
    );
    for owner in owners {
        let owner_report = rebuild_owner(context, mode, &owner, &mut named).await?;
        tracing::info!(
            %owner,
            verified = owner_report.verified,
            restored = owner_report.restored.len(),
            agreed = owner_report.agreed.len(),
            differs = owner_report.differs.len(),
            unprovable = owner_report.unprovable.len(),
            dangling = owner_report.dangling.len(),
            unusable_manifests = owner_report.unusable_manifests.len(),
            epoch = ?owner_report.epoch,
            "rebuilt an owner's rows from its ledger"
        );
        report.owners.push(owner_report);
    }

    report.orphans = orphans(context, &named).await?;
    tracing::info!(
        applied = report.applied,
        owners = report.owners.len(),
        orphans = report.orphans.len(),
        faults = report.has_faults(),
        "an index rebuild finished"
    );
    Ok(report)
}

/// One asset's facts, accumulated over its ledger entries in ledger order.
#[derive(Debug, Default)]
struct Accumulated {
    blobs: BTreeMap<(BlobRole, String), BlobRef>,
    /// The account each address's storage is attributed to, from its latest custody entry.
    uploaders: BTreeMap<ContentAddress, UserId>,
    superseded: Vec<ContentAddress>,
    /// The manifest the asset was created under: its album, protocol pin and suite are the
    /// row's for life.
    first: Option<ManifestCore>,
    /// The chain head: the provenance blob's address, the chain-head hash, and its manifest.
    head: Option<(ContentAddress, Option<Hash32>, ManifestCore)>,
    /// A lifecycle op's state change, when one applied.
    state: Option<AssetState>,
    /// A lifecycle op's retention floor, when one applied.
    retention_until: Option<Option<Timestamp>>,
    /// Addresses the replay named that the store does not hold. Reported when the replay ends,
    /// unless a purge retired the reference first.
    dangling: Vec<ContentAddress>,
    /// Whether a retention purge ended the asset's references.
    purged: bool,
    proven: Vec<u64>,
    unresolved: bool,
    created_at: Option<Timestamp>,
    updated_at: Option<Timestamp>,
}

/// One verified ledger entry a rebuild replays, in ledger order.
enum Replayed {
    Custody(Custody),
    Lifecycle(replay::Lifecycle),
}

impl Replayed {
    fn ledger_seq(&self) -> u64 {
        match self {
            Self::Custody(record) => record.ledger_seq,
            Self::Lifecycle(op) => op.ledger_seq,
        }
    }

    fn asset(&self) -> AssetId {
        match self {
            Self::Custody(record) => AssetId::new(record.asset_id.as_str()),
            Self::Lifecycle(op) => AssetId::new(op.asset_id.as_str()),
        }
    }
}

/// Steps 1–3 for one owner.
async fn rebuild_owner(
    context: &RebuildContext,
    mode: Mode,
    owner: &OwnerId,
    named: &mut BTreeSet<ContentAddress>,
) -> Result<OwnerReport, RebuildError> {
    let scan = context.ledger.scan(owner).await?;
    let anchor = context.index.ledger_head(owner).await?;
    let chain = context.keys.verify(owner, &scan, anchor);
    // A reported cut is re-anchored under `--apply`; a caught rewrite only when the operator
    // accepted it.
    let lower = match chain.fault {
        Some(ChainFault::BehindAnchor { .. }) => true,
        Some(ChainFault::AnchorMismatch { .. }) => context.accept_rewrites,
        _ => false,
    };
    let verified_head = chain.head;
    let mut report = OwnerReport {
        owner: owner.clone(),
        verified: chain.verified,
        epoch_bump_needed: chain.fault.is_some(),
        chain_fault: chain.fault,
        restored: Vec::new(),
        agreed: Vec::new(),
        differs: Vec::new(),
        unprovable: Vec::new(),
        incomplete: Vec::new(),
        purged: Vec::new(),
        dangling: Vec::new(),
        unusable_manifests: Vec::new(),
        foreign_manifests: Vec::new(),
        epoch: None,
        reanchored: None,
    };

    // Ledger order, uploads and lifecycle ops interleaved as the server accepted them, and
    // assets in the order they first appear, so unprovable rows are numbered in the order the
    // server took custody of them.
    let verified = &scan.entries[..chain.verified];
    let mut replayed: Vec<Replayed> = replay::custody(verified)
        .into_iter()
        .filter(|record| record.resolution != Resolution::Refused)
        .map(Replayed::Custody)
        .chain(
            replay::lifecycle(verified)
                .into_iter()
                .map(Replayed::Lifecycle),
        )
        .collect();
    replayed.sort_by_key(Replayed::ledger_seq);

    let mut order: Vec<AssetId> = Vec::new();
    let mut assets: BTreeMap<AssetId, Accumulated> = BTreeMap::new();
    for entry in replayed {
        let asset = entry.asset();
        if !assets.contains_key(&asset) {
            order.push(asset.clone());
        }
        let facts = assets.entry(asset.clone()).or_default();
        match entry {
            Replayed::Custody(record) => {
                accumulate(context, mode, &asset, &record, facts, &mut report, named).await?;
            }
            Replayed::Lifecycle(op) => {
                apply_lifecycle(context, mode, &asset, &op, facts, &mut report, named).await?;
            }
        }
    }

    // Rows the ledger numbered are restored first, each raising the owner's allocator past its
    // number; rows it could not number go last and are numbered by that allocator inside the
    // restore, so they land above every proven number and above anything a live server mints
    // meanwhile.
    let mut unnumbered: Vec<(AssetRow, Accumulated)> = Vec::new();
    for asset in order {
        let Some(facts) = assets.remove(&asset) else {
            continue;
        };
        report.dangling.extend(
            facts
                .dangling
                .iter()
                .map(|address| (asset.clone(), address.clone())),
        );
        let Some(row) = assemble(owner, &asset, &facts) else {
            if facts.purged {
                report.purged.push(asset);
            } else {
                report.incomplete.push(asset);
            }
            continue;
        };
        if facts.unresolved || row.sync_seq.is_none() {
            // The ledger cannot prove where this write sits in the owner's sequence, so the
            // owner moves epoch and no reader resumes against the old numbering.
            report.unprovable.push(asset.clone());
            report.epoch_bump_needed = true;
        }
        if row.sync_seq.is_none() {
            unnumbered.push((row, facts));
            continue;
        }
        compare_or_restore(context, mode, row, &facts, &mut report).await?;
    }
    for (row, facts) in unnumbered {
        compare_or_restore(context, mode, row, &facts, &mut report).await?;
    }

    if mode == Mode::Apply && report.epoch_bump_needed {
        report.epoch = Some(context.index.bump_sync_epoch(owner).await?);
    }
    if mode == Mode::Apply {
        reanchor(context, owner, lower, verified_head).await?;
        if lower {
            report.reanchored = Some(verified_head.map_or(0, |head| head.seq));
        }
    }
    Ok(report)
}

/// Bring `owner`'s recorded ledger head in line with what this pass verified.
///
/// With `lower` — a cut the pass reported (the ledger ends below its anchor), or a rewrite the
/// operator accepted ([`RebuildContext::accepting_rewrites`]), each acknowledged by the epoch
/// bump just made — the anchor is moved **down** to the head that verified (cleared when none
/// did): until then the server refuses to extend that owner's ledger
/// ([`super::AnchoredLedger`]), and every pass would report the same fault. Otherwise it is only
/// **raised**, so a rebuild into an empty schema anchors the chain it verified, and a rewrite
/// nobody accepted keeps its evidence.
async fn reanchor(
    context: &RebuildContext,
    owner: &OwnerId,
    lower: bool,
    verified: Option<super::LedgerHead>,
) -> Result<(), RebuildError> {
    if lower {
        context.index.reset_ledger_head(owner, verified).await?;
    } else if let Some(head) = verified {
        context.index.record_ledger_head(owner, head).await?;
    }
    Ok(())
}

/// Fold one custody entry into its asset's facts.
async fn accumulate(
    context: &RebuildContext,
    mode: Mode,
    asset: &AssetId,
    record: &Custody,
    facts: &mut Accumulated,
    report: &mut OwnerReport,
    named: &mut BTreeSet<ContentAddress>,
) -> Result<(), RebuildError> {
    match record.resolution {
        Resolution::Indexed {
            sync_seq: Some(seq),
        } => facts.proven.push(seq),
        Resolution::Indexed { sync_seq: None } | Resolution::Refused => {}
        Resolution::Unresolved => facts.unresolved = true,
    }
    let at = record.received_at.parse::<Timestamp>().ok();
    facts.created_at = facts.created_at.or(at);
    facts.updated_at = at.or(facts.updated_at);

    let Ok(address) = ContentAddress::parse(&record.content_hash.to_hex()) else {
        return Ok(());
    };
    named.insert(address.clone());
    facts
        .uploaders
        .insert(address.clone(), UserId::new(record.upload_user_id.as_str()));
    let Some(role) = BlobRole::from_token(&record.blob_role) else {
        tracing::warn!(%asset, role = %record.blob_role, "a ledger entry names a role this build does not know");
        return Ok(());
    };
    if context.blobs.stat(&address).await?.is_none() {
        facts.dangling.push(address);
        return Ok(());
    }

    if role == BlobRole::Provenance {
        match manifest::read(context.blobs.as_ref(), &address).await? {
            None => {
                facts.dangling.push(address);
                return Ok(());
            }
            Some(Err(fault)) => {
                quarantine(context, mode, asset, &address, fault, report).await?;
                return Ok(());
            }
            Some(Ok(parsed)) => {
                if let Err(fault) = manifest::check_structure(&parsed) {
                    quarantine(context, mode, asset, &address, fault, report).await?;
                    return Ok(());
                }
                if parsed.core.file_id.to_string() != asset.as_str() {
                    report.foreign_manifests.push((asset.clone(), address));
                    return Ok(());
                }
                supersede(facts, &address);
                facts.first.get_or_insert_with(|| parsed.core.clone());
                facts.head = Some((address.clone(), record.manifest_sha256, parsed.core));
            }
        }
    }

    // A singular role holds one address: a later custody entry for it is a replace, which
    // re-points it. A plural role accumulates.
    if crate::index::is_singular(role) {
        facts.blobs.retain(|(held, _), _| *held != role);
    }
    facts.blobs.insert(
        (role, address.as_str().to_owned()),
        BlobRef {
            role,
            address,
            size: record.size,
        },
    );
    Ok(())
}

/// Move the current head into the superseded chain when `address` replaces it.
///
/// The rule [`crate::index`] applies on a re-point: a superseded manifest stays referenced, so
/// the chain the scrub walks and the takedown rebuttal rests on survive a rebuild.
fn supersede(facts: &mut Accumulated, address: &ContentAddress) {
    if let Some((previous, _, _)) = &facts.head
        && previous != address
        && !facts.superseded.contains(previous)
    {
        facts.superseded.push(previous.clone());
    }
}

/// Point a singular `role` at `address`, as a lifecycle op does: with no size, because the op
/// path never learns one ([`crate::index`]'s `set_singular`).
fn repoint(facts: &mut Accumulated, role: BlobRole, address: &ContentAddress) {
    facts.blobs.retain(|(held, _), _| *held != role);
    facts.blobs.insert(
        (role, address.as_str().to_owned()),
        BlobRef {
            role,
            address: address.clone(),
            size: 0,
        },
    );
}

/// Fold one lifecycle op into its asset's facts.
///
/// An op whose position was recorded is applied as the index applied it. An op with no recorded
/// position was refused by the index, or the process stopped before the second entry: the
/// replay re-runs invariant 17 (the manifest must chain onto the head the replay has reached),
/// applies it when it passes, and cannot prove its number. Invariant 18 is album-wide and is not
/// re-run; an op it refused and that still chains is replayed, and the epoch moves for it.
async fn apply_lifecycle(
    context: &RebuildContext,
    mode: Mode,
    asset: &AssetId,
    op: &replay::Lifecycle,
    facts: &mut Accumulated,
    report: &mut OwnerReport,
    named: &mut BTreeSet<ContentAddress>,
) -> Result<(), RebuildError> {
    // A retention purge (#767) carries no manifest: the server initiates it under the signed
    // `retention_until`. It drops the tombstone's references and keeps the row, as the index's
    // purge does, and only for a tombstone — the index refuses any other.
    if op.action == PURGE {
        if facts.state == Some(AssetState::Tombstoned) || facts.head.is_none() {
            facts.blobs.clear();
            facts.superseded.clear();
            facts.dangling.clear();
            facts.purged = true;
        }
        return Ok(());
    }
    let Some(manifest_hash) = op.manifest_hash else {
        return Ok(());
    };
    let Ok(address) = ContentAddress::parse(&manifest_hash.to_hex()) else {
        return Ok(());
    };
    named.insert(address.clone());
    // An op on an asset whose create the replay never reached is one the index answered
    // `NotFound`: there is no row for it to change.
    let Some((head_address, head_hash, _)) = &facts.head else {
        return Ok(());
    };
    // The same op twice — a client retry the index answered as a replay — applies once.
    if *head_address == address {
        if let Some(seq) = op.sync_seq {
            facts.proven.push(seq);
        }
        return Ok(());
    }
    let head_hash = *head_hash;
    let parsed = match manifest::read(context.blobs.as_ref(), &address).await? {
        None => {
            facts.dangling.push(address);
            return Ok(());
        }
        Some(Err(fault)) => {
            quarantine(context, mode, asset, &address, fault, report).await?;
            return Ok(());
        }
        Some(Ok(parsed)) => parsed,
    };
    if let Err(fault) = manifest::check_structure(&parsed) {
        quarantine(context, mode, asset, &address, fault, report).await?;
        return Ok(());
    }
    if parsed.core.file_id.to_string() != asset.as_str() {
        report.foreign_manifests.push((asset.clone(), address));
        return Ok(());
    }
    if let Some(seq) = op.sync_seq {
        facts.proven.push(seq);
    } else {
        if parsed.core.prior_provenance_hash != head_hash {
            tracing::debug!(%asset, action = %op.action, "an unrecorded op does not chain; the index refused it");
            return Ok(());
        }
        facts.unresolved = true;
    }

    supersede(facts, &address);
    repoint(facts, BlobRole::Provenance, &address);
    if let Ok(at) = op.recorded_at.parse::<Timestamp>() {
        facts.updated_at = Some(at);
    }
    if op.action == OpAction::MetadataUpdate.as_str()
        && let Some(metadata) = parsed.core.metadata_blob_hash
        && let Ok(metadata) = ContentAddress::parse(&metadata.to_hex())
    {
        named.insert(metadata.clone());
        if context.blobs.stat(&metadata).await?.is_none() {
            facts.dangling.push(metadata.clone());
        }
        repoint(facts, BlobRole::Metadata, &metadata);
    }
    if op.action == OpAction::Delete.as_str() {
        facts.state = Some(AssetState::Tombstoned);
        facts.retention_until = Some(
            parsed
                .core
                .retention_until
                .as_deref()
                .and_then(|at| at.parse::<Timestamp>().ok()),
        );
    } else if op.action == OpAction::TrashRestore.as_str() {
        facts.state = Some(AssetState::Visible);
        facts.retention_until = Some(None);
    }
    facts.head = Some((address, Some(manifest_hash), parsed.core));
    Ok(())
}

/// Record a provenance blob that is not a usable manifest, and under `--apply` move it to
/// `quarantine/` — unless a row still references it, which is the scrub's to report rather than
/// the rebuild's to break, or a newer client wrote it, which is not damage at all.
async fn quarantine(
    context: &RebuildContext,
    mode: Mode,
    asset: &AssetId,
    address: &ContentAddress,
    fault: ManifestFault,
    report: &mut OwnerReport,
) -> Result<(), RebuildError> {
    report
        .unusable_manifests
        .push((asset.clone(), address.clone(), fault));
    if mode == Mode::DryRun || matches!(fault, ManifestFault::NewerVersion { .. }) {
        return Ok(());
    }
    if context.index.reference_count(address).await? > 0 {
        tracing::warn!(%asset, %address, %fault, "a malformed manifest is still referenced; left in place");
        return Ok(());
    }
    let moved = context
        .blobs
        .quarantine(
            address,
            QuarantineReason {
                code: error_codes::UPLOAD_ENVELOPE_REJECTED.to_owned(),
                detail: format!("index rebuild: {fault}"),
                at: context.clock.now(),
            },
        )
        .await?;
    tracing::warn!(%asset, %address, %fault, moved, "quarantined a manifest the rebuild could not use");
    Ok(())
}

/// The row an asset's accumulated facts imply, or `None` when its index tier never landed and
/// it is not a tombstone.
fn assemble(owner: &OwnerId, asset: &AssetId, facts: &Accumulated) -> Option<AssetRow> {
    let (_, chain_head, core) = facts.head.as_ref()?;
    let first = facts.first.as_ref().unwrap_or(core);
    let blobs: Vec<BlobRef> = facts.blobs.values().cloned().collect();
    let row = AssetRow {
        asset_id: asset.clone(),
        owner_id: owner.clone(),
        album_id: AlbumId::new(first.album_id.to_string()),
        protocol_version: first.protocol_version.clone(),
        crypto_suite_id: first.crypto_suite_id,
        state: facts.state.unwrap_or(AssetState::Visible),
        blobs,
        first_seq: facts.proven.iter().copied().min(),
        sync_seq: facts.proven.iter().copied().max(),
        chain_head: *chain_head,
        amk_version: u64::from(core.amk_version.0),
        superseded: facts.superseded.clone(),
        hold: None,
        retention_until: facts.retention_until.flatten(),
        created_at: facts.created_at.unwrap_or(Timestamp::UNIX_EPOCH),
        updated_at: facts.updated_at.unwrap_or(Timestamp::UNIX_EPOCH),
    };
    // A purged tombstone holds no blobs and is still a row: the deletion has to reach a client
    // that has not synced since.
    (row.is_publishable() || row.state == AssetState::Tombstoned).then_some(row)
}

/// Step 3 for one row: compare it with the index, or restore it.
async fn compare_or_restore(
    context: &RebuildContext,
    mode: Mode,
    row: AssetRow,
    facts: &Accumulated,
    report: &mut OwnerReport,
) -> Result<(), RebuildError> {
    let asset = row.asset_id.clone();
    let existing = match context.index.read(&asset).await? {
        Some(existing) => Some(existing),
        None if mode == Mode::DryRun => None,
        None => match context.index.restore(row.clone()).await? {
            RestoreOutcome::Restored => {
                charge(context, &row, facts).await?;
                None
            }
            // A finalization landed between the read and the restore.
            RestoreOutcome::Present(existing) => Some(*existing),
        },
    };
    match existing {
        None => report.restored.push(asset),
        Some(existing) => {
            let facts = differences(&existing, &row);
            if facts.is_empty() {
                report.agreed.push(asset);
            } else {
                report.differs.push(RowDifference { asset, facts });
            }
        }
    }
    Ok(())
}

/// Re-attribute a restored row's blobs to the accounts the ledger names.
///
/// Quota is charged by content address, once: an address already attributed is a no-op, so a
/// second rebuild, or a quota ledger restored from the same backup, is not charged twice.
async fn charge(
    context: &RebuildContext,
    row: &AssetRow,
    facts: &Accumulated,
) -> Result<(), RebuildError> {
    let now = context.clock.now();
    for blob in &row.blobs {
        let Some(user) = facts.uploaders.get(&blob.address) else {
            continue;
        };
        context
            .quotas
            .charge(
                user,
                &blob.address,
                blob.size,
                now,
                QuotaLimits::unlimited(),
            )
            .await?;
    }
    Ok(())
}

/// The facts on which `held` and `rebuilt` disagree, by name.
///
/// Only facts a rebuild can reconstruct: a serving hold, a retention floor set by an unledgered
/// delete, and the superseded chain of unledgered lifecycle writes are the backup's alone, and
/// comparing them would report every such row as a disagreement.
pub fn differences(held: &AssetRow, rebuilt: &AssetRow) -> Vec<&'static str> {
    let mut facts = Vec::new();
    if held.owner_id != rebuilt.owner_id {
        facts.push("owner_id");
    }
    if held.album_id != rebuilt.album_id {
        facts.push("album_id");
    }
    if held.protocol_version != rebuilt.protocol_version {
        facts.push("protocol_version");
    }
    if held.crypto_suite_id != rebuilt.crypto_suite_id {
        facts.push("crypto_suite_id");
    }
    // A rebuilt row with no number is one the ledger could not number, which says nothing about
    // the held row's; a numbered rebuilt row says the asset was published.
    if held.sync_seq.is_none() && rebuilt.sync_seq.is_some() {
        facts.push("sync_seq");
    }
    let addresses = |row: &AssetRow| -> BTreeSet<(BlobRole, String)> {
        row.blobs
            .iter()
            .map(|blob| (blob.role, blob.address.as_str().to_owned()))
            .collect()
    };
    if !addresses(rebuilt).is_subset(&addresses(held)) {
        facts.push("blobs");
    }
    facts
}

/// Blobs no verified ledger entry names and no row references.
async fn orphans(
    context: &RebuildContext,
    named: &BTreeSet<ContentAddress>,
) -> Result<Vec<ContentAddress>, RebuildError> {
    let mut found = Vec::new();
    let mut after = None;
    loop {
        let page = context.blobs.enumerate(after.as_ref(), 256).await?;
        for entry in &page.entries {
            if !named.contains(&entry.address)
                && context.index.reference_count(&entry.address).await? == 0
            {
                found.push(entry.address.clone());
            }
        }
        match page.next {
            Some(next) => after = Some(next),
            None => return Ok(found),
        }
    }
}

#[cfg(test)]
mod tests;
