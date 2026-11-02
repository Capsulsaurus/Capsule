//! Invariant 20: everything a peer sends is re-validated before anything of it is written.
//!
//! design/threat-model/validation.md: on federation pull, invariants 1–18 are re-applied and
//! federation never loosens a rule; invariant 25 binds the metadata blob. This module is that
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
//! | 18 | The album's AMK epoch never regresses across what this server accepted. |
//! | 25 | The metadata blob's content hash is the manifest's `metadata_blob_hash`, and the entry names that blob. |
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
    Action, AssetManifest, DerivativeManifest, ProvenanceRecord,
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
}

impl Invalid {
    fn new(rule: &'static str, detail: impl Into<String>) -> Self {
        Self {
            rule,
            detail: detail.into(),
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
        if let Some(accepted) = screened {
            if let Some(hash) = accepted.metadata_blob {
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
/// manifest from an established peer, which its home server could not produce.
fn screen_entry(
    entry: SyncEntry,
    album: uuid::Uuid,
    state: &mut PulledAlbum,
    ctx: &Context<'_>,
    rules: &Rules,
) -> Result<Option<AcceptedEntry>, Refused> {
    // The feed's own contract: the album asked for, in strictly increasing order.
    if uuid::Uuid::parse_str(&entry.album_id).ok() != Some(album) {
        return Err(Invalid::new("invariant 6", "an entry names another album").into());
    }
    let sync_seq = u64::try_from(entry.sync_seq)
        .map_err(|_| Invalid::new("anti-rewind", "a negative sync_seq"))?;
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
            return Ok(Some(AcceptedEntry {
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
            }));
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
    // 25: the entry names exactly the metadata blob the manifest commits to.
    if metadata_named != core.metadata_blob_hash {
        return Err(Invalid::new(
            "invariant 25",
            "the entry's metadata blob is not the one the manifest commits to",
        )
        .into());
    }
    // 17: the stale-revival defense.
    let head = hash_bytes(&bytes);
    let held = state.assets.get(&asset_id);
    if let Some(held) = held {
        if held.superseded.contains(&head) {
            return Err(Invalid::new(
                "invariant 17",
                "a manifest this album already moved past was served again",
            )
            .into());
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
    // 18: the album's epoch never regresses.
    if !amk_version_monotonic(core.amk_version.0, state.amk_version) {
        return Err(Invalid::new("invariant 18", "the manifest's AMK epoch regresses").into());
    }
    state.amk_version = Some(state.amk_version.unwrap_or(0).max(core.amk_version.0));
    let deleted = core.action == Action::Delete;
    let next = match held {
        Some(held) => held.advanced(head, deleted),
        None => AssetHead {
            head,
            superseded: Vec::new(),
            deleted,
        },
    };
    let metadata_blob = core.metadata_blob_hash;
    state.assets.insert(asset_id.clone(), next);

    Ok(Some(AcceptedEntry {
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
    }))
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
            let size = u64::try_from(blob.size)
                .ok()
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
