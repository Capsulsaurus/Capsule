//! Invariant 20: everything a peer sends is re-validated before anything of it is written.
//!
//! design/threat-model/validation.md: on federation pull, invariants 1–18 and 25 are re-applied,
//! each as strict as the compacted feed lets a receiver check it. This module is that
//! re-application, as two pure functions over values — [`screen_page`] over a page of the feed
//! and the album's last accepted state, [`check_metadata_blob`] over the bytes fetched for one
//! entry — so every rule is asserted directly, and the puller only times and sequences them.
//!
//! # The rules, as a pull meets them
//!
//! | Invariant | On a pulled entry |
//! | --- | --- |
//! | 1 | The entry's `protocol_version` is inside this server's window. |
//! | 2 | The manifest's `crypto_suite_id` is a suite this build knows. |
//! | 3 | Every blob address is the suite's digest length, as lowercase hex. |
//! | 4 | Every blob size is in `(0, max_file_size]`. |
//! | 5 | Not carried: the feed names no `content_type`. |
//! | 6 | The entry and its manifest name the subscribed album, at the album's pinned protocol date (the capability's `min_protocol_version`). |
//! | 7 | **Not re-applied**: it needs the writing member's device directory, which a capability does not reach. The receiving client's `verify_asset` checks the device signature. |
//! | 8 | The manifest's timestamp parses and is no further in the future than the drift bound. |
//! | 9–13 | The transfer's bounds, as a pull has them: a metadata blob is at most the declared size and the tier's bound, and arrives whole. |
//! | 14 | A fetched blob's recomputed hash is its address; an original listed beside the manifest is the one it commits to. |
//! | 15 | This function is the re-run. |
//! | 16 | The manifest decodes through the server's one parser, is canonical, keeps the presence rules, and its action agrees with the entry's change. |
//! | 17 | The stale-revival defense: a manifest the asset already moved past, or a second `create`, is refused. |
//! | 18 | An asset's AMK epoch never regresses along its chain as this server accepted it. Not album-wide: the feed re-mints a position without a new record and replays from zero after a restart, so an honest feed serves an older epoch after a newer one. MLS is the authority on the album's epoch, checked by the receiving client. |
//! | 25 | The metadata blob's content hash is its address. A record that binds one names it as `metadata_blob_hash` and the entry names that blob; a record that binds none is served with the blob the asset holds, which must be the held one when the record is or directly continues the held head, and never replaces the held one when it skips past it. |
//!
//! Beyond them, the feed's own contract: every entry names the subscribed album, and `sync_seq`
//! strictly increases past the high-water mark (the anti-rewind rule of design/sync.md).
//!
//! # Seventeen across a compacted feed
//!
//! The feed serves an asset's **current** manifest at its latest position, so a pull that
//! missed intermediate edits sees a manifest whose predecessor it never held. Requiring
//! `prior == head` would refuse every asset edited twice between pulls. So a pull refuses what
//! the Stale-Revival Defense names — a manifest *behind* the held head, which is one this
//! server already superseded ([`AssetHead::superseded`]) — and a second `create`, and accepts
//! the home server's current manifest otherwise. The home server is the authority on the
//! current manifest; the receiving client re-verifies the chain with keys.
//!
//! # Probation is stricter
//!
//! A peer inside `PEER_PROBATION` gets smaller pages and blobs ([`Rules::for_tier`]), its
//! entries must carry a manifest, and its manifests may carry no field this build does not know.

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::hash::{Hash32, hash_bytes};
use capsule_core::crypto::provenance::{
    Action, AssetManifest, DerivativeManifest, ManifestCore, ProvenanceRecord,
};
use capsule_core::validation::{
    amk_version_monotonic, check_suite, hash_length_ok, metadata_blob_hash_matches, protocol_gate,
    size_in_bounds,
};
use jiff::{SignedDuration, Timestamp};

use super::pulled::{AcceptedEntry, AssetHead, Change, PulledAlbum, PulledBlob, ValidatedPage};
use super::remote::rest::types::{SyncEntry, SyncPageResponse, WireBlobRole, WireChangeKind};
use crate::federation::PeerTier;
use crate::ledger::manifest::{self, ManifestFault};
use crate::store::AlbumId;
use crate::upload::UploadPolicy;

/// The longest cursor a peer may hand back. The cursor is stored and sent back verbatim, so it is
/// bounded like any other value a peer chooses; the issuer's own is 41 bytes, base64url.
pub const MAX_CURSOR_BYTES: usize = 1024;

/// How far a pull may read and what it demands, by the source peer's tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rules {
    /// Entries asked for per page; a page with more is refused.
    pub page_size: u32,
    /// Pages one pull reads before it stops and leaves the rest to the next.
    pub max_pages: u32,
    /// The most bytes one page's response may be.
    pub max_page_bytes: u64,
    /// The most bytes one metadata blob may be.
    pub max_metadata_blob_bytes: u64,
    /// The most blobs one entry may list.
    pub max_blobs_per_entry: usize,
    /// The most derivative manifests one entry may carry.
    pub max_derivative_manifests: usize,
    /// Whether an entry with no manifest (and no purge) fails the pull rather than being passed
    /// over.
    pub manifest_required: bool,
    /// Whether a manifest may carry fields this build does not know.
    pub unknown_fields_allowed: bool,
}

impl Rules {
    /// The rules a peer of `tier` is held to.
    pub fn for_tier(tier: PeerTier) -> Self {
        match tier {
            PeerTier::Established => Self {
                page_size: 100,
                max_pages: 20,
                max_page_bytes: 8 * 1024 * 1024,
                max_metadata_blob_bytes: 1024 * 1024,
                max_blobs_per_entry: 16,
                max_derivative_manifests: 8,
                manifest_required: false,
                unknown_fields_allowed: true,
            },
            PeerTier::Probation => Self {
                page_size: 25,
                max_pages: 4,
                max_page_bytes: 2 * 1024 * 1024,
                max_metadata_blob_bytes: 256 * 1024,
                max_blobs_per_entry: 8,
                max_derivative_manifests: 4,
                manifest_required: true,
                unknown_fields_allowed: false,
            },
        }
    }
}

/// What a page is validated against, beyond the album's own state.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    /// The subscribed album: every entry must name it.
    pub album: &'a AlbumId,
    /// The album's pinned protocol date, from the capability.
    pub pin: &'a str,
    /// The protocol window, drift bound and blob ceiling this server applies to its own writes.
    pub policy: &'a UploadPolicy,
    /// This server's clock.
    pub now: Timestamp,
}

/// A rule a peer's input broke. Spends the peer's error budget.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{rule}: {detail}")]
pub struct Invalid {
    /// The invariant or bound, as the log names it.
    pub rule: &'static str,
    /// What was wrong.
    pub detail: String,
    /// The manifest a stale revival served, when that is the rule broken: what the puller
    /// remembers in the [rejected-hash table](crate::federation::rejected_hashes).
    pub revived: Option<Revived>,
}

/// A manifest the album had already moved past, served again (invariant 17).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revived {
    /// The asset it names.
    pub asset_id: String,
    /// Its record hash: one of the asset's superseded heads.
    pub head: Hash32,
}

impl Invalid {
    fn new(rule: &'static str, detail: impl Into<String>) -> Self {
        Self {
            rule,
            detail: detail.into(),
            revived: None,
        }
    }

    /// The stale revival of `asset_id`'s superseded head `head`.
    fn revived(asset_id: &str, head: Hash32) -> Self {
        Self {
            revived: Some(Revived {
                asset_id: asset_id.to_owned(),
                head,
            }),
            ..Self::new(
                "invariant 17",
                "a manifest this album already moved past was served again",
            )
        }
    }
}

/// Why a page was not accepted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refused {
    /// The page broke a rule.
    #[error(transparent)]
    Invalid(#[from] Invalid),
    /// A manifest a newer writer produced: not malformed, and not readable by this build.
    ///
    /// Nothing of the page is written and the peer is not charged for it; the pull backs off
    /// until this server is upgraded (versioning invariants 3 and 5: refuse, never rewrite).
    #[error(
        "asset {asset_id} carries a provenance record of version {found}, newer than this build reads"
    )]
    Newer {
        /// The asset.
        asset_id: String,
        /// The version it declares.
        found: u64,
    },
}

/// A metadata blob a screened page needs fetched before it can be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetadataNeed {
    /// The address the manifest commits to.
    pub hash: Hash32,
    /// The size the entry declares for it, when it lists it.
    pub size: Option<u64>,
}

/// A page that passed every rule that needs no fetched bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screened {
    /// The page, with no metadata blob yet.
    pub page: ValidatedPage,
    /// The metadata blobs to fetch and check, each once.
    pub needed: Vec<MetadataNeed>,
    /// Whether the peer holds more after this page.
    pub has_more: bool,
}

/// Re-validate one page of `ctx.album`'s feed against `from`, the album's last accepted state.
///
/// # Errors
///
/// [`Refused`], naming the first rule broken. Nothing of a refused page is written.
pub fn screen_page(
    page: SyncPageResponse,
    from: &PulledAlbum,
    ctx: &Context<'_>,
    rules: &Rules,
) -> Result<Screened, Refused> {
    if page.entries.len() > rules.page_size as usize {
        return Err(Invalid::new(
            "page bound",
            format!(
                "{} entries, {} asked for",
                page.entries.len(),
                rules.page_size
            ),
        )
        .into());
    }
    if page.next_cursor.len() > MAX_CURSOR_BYTES {
        return Err(Invalid::new("cursor bound", "the cursor is too long to keep").into());
    }
    let album = uuid::Uuid::parse_str(ctx.album.as_str())
        .map_err(|_| Invalid::new("invariant 6", "the subscribed album is not a UUID"))?;

    let mut state = from.clone();
    let mut entries = Vec::with_capacity(page.entries.len());
    let mut needed: BTreeMap<Hash32, Option<u64>> = BTreeMap::new();
    for entry in page.entries {
        let screened = screen_entry(entry, album, &mut state, ctx, rules)?;
        if let Some((accepted, fetch)) = screened {
            if let Some(hash) = fetch {
                let size = accepted
                    .blobs
                    .iter()
                    .find(|blob| blob.role == "metadata" && blob.hash == hash)
                    .map(|blob| blob.size);
                needed.insert(hash, size);
            }
            entries.push(accepted);
        }
    }
    state.cursor = Some(page.next_cursor);
    Ok(Screened {
        page: ValidatedPage {
            entries,
            metadata_blobs: BTreeMap::new(),
            state,
        },
        needed: needed
            .into_iter()
            .map(|(hash, size)| MetadataNeed { hash, size })
            .collect(),
        has_more: page.has_more,
    })
}

/// One entry, applied to `state` when it passes. `None` when it is passed over: an entry with no
/// manifest from an established peer, which its home server could not produce. Beside the entry,
/// the metadata blob the page must fetch for it, if any.
fn screen_entry(
    entry: SyncEntry,
    album: uuid::Uuid,
    state: &mut PulledAlbum,
    ctx: &Context<'_>,
    rules: &Rules,
) -> Result<Option<(AcceptedEntry, Option<Hash32>)>, Refused> {
    // The feed's own contract: the album asked for, in strictly increasing order.
    if uuid::Uuid::parse_str(&entry.album_id).ok() != Some(album) {
        return Err(Invalid::new("invariant 6", "an entry names another album").into());
    }
    // Unsigned on the wire: a negative `sync_seq` never decodes, so a page carrying one is
    // refused as malformed before it is validated.
    let sync_seq = entry.sync_seq;
    if state.high_water.is_some_and(|high| sync_seq <= high) {
        return Err(Invalid::new(
            "anti-rewind",
            format!("sync_seq {sync_seq} is not past {:?}", state.high_water),
        )
        .into());
    }
    state.high_water = Some(sync_seq);

    let asset = uuid::Uuid::parse_str(&entry.asset_id)
        .map_err(|_| Invalid::new("invariant 16", "an asset id is not a UUID"))?;
    let asset_id = asset.to_string();

    // 1 and 6: the protocol date.
    protocol_gate(
        &entry.protocol_version,
        ctx.policy.protocol_min(),
        ctx.policy.protocol_max(),
    )
    .map_err(|reject| Invalid::new("invariant 1", format!("{reject:?}")))?;
    if entry.protocol_version != ctx.pin {
        return Err(Invalid::new(
            "invariant 6",
            "an entry is not at the album's pinned protocol date",
        )
        .into());
    }

    let change = match entry.change {
        WireChangeKind::Created => Change::Created,
        WireChangeKind::Updated => Change::Updated,
        WireChangeKind::Deleted => Change::Deleted,
    };
    let purged = entry.purged.unwrap_or(false);
    if purged && change != Change::Deleted {
        return Err(Invalid::new("invariant 16", "a purge on an entry that is no delete").into());
    }
    let changed_at = entry
        .changed_at
        .parse::<Timestamp>()
        .map_err(|_| Invalid::new("invariant 8", "changed_at is not RFC 3339"))?;

    let blobs = screen_blobs(&entry, ctx, rules)?;
    let derivative_manifests = screen_derivatives(&entry, rules)?;
    let metadata_named = entry
        .metadata_blob
        .as_deref()
        .map(|hash| address(hash, "invariant 25"))
        .transpose()?;

    let Some(encoded) = entry.manifest_cbor.as_deref() else {
        if purged {
            // A purged tombstone carries no record: the asset's references are gone.
            if let Some(head) = state.assets.get(&asset_id).cloned() {
                state.assets.insert(
                    asset_id.clone(),
                    AssetHead {
                        deleted: true,
                        ..head
                    },
                );
            }
            return Ok(Some((
                AcceptedEntry {
                    asset_id,
                    sync_seq,
                    change,
                    changed_at,
                    manifest: None,
                    head: None,
                    metadata_blob: None,
                    blobs: Vec::new(),
                    derivative_manifests: Vec::new(),
                    original_held: entry.original_held,
                    purged,
                },
                None,
            )));
        }
        if rules.manifest_required {
            return Err(Invalid::new(
                "invariant 16",
                "an entry from a peer on probation carries no manifest",
            )
            .into());
        }
        tracing::info!(asset = %asset_id, sync_seq, "a pulled entry carries no manifest; it is passed over");
        return Ok(None);
    };

    let bytes = BASE64
        .decode(encoded)
        .map_err(|_| Invalid::new("invariant 16", "a manifest is not base64"))?;
    let record = screen_manifest(&bytes, &asset_id)?;
    let core = &record.core;

    // 16: the record agrees with the entry and the album.
    if core.file_id != asset {
        return Err(Invalid::new("invariant 16", "a manifest names another asset").into());
    }
    if core.album_id != album {
        return Err(Invalid::new("invariant 6", "a manifest names another album").into());
    }
    if (core.action == Action::Delete) != (change == Change::Deleted) {
        return Err(Invalid::new(
            "invariant 16",
            "the manifest's action disagrees with the entry's change",
        )
        .into());
    }
    if !rules.unknown_fields_allowed && !core.unknown.is_empty() {
        return Err(Invalid::new(
            "invariant 16",
            "a manifest from a peer on probation carries fields this build does not know",
        )
        .into());
    }
    // 2, 3 and 6 over the manifest.
    check_suite(core.crypto_suite_id)
        .map_err(|_| Invalid::new("invariant 2", "an unknown crypto suite"))?;
    if !hash_length_ok(core.crypto_suite_id, core.ciphertext_hash.as_bytes().len()) {
        return Err(Invalid::new(
            "invariant 3",
            "the ciphertext hash is not the suite's digest",
        )
        .into());
    }
    if core.protocol_version != ctx.pin {
        return Err(Invalid::new(
            "invariant 6",
            "a manifest is not at the album's pinned protocol date",
        )
        .into());
    }
    // 8: the self-asserted time is grammatical and not from the future.
    let written = core
        .timestamp
        .parse::<Timestamp>()
        .map_err(|_| Invalid::new("invariant 8", "the manifest's timestamp is not RFC 3339"))?;
    let drift = SignedDuration::from_hours(24 * ctx.policy.drift_days());
    if written.duration_since(ctx.now) > drift {
        return Err(Invalid::new(
            "invariant 8",
            "the manifest's timestamp is past the drift bound",
        )
        .into());
    }
    // 14: an original listed beside the manifest is the one it commits to.
    if change != Change::Deleted
        && let Some(original) = blobs.iter().find(|blob| blob.role == "original")
        && original.hash != core.ciphertext_hash
    {
        return Err(Invalid::new(
            "invariant 14",
            "the listed original is not the one the manifest commits to",
        )
        .into());
    }
    let head = hash_bytes(&bytes);
    let held = state.assets.get(&asset_id);
    // 25: the entry names the metadata blob the asset's records bind.
    let (metadata_blob, fetch) = metadata_bound(core, head, metadata_named, held)?;
    // 17: the stale-revival defense.
    if let Some(held) = held {
        if held.superseded.contains(&head) {
            return Err(Invalid::revived(&asset_id, head).into());
        }
        if core.action.is_create() && held.head != head {
            return Err(
                Invalid::new("invariant 17", "a second create for an asset already held").into(),
            );
        }
        if held.head != head && core.prior_provenance_hash != Some(held.head) {
            tracing::debug!(asset = %asset_id, "a pulled manifest continues past heads this server never saw");
        }
    }
    // 18: the epoch never regresses along the asset's chain. Not across the album: the feed
    // re-mints an asset's position without a new record and replays from zero after a restart,
    // so an honest feed serves an older epoch's record after a newer one.
    if !amk_version_monotonic(core.amk_version.0, held.map(|held| held.amk_version)) {
        return Err(Invalid::new("invariant 18", "the manifest's AMK epoch regresses").into());
    }
    state.amk_version = Some(state.amk_version.unwrap_or(0).max(core.amk_version.0));
    let deleted = core.action == Action::Delete;
    let next = AssetHead {
        head,
        superseded: Vec::new(),
        deleted,
        amk_version: core.amk_version.0,
        // A delete binds no blob: a peer's page names none on it, and one it names anyway is
        // not taken. The asset still holds the one it bound, which a `trash-restore` brings back.
        metadata_blob: if deleted {
            held.and_then(|held| held.metadata_blob)
        } else {
            metadata_blob
        },
    };
    let next = match held {
        Some(held) => held.advanced(next),
        None => next,
    };
    state.assets.insert(asset_id.clone(), next);

    Ok(Some((
        AcceptedEntry {
            asset_id,
            sync_seq,
            change,
            changed_at,
            manifest: Some(bytes),
            head: Some(head),
            metadata_blob,
            blobs,
            derivative_manifests,
            original_held: entry.original_held,
            purged,
        },
        fetch,
    )))
}

/// Invariant 25 on one entry: the metadata blob it names, given its record and the asset's held
/// head. Answers the blob the asset now carries, and the blob the page must fetch for it.
///
/// A record that binds one (`create`, `replace`, `metadata-update`) must be served with exactly
/// that blob. A `delete` binds none and takes none: a home server serves it with none, and a
/// blob one names anyway (the shape the owner's own feed gives a tombstone) is neither taken
/// nor fetched, so the page is accepted and a blob gone past its window never aborts the pull.
/// A record that binds none (`derivative-*`,
/// `trash-restore`) is served with the blob the asset's last binding record committed to, which
/// the home server keeps:
///
/// - When this server holds no blob for the asset, the binding record is one the compacted feed
///   no longer serves, so the named blob is taken at its address (fetched and checked against
///   it) and the receiving client verifies the chain with keys.
/// - When the record is the head held here or directly continues it, the named blob must be the
///   held one, so a swap is refused.
/// - When it continues past records this server never pulled, the named blob is never taken
///   over the held one: only a signed binding record proves a newer blob, and a record binding
///   none proves nothing about it. A peer could otherwise pair a genuine skipping record with an
///   older genuine blob of the same asset and roll back what this server holds. The held blob
///   is kept and nothing is fetched; a newer blob arrives with the next binding record.
fn metadata_bound(
    core: &ManifestCore,
    head: Hash32,
    named: Option<Hash32>,
    held: Option<&AssetHead>,
) -> Result<(Option<Hash32>, Option<Hash32>), Invalid> {
    if core.action == Action::Delete {
        if let Some(named) = named {
            tracing::debug!(
                named = %named.to_hex(),
                "a delete names a metadata blob; it binds none, so the blob is neither taken nor fetched"
            );
        }
        return Ok((None, None));
    }
    if core.action.binds_metadata_blob() {
        if named != core.metadata_blob_hash {
            return Err(Invalid::new(
                "invariant 25",
                "the entry's metadata blob is not the one the manifest commits to",
            ));
        }
        return Ok((named, named));
    }
    let Some(held) = held.filter(|held| held.metadata_blob.is_some()) else {
        tracing::debug!(action = ?core.action, "a record binding no metadata blob names one this server does not hold; it is taken at its address");
        return Ok((named, named));
    };
    if named == held.metadata_blob {
        return Ok((named, named));
    }
    if held.head == head || core.prior_provenance_hash == Some(held.head) {
        return Err(Invalid::new(
            "invariant 25",
            "a record binding no metadata blob is served with another than the asset holds",
        ));
    }
    tracing::info!(
        action = ?core.action,
        named = ?named.map(|hash| hash.to_hex()),
        held = ?held.metadata_blob.map(|hash| hash.to_hex()),
        "a record binding no metadata blob skips past the held head with another blob; the held blob is kept"
    );
    Ok((held.metadata_blob, None))
}

/// Decode `bytes` through the server's one manifest parser, and require the record form, in its
/// canonical encoding, keeping the presence rules.
fn screen_manifest(bytes: &[u8], asset_id: &str) -> Result<AssetManifest, Refused> {
    let manifest = match manifest::parse_valid(bytes) {
        Ok(manifest) => manifest,
        Err(ManifestFault::NewerVersion { found }) => {
            return Err(Refused::Newer {
                asset_id: asset_id.to_owned(),
                found,
            });
        }
        Err(fault) => {
            return Err(
                Invalid::new("invariant 16", format!("the manifest is refused: {fault}")).into(),
            );
        }
    };
    // The chain head is the record's hash, and a peer could otherwise present one record under
    // many encodings, each hashing differently, to slip past the superseded-head check.
    // A bare manifest is what a provenance blob held before the record wrapped it, and the
    // upload path accepts both forms under the same rule.
    let reencoded = match ProvenanceRecord::decode(bytes) {
        Ok(record) => capsule_core::cbor::to_canonical_vec(&record),
        Err(_) => capsule_core::cbor::to_canonical_vec(&manifest),
    };
    if !reencoded.is_ok_and(|reencoded| reencoded == bytes) {
        return Err(Invalid::new("invariant 16", "the manifest is not canonically encoded").into());
    }
    Ok(manifest)
}

/// The entry's blob list, bounded and checked against 3 and 4.
fn screen_blobs(
    entry: &SyncEntry,
    ctx: &Context<'_>,
    rules: &Rules,
) -> Result<Vec<PulledBlob>, Refused> {
    if entry.blobs.len() > rules.max_blobs_per_entry {
        return Err(
            Invalid::new("entry bound", format!("{} blobs listed", entry.blobs.len())).into(),
        );
    }
    entry
        .blobs
        .iter()
        .map(|blob| {
            let role = match blob.role {
                WireBlobRole::Original => "original",
                WireBlobRole::Metadata => "metadata",
                WireBlobRole::Derivative => "derivative",
                WireBlobRole::Provenance => "provenance",
                WireBlobRole::DerivativeManifest => "derivative_manifest",
                // A backup is outside every scope; a feed that lists one to a peer is wrong.
                WireBlobRole::Backup => {
                    return Err(Invalid::new("invariant 16", "an entry lists a backup").into());
                }
            };
            let size = Some(blob.size)
                .filter(|size| size_in_bounds(*size, ctx.policy.max_file_bytes()))
                .ok_or_else(|| Invalid::new("invariant 4", "a blob size is out of bounds"))?;
            Ok(PulledBlob {
                role,
                hash: address(&blob.hash, "invariant 3")?,
                size,
            })
        })
        .collect()
}

/// The entry's derivative manifests, bounded, decoded to prove they are what they claim.
fn screen_derivatives(entry: &SyncEntry, rules: &Rules) -> Result<Vec<Vec<u8>>, Refused> {
    let encoded = entry.derivative_manifests.as_deref().unwrap_or_default();
    if encoded.len() > rules.max_derivative_manifests {
        return Err(Invalid::new(
            "entry bound",
            format!("{} derivative manifests", encoded.len()),
        )
        .into());
    }
    encoded
        .iter()
        .map(|encoded| {
            let bytes = BASE64
                .decode(encoded)
                .map_err(|_| Invalid::new("invariant 16", "a derivative manifest is not base64"))?;
            if bytes.len() as u64 > manifest::MAX_MANIFEST_BYTES {
                return Err(Invalid::new(
                    "entry bound",
                    "a derivative manifest is over the manifest ceiling",
                )
                .into());
            }
            capsule_core::cbor::from_slice::<DerivativeManifest>(&bytes).map_err(|_| {
                Invalid::new("invariant 16", "a derivative manifest does not decode")
            })?;
            Ok(bytes)
        })
        .collect()
}

/// A content address: 64 lowercase hex digits (3: the suite's digest length).
fn address(text: &str, rule: &'static str) -> Result<Hash32, Invalid> {
    if text.len() != 64
        || !text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(Invalid::new(
            rule,
            "a blob address is not 64 lowercase hex digits",
        ));
    }
    Hash32::from_hex(text).map_err(|_| Invalid::new(rule, "a blob address is not hex"))
}

/// The most bytes to read for `need`: its declared size, never past the tier's bound.
pub fn metadata_read_limit(need: &MetadataNeed, rules: &Rules) -> u64 {
    need.size.map_or(rules.max_metadata_blob_bytes, |size| {
        size.min(rules.max_metadata_blob_bytes)
    })
}

/// Check the bytes fetched for `need`: whole (9–13), within the tier's bound, and the blob the
/// manifest commits to (14, 25).
///
/// # Errors
///
/// [`Invalid`], naming the rule.
pub fn check_metadata_blob(
    need: &MetadataNeed,
    bytes: &[u8],
    rules: &Rules,
) -> Result<(), Invalid> {
    let length = bytes.len() as u64;
    if length > rules.max_metadata_blob_bytes {
        return Err(Invalid::new(
            "invariant 11",
            "the metadata blob is over the tier's bound",
        ));
    }
    if need.size.is_some_and(|size| size != length) {
        return Err(Invalid::new(
            "invariant 13",
            "the metadata blob is not the size the entry declares",
        ));
    }
    if !metadata_blob_hash_matches(Some(need.hash), bytes) {
        return Err(Invalid::new(
            "invariant 25",
            "the metadata blob's hash is not its address",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
