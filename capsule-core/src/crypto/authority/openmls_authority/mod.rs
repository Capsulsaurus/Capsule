//! The live [`AlbumAuthority`] backed by a real OpenMLS group (RFC 9420), pinned to the
//! X-Wing PQ ciphersuite `MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519` (`0x004D`) via the
//! formally-verified libcrux provider. This is the design-target authority the offline
//! [`ReferenceAuthority`](super::ReferenceAuthority) stands in for — it drops in behind the
//! same `&dyn AlbumAuthority` seam without touching [`verify_asset`](crate::crypto::verify_asset).
//!
//! **Slice S-X1** landed the backend/authority layer: single-member (self) group creation, the
//! epoch-ledger semantics `verify_asset` consumes (monotonic ceiling, per-epoch write-tier key,
//! AMK export, `has_amk` pending window). **Slice S-X2** (this module) lands the membership layer:
//!
//! - the four lifecycle ceremonies — [add member](OpenMlsAuthority::add_member),
//!   [remove member](OpenMlsAuthority::remove_member), [self-update /
//!   rotate](OpenMlsAuthority::rotate_epoch), and [join via Welcome](OpenMlsAuthority::join_via_welcome)
//!   — every one driven through OpenMLS commits/Welcome processing, never server-asserted state;
//! - in-band [`AlbumKeyDistribution`] AMK delivery and per-album [`HistoryPolicy`] history batches
//!   ([messages]);
//! - **minted-and-distributed write-tier keys**: the committer of every epoch-advancing commit
//!   mints the epoch's hybrid write-tier keypair; the *public* half is attested by the commit
//!   itself (authenticated AAD, i.e. the admin-signed chain), and the *private* half is delivered
//!   over an MLS [`WriteTierDistribution`] application message to the epoch's
//!   [writers](OpenMlsAuthority::writers). It is **never derivable from group secrets**, so a
//!   member outside the distribution set can verify manifests but cannot sign them — the
//!   keys-doc "distributed via MLS to writers only" shape (today's writer set is all members;
//!   the filter is the documented roles seam);
//! - the hybrid **MLS ↔ device-identity LeafNode binding** ([identity]);
//! - durable group **persistence** via an [export](OpenMlsAuthority::export_state) /
//!   [import](OpenMlsAuthority::import_state) blob over an owned, serializable storage
//!   [provider].
//!
//! **Slice S-X3** (this module's [upgrade] and [resilience] submodules) lands the album **upgrade
//! ceremony** and **MLS resilience**:
//!
//! - the **tombstone-plus-fork** upgrade ceremony ([upgrade]): a version-pinned album is frozen by
//!   an `AlbumTombstone` commit and re-founded as a fork at a target `protocol_version` /
//!   `crypto_suite_id`, with an `upgraded_from` continuity pointer. Suite-parametric (the general
//!   vehicle for a future move off the `0x004D` X-Wing suite), `intent_id`-keyed and resumable;
//! - the **group re-keying ceremony** ([resilience]): a compromise/scheduled response that mints a
//!   fresh AMK + write-tier key for every member as one `intent_id`-keyed, resumable operation;
//! - **reconciliation** ([`ReconcileOutcome`](resilience::ReconcileOutcome)): the single
//!   "bring-me-current" entry point over the server-authoritative commit chain, plus the
//!   lost-commit retry primitive.
//!
//! SSoT: [Cryptography — MLS](https://docs/design/cryptography/mls/),
//! [Keys — Write Authority](https://docs/design/cryptography/keys/#write-authorization),
//! [MLS Resilience](https://docs/design/mls-resilience/),
//! [Versioning — Album Upgrade Ceremony](https://docs/design/versioning/#album-upgrade-ceremony).

mod identity;
mod messages;
mod provider;
mod resilience;
mod upgrade;

use std::collections::{BTreeMap, BTreeSet};

pub use identity::MlsDeviceIdentity;
use messages::MlsAppPayload;
pub use messages::{
    AlbumHistoryBundle, AlbumKeyDistribution, AmkHistoryEntry, HistoryPolicy, WriteTierDistribution,
};
use openmls::group::{
    CommitMessageBundle, GroupId, MlsGroup, MlsGroupJoinConfig, StagedCommit, StagedWelcome,
};
use openmls::prelude::{
    Ciphersuite, LeafNodeIndex, LeafNodeParameters, MlsMessageBodyIn, MlsMessageIn, MlsMessageOut,
    ProcessedMessageContent,
};
use openmls_basic_credential::SignatureKeyPair;
use openmls_traits::OpenMlsProvider;
use provider::CapsuleMlsProvider;
use resilience::RekeyState;
pub use resilience::{
    CommitHash, LostCommitTracker, ReconcileOutcome, RekeyOutcome, RekeyReason, ServerChainView,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use upgrade::Quiescence;
pub use upgrade::{
    AlbumStateSummary, DEFAULT_UPGRADE_DEADLINE, SignedUpgradeIntent, TombstoneOutcome,
    UpgradeIntent, UpgradeLineage, UpgradeProposal,
};
use uuid::Uuid;

use super::AlbumAuthority;
use crate::crypto::keys::{AmkVersion, DeviceDirectory, HybridSigningKey, HybridVerifyingKey};

/// The MLS ciphersuite Capsule pins every album group to: X-Wing (ML-KEM-768 + X25519) KEM,
/// ChaCha20-Poly1305 AEAD, SHA-256, Ed25519 signatures — codepoint `0x004D`. The suite ships
/// today in OpenMLS via its libcrux provider (SSoT: [Cryptography — MLS](https://docs/design/cryptography/mls/)).
pub(crate) const PINNED_CIPHERSUITE: Ciphersuite =
    Ciphersuite::MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519;

/// The wire codepoint of [`PINNED_CIPHERSUITE`]. Asserted in tests so an upstream re-pin can
/// never silently change the suite Capsule negotiates.
pub const PINNED_CIPHERSUITE_ID: u16 = 0x004D;

/// The RFC 9420 exporter label the AMK content key is derived under. A fixed, suite-scoped
/// domain-separation string; the album id is the exporter *context*, so two albums at the same
/// epoch never export the same AMK.
const AMK_EXPORT_LABEL: &str = "capsule amk v1";

/// The AMK content-key length (32 bytes), matching [`Amk`](crate::crypto::keys::Amk).
pub(crate) const AMK_LEN: usize = 32;

/// The seed length of a hybrid write-tier signing key (32-byte Ed25519 seed ‖ 32-byte ML-DSA ξ).
/// The per-epoch write-tier keypair is **minted** by the committer (never derived from group
/// secrets) and its private half distributed via [`WriteTierDistribution`]; this is the length
/// that message and the persistence blob carry.
pub(crate) const WRITE_TIER_SEED_LEN: usize = 64;

/// Errors from constructing, advancing, or running a membership ceremony on an
/// [`OpenMlsAuthority`]. OpenMLS/libcrux error details are captured as English strings
/// (server-internal diagnostics, not user-facing).
#[derive(Debug, Error)]
pub enum OpenMlsAuthorityError {
    /// The libcrux crypto provider failed to initialise.
    #[error("libcrux provider init failed: {0}")]
    Provider(String),
    /// The MLS leaf signature keypair could not be created or stored.
    #[error("mls signature keypair: {0}")]
    Signature(String),
    /// The MLS group could not be created at the pinned ciphersuite.
    #[error("mls group create failed: {0}")]
    GroupCreate(String),
    /// A self-update commit (epoch advance) failed to be produced or merged.
    #[error("mls self-update commit failed: {0}")]
    SelfUpdate(String),
    /// The RFC 9420 exporter failed to derive the AMK for the current epoch.
    #[error("mls exporter failed: {0}")]
    Export(String),
    /// The exporter returned an AMK of the wrong length (never expected under a fixed suite).
    #[error("exported AMK wrong length: expected {AMK_LEN}, got {0}")]
    AmkLength(usize),
    /// A KeyPackage could not be built for a joining device.
    #[error("mls key package build failed: {0}")]
    KeyPackage(String),
    /// An `Add` proposal + commit failed.
    #[error("mls add-member ceremony failed: {0}")]
    AddMember(String),
    /// A `Remove` proposal + commit failed.
    #[error("mls remove-member ceremony failed: {0}")]
    RemoveMember(String),
    /// Processing an incoming MLS message (commit / application) failed.
    #[error("mls process-message failed: {0}")]
    ProcessMessage(String),
    /// A received message was not the kind the ceremony expected (e.g. a proposal where a commit
    /// or application message was required).
    #[error("unexpected mls message: {0}")]
    UnexpectedMessage(String),
    /// A `Welcome` could not be processed into a joined group.
    #[error("mls welcome processing failed: {0}")]
    Welcome(String),
    /// The MLS ↔ device-identity LeafNode binding failed to build or verify.
    #[error("mls leaf identity binding: {0}")]
    Binding(String),
    /// An in-band key-delivery message could not be encoded/decoded or applied.
    #[error("mls key-delivery message: {0}")]
    Message(String),
    /// Durable group-state export/import failed.
    #[error("mls persistence: {0}")]
    Persist(String),
    /// A write / ceremony was attempted on a **tombstoned** (frozen) album: the album has been
    /// upgraded and all activity has moved to the fork. Reads are unaffected — only writes refuse.
    #[error("album is tombstoned under upgrade intent {0}")]
    Tombstoned(Uuid),
    /// A step of the [tombstone-plus-fork upgrade ceremony](upgrade) failed (intent signature,
    /// quiescence conflict, or fork construction).
    #[error("album upgrade ceremony: {0}")]
    Upgrade(String),
    /// A received `AlbumTombstone`'s `frozen_state_hash` disagrees with this member's own recomputed
    /// hash — at least one member's album view diverges. The upgrade aborts and the album returns to
    /// normal operation (each member independently).
    #[error("frozen-state hash mismatch: at least one member's album view diverges")]
    FrozenStateMismatch,
    /// A [resilience](resilience) operation (reconciliation / re-keying) failed.
    #[error("mls resilience: {0}")]
    Resilience(String),
    /// [`block_user`](OpenMlsAuthority::block_user) was asked to block the **local** user. A device
    /// cannot evict itself from its own group, and returning success would tell the caller a block
    /// took effect when nothing was removed (`S-X4`).
    #[error(
        "refusing to block the local user {0}: a device cannot remove itself from its own album"
    )]
    BlockSelf(Uuid),
}

impl From<crate::crypto::upgrade::UpgradeError> for OpenMlsAuthorityError {
    /// The ceremony's wire vocabulary is ungated (`S-C24`), so its errors arrive from outside
    /// this module and are folded into the one variant that already names this ceremony.
    fn from(error: crate::crypto::upgrade::UpgradeError) -> Self {
        Self::Upgrade(error.to_string())
    }
}

type Result<T> = std::result::Result<T, OpenMlsAuthorityError>;

/// Per-epoch write authority + content key.
///
/// The write-tier keypair is **minted by the epoch's committer** and explicitly distributed —
/// never derived from group secrets — so what a member holds depends on what was delivered to it:
///
/// - `write_tier_pub`: `Some` once attested — by the epoch's own commit (authenticated AAD), by a
///   received [`WriteTierDistribution`], or by a join-time history entry. `None` only in the
///   narrow window where a joiner has adopted the current epoch from its Welcome but the
///   committer's write-tier delivery has not yet been applied.
/// - `write_tier_priv`: `Some` only for an epoch whose [`WriteTierDistribution`] this member
///   received (or that it minted itself as committer). This is the **only** way signing capability
///   exists — pending semantics analogous to `has_amk`: a member that has not received the
///   distribution has no sign-capable handle for the epoch and cannot produce a `write_sig`.
struct EpochState {
    write_tier_pub: Option<HybridVerifyingKey>,
    write_tier_priv: Option<HybridSigningKey>,
    amk: [u8; AMK_LEN],
}

/// How an epoch's write-tier key material arrives at [`ingest_current_epoch`]
/// (`OpenMlsAuthority::ingest_current_epoch`).
enum WriteTierIngest {
    /// This member is the committer: it minted the keypair (holds both halves).
    Minted(HybridSigningKey),
    /// A peer's commit attested the public half through its authenticated AAD; the private half
    /// awaits the [`WriteTierDistribution`] delivery.
    Attested(HybridVerifyingKey),
    /// A joiner adopting the current epoch from a Welcome: neither half yet — both arrive with
    /// the committer's [`WriteTierDistribution`] in the join's history messages.
    Deferred,
}

/// The commit-AAD payload attesting the fresh epoch's write-tier **public** key. Riding the
/// commit's authenticated data means the attestation is covered by the committer's leaf signature
/// and validated by OpenMLS with the commit itself — the code-level form of the keys-doc's
/// "write-tier public key learned from the admin-signed attestation chain", exactly parallel to
/// how [`ReferenceAuthority`](super::ReferenceAuthority) epochs attest their pubkey.
#[derive(Serialize, Deserialize)]
struct WriteTierAttestation {
    /// The epoch the commit advances the group to.
    amk_version: u32,
    /// The write-tier public key minted for that epoch.
    write_tier_pub: HybridVerifyingKey,
}

/// The authenticated-data payload every epoch-advancing commit carries. Always attests the fresh
/// epoch's write-tier public key; on the single `AlbumTombstone` commit of an upgrade ceremony it
/// **also** carries the [`TombstoneMark`](upgrade::TombstoneMark) so every receiving member can
/// recompute and check the `frozen_state_hash` before adopting the freeze. Wrapping the two in one
/// enum keeps a single commit-processing path — [`process_commit`](OpenMlsAuthority::process_commit)
/// dispatches on the variant.
#[derive(Serialize, Deserialize)]
enum CommitAad {
    /// An ordinary epoch-advancing commit (add / remove / self-update / rotation / re-key).
    WriteTier(WriteTierAttestation),
    /// The `AlbumTombstone` commit that freezes the album for an upgrade ceremony. Still attests the
    /// terminal epoch's write-tier (so ingestion is uniform), plus the tombstone mark.
    Tombstone {
        /// The terminal epoch's write-tier attestation (unused for signing — the group is frozen —
        /// but ingested so the epoch ledger stays well-formed).
        write_tier: WriteTierAttestation,
        /// The freeze marker recomputed and checked by every receiving member.
        mark: upgrade::TombstoneMark,
    },
}

/// The outcome of an [`add_member`](OpenMlsAuthority::add_member) ceremony: the artifacts the
/// caller relays over the (out-of-scope, server-side) delivery service.
pub struct AddOutcome {
    /// The `Add` commit, for delivery to the **existing** members so they advance to the new epoch.
    pub commit: MlsMessageOut,
    /// The `Welcome`, for delivery to the **joining** device(s).
    pub welcome: MlsMessageOut,
    /// In-band key-delivery application messages: the new epoch's [`AlbumKeyDistribution`] (read
    /// capability, all members), its [`WriteTierDistribution`] (write capability, the
    /// [writers](OpenMlsAuthority::writers) set), and the joiner's [`AlbumHistoryBundle`] per the
    /// album's [`HistoryPolicy`]. Recipients apply the ones addressed to their role (application
    /// is idempotent). Deliver **after** the commit/welcome so recipients are at the new epoch.
    pub key_delivery: Vec<MlsMessageOut>,
}

/// The outcome of a [`remove_member`](OpenMlsAuthority::remove_member) ceremony.
pub struct RemoveOutcome {
    /// The `Remove` commit, for delivery to the **remaining** members. The removed device cannot
    /// process it (it is evicted), so it never advances past its removal epoch.
    pub commit: MlsMessageOut,
    /// The fresh (re-keyed) epoch's [`AlbumKeyDistribution`] (all remaining members) and
    /// [`WriteTierDistribution`] (the remaining [writers](OpenMlsAuthority::writers)).
    pub key_delivery: Vec<MlsMessageOut>,
}

/// The outcome of a [`block_user`](OpenMlsAuthority::block_user) ceremony (`S-X4`).
///
/// Deliberately distinguishes "removed them" from "they were not here", because the two have
/// different consequences for the caller: only the first produces a commit to deliver and a new
/// epoch to publish keys for.
pub struct BlockOutcome {
    /// The user that was blocked.
    pub user_id: Uuid,
    /// The leaf indices removed — one per device the blocked user had in this album. Empty iff
    /// they were not a member.
    pub removed_leaves: Vec<u32>,
    /// The epoch new writes are authorized under after the block. Bumped by exactly one when a
    /// removal happened; the unchanged ceiling otherwise.
    pub amk_version: AmkVersion,
    /// The `Remove` commit + the fresh epoch's key delivery, or `None` when the blocked user held
    /// no leaves in this album (nothing to commit, no epoch burnt).
    pub removal: Option<RemoveOutcome>,
}

impl BlockOutcome {
    /// Whether the blocked user was already absent from this album, so no MLS commit was produced
    /// and the epoch did not move.
    pub fn already_absent(&self) -> bool {
        self.removal.is_none()
    }
}

/// An [`AlbumAuthority`] backed by a live OpenMLS group pinned to [`PINNED_CIPHERSUITE`].
///
/// One instance owns one album's group **as seen by one device**. Its epoch ledger is produced by
/// real MLS commits (self-update, add, remove) and Welcome processing; every `verify_asset` answer
/// is read from live MLS state, never a server assertion.
pub struct OpenMlsAuthority {
    album_id: Uuid,
    /// This device's MLS participation identity: the owned provider (crypto + rand + serializable
    /// storage), the MLS leaf signer, and the hybrid DSK that attests the leaf binding.
    identity: MlsDeviceIdentity,
    /// The live MLS group. Its epoch (`u64`) maps to `amk_version = epoch + 1`.
    group: MlsGroup,
    /// The album's fixed history policy (album metadata; never chosen per-add).
    history_policy: HistoryPolicy,
    /// Per-epoch write authority + AMK, keyed by `amk_version`.
    epochs: BTreeMap<u32, EpochState>,
    /// The epochs whose AMK content key is considered locally delivered. `has_amk` reads this,
    /// modelling the pending window: a commit can attest an epoch (bumping the ceiling) before its
    /// in-band [`AlbumKeyDistribution`] broadcast arrives locally.
    amk_held: BTreeSet<u32>,
    /// The monotonic epoch ceiling (`= max attested amk_version`).
    ceiling: u32,
    /// The write-tier keypair minted for a [staged](Self::stage_self_update) (not-yet-merged)
    /// self-update commit. Consumed by [`merge_pending`](Self::merge_pending) (won) or dropped by
    /// [`discard_pending_and_process`](Self::discard_pending_and_process) (lost). Transient — not
    /// persisted (a pending commit does not survive a restart either).
    staged_write_tier: Option<HybridSigningKey>,
    // ── S-X3: upgrade ceremony + resilience state ────────────────────────────
    /// If this group is a **fork** produced by an upgrade ceremony, the continuity pointer back to
    /// the album it forked from (the manifest `upgraded_from` field). `None` for an original album.
    upgraded_from: Option<UpgradeLineage>,
    /// Set once this album has been **tombstoned** (frozen by an `AlbumTombstone` commit) — carries
    /// the upgrade `intent_id`. A tombstoned album refuses new write ceremonies (see
    /// [`ensure_not_tombstoned`](Self::ensure_not_tombstoned)); reads/`verify_asset` are unaffected.
    tombstoned: Option<Uuid>,
    /// Upgrade **quiescence**: set on issuing/receiving an [`UpgradeIntent`], so a second intent
    /// under a *different* `intent_id` is rejected (only one upgrade in flight) and new writes are
    /// queued locally rather than sent.
    quiescence: Option<Quiescence>,
    /// Writes queued locally during upgrade quiescence (`pending_until_upgrade`), replayed into the
    /// fork after cutover. Opaque encoded payloads — the caller owns the manifest re-encode against
    /// `to_version` (an application-layer concern outside this authority).
    pending_writes: Vec<Vec<u8>>,
    /// Idempotency ledger of completed ceremony `intent_id`s (re-key, upgrade fork). A duplicate
    /// intent is a no-op — the crash-resume guarantee both ceremonies share.
    completed_intents: BTreeSet<Uuid>,
    /// An in-flight re-keying ceremony's resume state (two-phase: commit, then broadcast). Persisted
    /// so a crash between the two phases resumes on restart.
    rekey_pending: Option<RekeyState>,
}

impl OpenMlsAuthority {
    // ── Construction ────────────────────────────────────────────────────────

    /// Found a new album: create the MLS group for `album_id` with `identity` as its first
    /// (admin) member, at the pinned X-Wing ciphersuite, and attest the genesis epoch
    /// (`amk_version = 1`, MLS epoch 0). Mints the epoch-1 write-tier key and AMK; the AMK is held
    /// locally (the founder authored the epoch). `history_policy` is fixed for the album here.
    #[tracing::instrument(skip_all, fields(album_id = %album_id, ciphersuite = PINNED_CIPHERSUITE_ID, ?history_policy))]
    pub fn create_album(
        identity: MlsDeviceIdentity,
        album_id: Uuid,
        history_policy: HistoryPolicy,
    ) -> Result<Self> {
        identity.store_signer()?;
        let credential = identity.credential_with_key()?;
        let group = MlsGroup::builder()
            .ciphersuite(PINNED_CIPHERSUITE)
            .use_ratchet_tree_extension(true)
            .with_group_id(GroupId::from_slice(album_id.as_bytes()))
            .build(&identity.provider, &identity.mls_signer, credential)
            .map_err(|e| OpenMlsAuthorityError::GroupCreate(format!("{e:?}")))?;

        let mut authority = Self {
            album_id,
            identity,
            group,
            history_policy,
            epochs: BTreeMap::new(),
            amk_held: BTreeSet::new(),
            ceiling: 0,
            staged_write_tier: None,
            upgraded_from: None,
            tombstoned: None,
            quiescence: None,
            pending_writes: Vec::new(),
            completed_intents: BTreeSet::new(),
            rekey_pending: None,
        };
        // The founder mints the genesis epoch's write-tier keypair (committer role).
        authority
            .ingest_current_epoch(WriteTierIngest::Minted(HybridSigningKey::generate()), true)?;
        tracing::info!(album_id = %album_id, epoch = authority.ceiling, "OpenMLS album founded");
        Ok(authority)
    }

    /// Create a fresh **single-member (self) album group** with a freshly-generated device
    /// identity and the default [`HistoryPolicy::Full`]. This is the S-X1 entry point, preserved
    /// verbatim for the offline/parity surface; multi-member ceremonies use
    /// [`create_album`](Self::create_album) with an explicit [`MlsDeviceIdentity`].
    #[tracing::instrument(skip_all, fields(album_id = %album_id, ciphersuite = PINNED_CIPHERSUITE_ID))]
    pub fn create_self_group(album_id: Uuid) -> Result<Self> {
        let identity = MlsDeviceIdentity::generate(Uuid::now_v7(), Uuid::now_v7())?;
        Self::create_album(identity, album_id, HistoryPolicy::Full)
    }

    // ── Ceremony 1/2: add member / add device ────────────────────────────────

    /// **Add** a device (a new user's device, or an existing user's new device) to the album.
    ///
    /// Verifies the KeyPackage's hybrid **LeafNode identity binding** against `joiner_directory`
    /// first — a leaf whose Ed25519 MLS key is not covered by the device DSK's Ed25519 **and**
    /// ML-DSA identity signature is rejected before any group mutation. Then runs the MLS `Add`
    /// proposal + `Commit`, advancing to a fresh epoch, and prepares the in-band key delivery:
    /// the new epoch's AMK for existing members, and the joiner's history batch per the album's
    /// fixed [`HistoryPolicy`] (never a per-add choice — SSoT: MLS § History Delivery).
    #[tracing::instrument(skip_all, fields(album_id = %self.album_id, from_epoch = self.ceiling))]
    pub fn add_member(
        &mut self,
        key_package: openmls::prelude::KeyPackage,
        joiner_directory: &DeviceDirectory,
    ) -> Result<AddOutcome> {
        self.ensure_not_tombstoned()?;
        // Gate: the joining leaf must be identity-bound to a device in the joiner's directory.
        let (user_id, device_id) = identity::verify_leaf_binding(
            key_package.leaf_node().credential(),
            key_package.leaf_node().signature_key().as_slice(),
            joiner_directory,
        )?;

        let prior_ceiling = self.ceiling;
        // Mint the fresh epoch's write-tier keypair and attest its public half in the commit's
        // authenticated AAD (the admin-signed chain), before building the commit.
        let minted = HybridSigningKey::generate();
        self.set_write_tier_aad(&minted)?;
        let (commit, welcome, _group_info) = self
            .group
            .add_members(
                &self.identity.provider,
                &self.identity.mls_signer,
                &[key_package],
            )
            .map_err(|e| OpenMlsAuthorityError::AddMember(format!("{e:?}")))?;
        self.group
            .merge_pending_commit(&self.identity.provider)
            .map_err(|e| OpenMlsAuthorityError::AddMember(format!("merge: {e:?}")))?;

        // The add commit advanced the epoch; the committer holds the new AMK + write-tier key.
        let new_version = self.ingest_current_epoch(WriteTierIngest::Minted(minted), true)?;

        // Key delivery for the fresh epoch: the AMK broadcast (read, all members), the write-tier
        // private half (write, the writers set — includes the joiner today), and the joiner's
        // history batch (read-only prior epochs it is entitled to).
        let key_delivery = vec![
            self.build_key_distribution(new_version)?,
            self.build_write_tier_distribution(new_version)?,
            self.build_history_bundle(new_version)?,
        ];

        tracing::info!(
            album_id = %self.album_id, %user_id, %device_id,
            from_epoch = prior_ceiling, to_epoch = new_version.0,
            "OpenMLS member added"
        );
        Ok(AddOutcome {
            commit,
            welcome,
            key_delivery,
        })
    }

    // ── Ceremony 3: remove member / remove device ────────────────────────────

    /// **Remove** a device from the album by its leaf index and re-key: the MLS `Remove` +
    /// `Commit` advances to a fresh epoch whose new AMK and write-tier key the removed device does
    /// not hold, so it cannot read or write future epochs. The removed device cannot even process
    /// the commit (it is evicted). Prepares the fresh AMK's [`AlbumKeyDistribution`] for the
    /// remaining members (SSoT: MLS § Remove user).
    #[tracing::instrument(skip_all, fields(album_id = %self.album_id, from_epoch = self.ceiling, leaf = leaf.u32()))]
    pub fn remove_member(&mut self, leaf: LeafNodeIndex) -> Result<RemoveOutcome> {
        self.remove_leaves(&[leaf])
    }

    /// The one remove ceremony, over **one or more** leaves in a single `Remove` + `Commit`.
    ///
    /// A per-device removal passes one leaf; a [per-user block](Self::block_user) passes all of
    /// that user's leaves, which is what the design's "MLS `Remove` proposal + `Commit` removing
    /// **all** Charlie's devices" means — one commit, therefore exactly **one** epoch bump, not
    /// one per device. Both spellings share this body so there is a single re-key path to reason
    /// about.
    #[tracing::instrument(skip_all, fields(album_id = %self.album_id, from_epoch = self.ceiling, leaves = leaves.len()))]
    fn remove_leaves(&mut self, leaves: &[LeafNodeIndex]) -> Result<RemoveOutcome> {
        self.ensure_not_tombstoned()?;
        // Mint the re-keyed epoch's write-tier keypair; the removed member never receives its
        // private half (it cannot even decrypt the distribution — it is evicted by the commit).
        let minted = HybridSigningKey::generate();
        self.set_write_tier_aad(&minted)?;
        let (commit, _welcome, _group_info) = self
            .group
            .remove_members(&self.identity.provider, &self.identity.mls_signer, leaves)
            .map_err(|e| OpenMlsAuthorityError::RemoveMember(format!("{e:?}")))?;
        self.group
            .merge_pending_commit(&self.identity.provider)
            .map_err(|e| OpenMlsAuthorityError::RemoveMember(format!("merge: {e:?}")))?;

        let new_version = self.ingest_current_epoch(WriteTierIngest::Minted(minted), true)?;
        let key_delivery = vec![
            self.build_key_distribution(new_version)?,
            self.build_write_tier_distribution(new_version)?,
        ];
        tracing::info!(album_id = %self.album_id, to_epoch = new_version.0, removed = leaves.len(), "OpenMLS member removed + re-keyed");
        Ok(RemoveOutcome {
            commit,
            key_delivery,
        })
    }

    /// The leaf index of the member whose LeafNode binding names `device_id`, or `None`.
    /// Convenience for driving a [`remove_member`](Self::remove_member) by device rather than by
    /// raw leaf index.
    pub fn leaf_index_of_device(&self, device_id: Uuid) -> Option<LeafNodeIndex> {
        self.group.members().find_map(|m| {
            let binding =
                identity::LeafBinding::from_credential_bytes(m.credential.serialized_content())
                    .ok()?;
            (binding.core.device_id == device_id).then_some(m.index)
        })
    }

    /// Every leaf index whose LeafNode binding names `user_id` — a user's **whole device set** in
    /// this album, which is the unit a per-user block removes. Sorted, so the removal is
    /// deterministic; empty when the user is not a member.
    ///
    /// Reads the same hybrid identity binding [`leaf_index_of_device`](Self::leaf_index_of_device)
    /// does, so a leaf that never passed `verify_leaf_binding` cannot smuggle in a `user_id`.
    pub fn leaf_indices_of_user(&self, user_id: Uuid) -> Vec<LeafNodeIndex> {
        let mut leaves: Vec<LeafNodeIndex> = self
            .group
            .members()
            .filter_map(|m| {
                let binding =
                    identity::LeafBinding::from_credential_bytes(m.credential.serialized_content())
                        .ok()?;
                (binding.core.user_id == user_id).then_some(m.index)
            })
            .collect();
        leaves.sort_by_key(LeafNodeIndex::u32);
        leaves
    }

    // ── Ceremony 3b: per-user block (moderation) ─────────────────────────────

    /// **Block a user** from this album (`S-X4`): remove *all* of their devices in one MLS
    /// `Remove` + `Commit` and bump the AMK epoch, so the blocked user loses read access to every
    /// future epoch and their write-tier capability is rotated out from under them.
    ///
    /// This is the crypto half of the [moderation doc's per-user block]; the server-visible half
    /// (revoking the blocker's `album_share` rows) is enforced independently, so a block is
    /// complete only when both have run. Per the design, prior epochs' keys the blocked user
    /// already holds are **not** clawed back — assets they could already read stay readable, which
    /// is the same removal semantics as any [`remove_member`](Self::remove_member).
    ///
    /// Blocking a user who is not a member is a **no-op**, not an error: it reports
    /// [`BlockOutcome::already_absent`] with the unchanged epoch and no commit. That makes the
    /// ceremony idempotent, which matters because the server-side block row is itself idempotent
    /// and a retry must not burn an epoch (and a spurious epoch bump would strand every honest
    /// concurrent uploader in the pending window for nothing).
    ///
    /// Refuses to block the **local** user: a device cannot evict itself from its own group, and
    /// silently succeeding would leave the caller believing a block took effect.
    ///
    /// [moderation doc's per-user block]: https://docs/design/moderation/#blocklists
    #[tracing::instrument(skip_all, fields(album_id = %self.album_id, from_epoch = self.ceiling, blocked = %user_id))]
    pub fn block_user(&mut self, user_id: Uuid) -> Result<BlockOutcome> {
        self.ensure_not_tombstoned()?;
        if user_id == self.identity.user_id() {
            return Err(OpenMlsAuthorityError::BlockSelf(user_id));
        }

        let leaves = self.leaf_indices_of_user(user_id);
        if leaves.is_empty() {
            tracing::info!(
                album_id = %self.album_id,
                blocked = %user_id,
                "per-user block: not a member of this album; no MLS Remove, no epoch bump"
            );
            return Ok(BlockOutcome {
                user_id,
                removed_leaves: Vec::new(),
                amk_version: AmkVersion(self.ceiling),
                removal: None,
            });
        }

        let removed_leaves: Vec<u32> = leaves.iter().map(LeafNodeIndex::u32).collect();
        let removal = self.remove_leaves(&leaves)?;
        let amk_version = AmkVersion(self.ceiling);
        tracing::info!(
            album_id = %self.album_id,
            blocked = %user_id,
            devices = removed_leaves.len(),
            to_epoch = amk_version.0,
            "per-user block: every device removed in one commit; AMK epoch bumped and write-tier \
             key rotated"
        );
        Ok(BlockOutcome {
            user_id,
            removed_leaves,
            amk_version,
            removal: Some(removal),
        })
    }

    // ── Ceremony 4: self-update / scheduled rotation ─────────────────────────

    /// Advance to a fresh epoch via a self-update commit (the scheduled-rotation ceremony): the
    /// RFC 9420 ratchet moves forward (new epoch secrets ⇒ new AMK + write-tier key), the ceiling
    /// bumps by one. `amk_present` records whether this epoch's AMK is locally delivered — `true`
    /// in the committer's own view. Returns the new `amk_version`.
    ///
    /// This is the S-X1 signature, preserved. For the multi-member rotation that also broadcasts
    /// the fresh AMK to the other members, use [`rotate_epoch`](Self::rotate_epoch).
    #[tracing::instrument(skip_all, fields(album_id = %self.album_id, amk_present))]
    pub fn advance_epoch(&mut self, amk_present: bool) -> Result<AmkVersion> {
        self.ensure_not_tombstoned()?;
        let minted = HybridSigningKey::generate();
        self.set_write_tier_aad(&minted)?;
        self.group
            .self_update(
                &self.identity.provider,
                &self.identity.mls_signer,
                LeafNodeParameters::default(),
            )
            .map_err(|e| OpenMlsAuthorityError::SelfUpdate(format!("{e:?}")))?;
        self.group
            .merge_pending_commit(&self.identity.provider)
            .map_err(|e| OpenMlsAuthorityError::SelfUpdate(format!("merge: {e:?}")))?;
        let version = self.ingest_current_epoch(WriteTierIngest::Minted(minted), amk_present)?;
        tracing::info!(album_id = %self.album_id, epoch = version.0, "OpenMLS epoch advanced");
        Ok(version)
    }

    /// Scheduled rotation as a ceremony: [`advance_epoch(true)`](Self::advance_epoch) plus the
    /// commit and the fresh epoch's [`AlbumKeyDistribution`] for the other members.
    #[tracing::instrument(skip_all, fields(album_id = %self.album_id))]
    pub fn rotate_epoch(&mut self) -> Result<RemoveOutcome> {
        self.ensure_not_tombstoned()?;
        let minted = HybridSigningKey::generate();
        self.set_write_tier_aad(&minted)?;
        // The self-update stages a pending commit; capture it for the other members before merge.
        let commit = self
            .group
            .self_update(
                &self.identity.provider,
                &self.identity.mls_signer,
                LeafNodeParameters::default(),
            )
            .map(CommitMessageBundle::into_commit)
            .map_err(|e| OpenMlsAuthorityError::SelfUpdate(format!("{e:?}")))?;
        self.group
            .merge_pending_commit(&self.identity.provider)
            .map_err(|e| OpenMlsAuthorityError::SelfUpdate(format!("merge: {e:?}")))?;
        let version = self.ingest_current_epoch(WriteTierIngest::Minted(minted), true)?;
        let key_delivery = vec![
            self.build_key_distribution(version)?,
            self.build_write_tier_distribution(version)?,
        ];
        Ok(RemoveOutcome {
            commit,
            key_delivery,
        })
    }

    // ── Concurrent-commit resolution (two members commit against one epoch) ───
    //
    // These are the OpenMLS convergence primitives: a member *stages* a self-update commit
    // without merging it, then either merges it (its commit won the delivery-service ordering) or
    // discards it and processes the winning peer's commit (it lost, and rebases). This is how two
    // parallel commits against the same epoch converge with no group split. The full partition /
    // reconciliation ceremony (`ReconcileOutcome`, tombstone-fork) is slice S-X3.

    /// Stage a self-update commit **without merging** it, returning the commit for the other
    /// members. Mints (and stashes) the prospective epoch's write-tier keypair; the group holds
    /// the pending commit until [`merge_pending`](Self::merge_pending) (won) or
    /// [`discard_pending_and_process`](Self::discard_pending_and_process) (lost — the stashed
    /// keypair is dropped with the commit).
    #[tracing::instrument(skip_all, fields(album_id = %self.album_id, epoch = self.ceiling))]
    pub fn stage_self_update(&mut self) -> Result<MlsMessageOut> {
        self.ensure_not_tombstoned()?;
        let minted = HybridSigningKey::generate();
        self.set_write_tier_aad(&minted)?;
        let commit = self
            .group
            .self_update(
                &self.identity.provider,
                &self.identity.mls_signer,
                LeafNodeParameters::default(),
            )
            .map(CommitMessageBundle::into_commit)
            .map_err(|e| OpenMlsAuthorityError::SelfUpdate(format!("{e:?}")))?;
        self.staged_write_tier = Some(minted);
        Ok(commit)
    }

    /// Merge this member's own staged pending commit — the "my commit won" path. Advances the
    /// epoch, holds the fresh AMK, and installs the write-tier keypair minted at staging.
    #[tracing::instrument(skip_all, fields(album_id = %self.album_id))]
    pub fn merge_pending(&mut self) -> Result<AmkVersion> {
        let minted = self
            .staged_write_tier
            .take()
            .ok_or_else(|| OpenMlsAuthorityError::SelfUpdate("no staged commit to merge".into()))?;
        self.group
            .merge_pending_commit(&self.identity.provider)
            .map_err(|e| OpenMlsAuthorityError::SelfUpdate(format!("merge: {e:?}")))?;
        self.ingest_current_epoch(WriteTierIngest::Minted(minted), true)
    }

    /// Discard this member's own staged pending commit and process the winning peer's commit
    /// instead — the "my commit lost, rebase" path. Leaves this member converged on the peer's
    /// epoch with no divergence; the write-tier keypair minted at staging is dropped (its epoch
    /// never existed on the chain).
    #[tracing::instrument(skip_all, fields(album_id = %self.album_id))]
    pub fn discard_pending_and_process(
        &mut self,
        winning_commit: MlsMessageIn,
    ) -> Result<AmkVersion> {
        self.staged_write_tier = None;
        self.group
            .clear_pending_commit(<CapsuleMlsProvider as OpenMlsProvider>::storage(
                &self.identity.provider,
            ))
            .map_err(|e| OpenMlsAuthorityError::ProcessMessage(format!("clear pending: {e:?}")))?;
        self.process_commit(winning_commit)
    }

    // ── Receive side: process a peer's commit / key delivery ─────────────────

    /// Process a peer's commit (add / remove / self-update) delivered over the group channel:
    /// merge it, advancing this member's view to the new epoch. The commit's authenticated AAD
    /// attests the fresh epoch's write-tier **public** key (installed immediately, so
    /// `verify_asset` can check the epoch's manifests); the AMK and the write-tier **private**
    /// half are both **pending** until their [`AlbumKeyDistribution`] / [`WriteTierDistribution`]
    /// deliveries arrive via [`process_key_delivery`](Self::process_key_delivery). Returns the new
    /// `amk_version`.
    ///
    /// A member removed by this commit is evicted: OpenMLS marks the group inactive and this
    /// returns the merged (now-inactive) state.
    #[tracing::instrument(skip_all, fields(album_id = %self.album_id, from_epoch = self.ceiling))]
    pub fn process_commit(&mut self, message: MlsMessageIn) -> Result<AmkVersion> {
        let protocol = protocol_message(message)?;
        let processed = self
            .group
            .process_message(&self.identity.provider, protocol)
            .map_err(|e| OpenMlsAuthorityError::ProcessMessage(format!("{e:?}")))?;
        // The committer's write-tier attestation rides the commit's authenticated data — covered
        // by the committer's leaf signature, validated with the commit by OpenMLS.
        let aad = processed.aad().to_vec();
        match processed.into_content() {
            ProcessedMessageContent::StagedCommitMessage(staged) => {
                let attestation = match parse_commit_aad(&aad)? {
                    CommitAad::WriteTier(a) => a,
                    CommitAad::Tombstone { .. } => {
                        return Err(OpenMlsAuthorityError::Upgrade(
                            "AlbumTombstone commit must be processed via process_tombstone".into(),
                        ));
                    }
                };
                let staged: StagedCommit = *staged;
                self.group
                    .merge_staged_commit(&self.identity.provider, staged)
                    .map_err(|e| OpenMlsAuthorityError::ProcessMessage(format!("merge: {e:?}")))?;
                // A member removed by this commit is now inactive; its exporter is unavailable.
                if !self.group.is_active() {
                    tracing::info!(album_id = %self.album_id, "OpenMLS: evicted by processed commit");
                    return Err(OpenMlsAuthorityError::ProcessMessage(
                        "evicted from group by this commit".into(),
                    ));
                }
                let version = self.ingest_current_epoch(
                    WriteTierIngest::Attested(attestation.write_tier_pub),
                    false,
                )?;
                if attestation.amk_version != version.0 {
                    return Err(OpenMlsAuthorityError::ProcessMessage(format!(
                        "commit attests write-tier for epoch {}, but advances the group to {}",
                        attestation.amk_version, version.0
                    )));
                }
                tracing::debug!(album_id = %self.album_id, epoch = version.0, "OpenMLS peer commit merged (AMK + write-tier private pending)");
                Ok(version)
            }
            other => Err(OpenMlsAuthorityError::UnexpectedMessage(format!(
                "expected a commit, got {}",
                describe_content(&other)
            ))),
        }
    }

    /// Apply an in-band key-delivery application message — an [`AlbumKeyDistribution`] (a single
    /// epoch's AMK, flipping it from pending to present), a [`WriteTierDistribution`] (the epoch's
    /// write-tier private half, granting signing capability), or an [`AlbumHistoryBundle`] (a
    /// joiner's prior-epoch batch). This is the vehicle that closes the pending windows.
    #[tracing::instrument(skip_all, fields(album_id = %self.album_id))]
    pub fn process_key_delivery(&mut self, message: MlsMessageIn) -> Result<()> {
        let protocol = protocol_message(message)?;
        let processed = self
            .group
            .process_message(&self.identity.provider, protocol)
            .map_err(|e| OpenMlsAuthorityError::ProcessMessage(format!("{e:?}")))?;
        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(app) => {
                match MlsAppPayload::from_bytes(&app.into_bytes())? {
                    MlsAppPayload::KeyDistribution(kd) => self.apply_key_distribution(&kd),
                    MlsAppPayload::WriteTier(wt) => self.apply_write_tier_distribution(&wt),
                    MlsAppPayload::History(bundle) => {
                        for entry in &bundle.entries {
                            self.apply_history_entry(entry);
                        }
                        Ok(())
                    }
                    MlsAppPayload::Upgrade(_) => Err(OpenMlsAuthorityError::Upgrade(
                        "upgrade intent must be processed via receive_upgrade_intent".into(),
                    )),
                }
            }
            other => Err(OpenMlsAuthorityError::UnexpectedMessage(format!(
                "expected an application message, got {}",
                describe_content(&other)
            ))),
        }
    }

    // ── Ceremony 4 (join side): adopt a Welcome ──────────────────────────────

    /// Join an album as a new member by processing a `Welcome`. Adopts the group, learns the
    /// album's **current epoch as its monotonic ceiling from the commit chain** (never the
    /// server), derives + holds the current epoch's AMK, then applies the committer's
    /// `history_messages` (the [`AlbumHistoryBundle`] and current-epoch [`AlbumKeyDistribution`])
    /// to populate the prior epochs its [`HistoryPolicy`] entitles it to.
    ///
    /// `history_policy` is the album's fixed policy, communicated to the joiner with the invite;
    /// binding it into the group's context extension so it is cryptographically album-fixed (not
    /// merely conveyed) is a follow-up (see module docs).
    #[tracing::instrument(skip_all, fields(ciphersuite = PINNED_CIPHERSUITE_ID))]
    pub fn join_via_welcome(
        identity: MlsDeviceIdentity,
        welcome_message: MlsMessageIn,
        history_messages: Vec<MlsMessageIn>,
        history_policy: HistoryPolicy,
    ) -> Result<Self> {
        // `into_welcome` is `test-utils`-gated upstream; `extract` (the public body accessor) is not.
        let MlsMessageBodyIn::Welcome(welcome) = welcome_message.extract() else {
            return Err(OpenMlsAuthorityError::Welcome(
                "message is not a Welcome".into(),
            ));
        };
        let join_config = MlsGroupJoinConfig::builder()
            .use_ratchet_tree_extension(true)
            .build();
        let staged =
            StagedWelcome::new_from_welcome(&identity.provider, &join_config, welcome, None)
                .map_err(|e| OpenMlsAuthorityError::Welcome(format!("{e:?}")))?;
        let group = staged
            .into_group(&identity.provider)
            .map_err(|e| OpenMlsAuthorityError::Welcome(format!("into_group: {e:?}")))?;

        let album_id = uuid_from_group_id(group.group_id())?;
        let mut authority = Self {
            album_id,
            identity,
            group,
            history_policy,
            epochs: BTreeMap::new(),
            amk_held: BTreeSet::new(),
            ceiling: 0,
            staged_write_tier: None,
            upgraded_from: None,
            tombstoned: None,
            quiescence: None,
            pending_writes: Vec::new(),
            completed_intents: BTreeSet::new(),
            rekey_pending: None,
        };
        // The joiner is a full member at the current epoch: derive + hold its AMK. This also sets
        // the monotonic ceiling from the (chain-attested) group epoch. The epoch's write-tier key
        // material is deferred — both halves arrive with the committer's `WriteTierDistribution`
        // in `history_messages` (a joiner has no signing capability until it does).
        let current = authority.ingest_current_epoch(WriteTierIngest::Deferred, true)?;
        tracing::info!(album_id = %album_id, epoch = current.0, "OpenMLS joined via Welcome");

        for message in history_messages {
            authority.process_key_delivery(message)?;
        }
        Ok(authority)
    }

    // ── Write-path guard ─────────────────────────────────────────────────────

    /// Refuse a write-producing ceremony on a [tombstoned](Self::is_tombstoned) (frozen) album. The
    /// album has been upgraded; all new activity belongs on the fork. Reads are never gated here —
    /// only commit-producing entry points call this, so `verify_asset` stays byte-for-byte untouched.
    fn ensure_not_tombstoned(&self) -> Result<()> {
        match self.tombstoned {
            Some(intent_id) => Err(OpenMlsAuthorityError::Tombstoned(intent_id)),
            None => Ok(()),
        }
    }

    // ── Epoch ingestion + derivation ─────────────────────────────────────────

    /// Record the group's *current* epoch as `amk_version = mls_epoch + 1`: export its AMK from
    /// the live MLS epoch secrets, install its write-tier key material per how it arrived
    /// ([`WriteTierIngest`]), and raise the ceiling. `amk_present` marks whether this member holds
    /// the AMK content key locally (the pending window seam).
    fn ingest_current_epoch(
        &mut self,
        write_tier: WriteTierIngest,
        amk_present: bool,
    ) -> Result<AmkVersion> {
        let version = u32::try_from(self.group.epoch().as_u64() + 1).map_err(|_| {
            OpenMlsAuthorityError::Export("mls epoch overflows u32 amk_version".into())
        })?;
        let (write_tier_pub, write_tier_priv) = match write_tier {
            WriteTierIngest::Minted(key) => (Some(key.verifying_key()), Some(key)),
            WriteTierIngest::Attested(pub_key) => (Some(pub_key), None),
            WriteTierIngest::Deferred => (None, None),
        };
        let amk = self.export_current_amk()?;
        self.epochs.insert(
            version,
            EpochState {
                write_tier_pub,
                write_tier_priv,
                amk,
            },
        );
        if amk_present {
            self.amk_held.insert(version);
        }
        self.ceiling = self.ceiling.max(version);
        Ok(AmkVersion(version))
    }

    /// Set the pending commit's authenticated AAD to the [`WriteTierAttestation`] for the epoch
    /// the commit will advance the group to (`current mls_epoch + 2` in `amk_version` terms:
    /// current version is `epoch + 1`, the commit bumps it by one). Must be called immediately
    /// before the commit-building operation — OpenMLS consumes and resets the AAD per operation.
    fn set_write_tier_aad(&mut self, minted: &HybridSigningKey) -> Result<()> {
        self.set_commit_aad(&CommitAad::WriteTier(
            self.next_write_tier_attestation(minted)?,
        ))
    }

    /// The [`WriteTierAttestation`] for the epoch the next commit will advance the group to
    /// (`current mls_epoch + 2` in `amk_version` terms).
    fn next_write_tier_attestation(
        &self,
        minted: &HybridSigningKey,
    ) -> Result<WriteTierAttestation> {
        let next_version = u32::try_from(self.group.epoch().as_u64() + 2).map_err(|_| {
            OpenMlsAuthorityError::Export("mls epoch overflows u32 amk_version".into())
        })?;
        Ok(WriteTierAttestation {
            amk_version: next_version,
            write_tier_pub: minted.verifying_key(),
        })
    }

    /// Encode a [`CommitAad`] into the group's pending-commit authenticated data. Must be called
    /// immediately before the commit-building operation — OpenMLS consumes and resets the AAD per
    /// operation.
    fn set_commit_aad(&mut self, aad: &CommitAad) -> Result<()> {
        let bytes = crate::cbor::to_canonical_vec(aad)
            .map_err(|e| OpenMlsAuthorityError::Message(format!("commit aad encode: {e}")))?;
        self.group.set_aad(bytes);
        Ok(())
    }

    /// Export the AMK content key for the group's **current** epoch from the RFC 9420 exporter.
    /// Deterministic for a fixed epoch; the album id is the exporter context so two albums never
    /// collide at the same epoch.
    pub fn export_current_amk(&self) -> Result<[u8; AMK_LEN]> {
        let bytes = self
            .group
            .export_secret(
                self.identity.provider.crypto(),
                AMK_EXPORT_LABEL,
                self.album_id.as_bytes(),
                AMK_LEN,
            )
            .map_err(|e| OpenMlsAuthorityError::Export(format!("{e:?}")))?;
        bytes
            .try_into()
            .map_err(|v: Vec<u8>| OpenMlsAuthorityError::AmkLength(v.len()))
    }

    // ── Key-delivery construction + application ──────────────────────────────

    /// Build the steady-state [`AlbumKeyDistribution`] application message for `version`.
    fn build_key_distribution(&mut self, version: AmkVersion) -> Result<MlsMessageOut> {
        let amk = self
            .epochs
            .get(&version.0)
            .ok_or_else(|| {
                OpenMlsAuthorityError::Message(format!("no AMK for epoch {}", version.0))
            })?
            .amk;
        let payload = MlsAppPayload::KeyDistribution(AlbumKeyDistribution {
            amk_version: version.0,
            amk_bytes: amk,
        });
        self.create_app_message(&payload)
    }

    /// Build the [`WriteTierDistribution`] application message delivering `version`'s write-tier
    /// **private** half to the epoch's writers — the only vehicle by which signing capability
    /// exists (SSoT: [Keys — AMKs], "distributed via MLS to writers only").
    ///
    /// **Writer-set seam:** the recipient set is [`writers()`](Self::writers). Today that is every
    /// member, so the delivery is a single group-encrypted application message; when the roles
    /// model lands, a proper subset forces per-writer delivery (e.g. HPKE to each writer's leaf
    /// encryption key) instead of the group channel — the seam is this method's send-path, not the
    /// message shape.
    ///
    /// [Keys — AMKs]: https://docs/design/cryptography/keys/#album-master-keys-amks
    fn build_write_tier_distribution(&mut self, version: AmkVersion) -> Result<MlsMessageOut> {
        let seed = self
            .epochs
            .get(&version.0)
            .and_then(|e| e.write_tier_priv.as_ref())
            .ok_or_else(|| {
                OpenMlsAuthorityError::Message(format!(
                    "no write-tier private key held for epoch {}",
                    version.0
                ))
            })?
            .to_seed_bytes();
        debug_assert_eq!(
            self.writers().len(),
            self.member_count(),
            "roles model not in core yet: the writer set must be all members, so the group \
             channel is a faithful delivery of the writers-only message"
        );
        let payload = MlsAppPayload::WriteTier(WriteTierDistribution {
            amk_version: version.0,
            write_tier_seed: seed.to_vec(),
        });
        self.create_app_message(&payload)
    }

    /// Build the joiner's [`AlbumHistoryBundle`]: the prior epochs (strictly below `current`) the
    /// album's [`HistoryPolicy`] entitles a joiner to, each with its AMK and write-tier public key
    /// (read-only — prior epochs never carry the private half; nobody signs new writes under an
    /// old epoch).
    fn build_history_bundle(&mut self, current: AmkVersion) -> Result<MlsMessageOut> {
        let range = self.history_policy.entitled_range(current.0);
        let entries = range
            .filter(|v| *v < current.0)
            .filter_map(|v| {
                self.epochs.get(&v).and_then(|e| {
                    e.write_tier_pub.as_ref().map(|pub_key| AmkHistoryEntry {
                        amk_version: v,
                        amk_bytes: e.amk,
                        write_tier_pub: pub_key.clone(),
                    })
                })
            })
            .collect();
        let payload = MlsAppPayload::History(AlbumHistoryBundle { entries });
        self.create_app_message(&payload)
    }

    fn create_app_message(&mut self, payload: &MlsAppPayload) -> Result<MlsMessageOut> {
        let bytes = payload.to_bytes()?;
        self.group
            .create_message(&self.identity.provider, &self.identity.mls_signer, &bytes)
            .map_err(|e| OpenMlsAuthorityError::Message(format!("create app message: {e:?}")))
    }

    /// Apply a received [`AlbumKeyDistribution`]: mark the epoch's AMK present. The epoch must
    /// already be attested (its commit processed) — the pending window always closes *after* the
    /// commit that opened it — and the delivered bytes must match what this member derived, or the
    /// delivery is a forgery/inconsistency and is rejected.
    fn apply_key_distribution(&mut self, kd: &AlbumKeyDistribution) -> Result<()> {
        match self.epochs.get(&kd.amk_version) {
            Some(state) => {
                if state.amk != kd.amk_bytes {
                    return Err(OpenMlsAuthorityError::Message(format!(
                        "delivered AMK for epoch {} disagrees with derived AMK",
                        kd.amk_version
                    )));
                }
                self.amk_held.insert(kd.amk_version);
                Ok(())
            }
            None => Err(OpenMlsAuthorityError::Message(format!(
                "AMK delivery for unattested epoch {} (commit not yet processed)",
                kd.amk_version
            ))),
        }
    }

    /// Apply a received [`WriteTierDistribution`]: install the epoch's write-tier private half,
    /// granting signing capability. The epoch must already be attested (its commit processed), and
    /// when the commit's AAD attested a public key, the delivered private half must derive exactly
    /// that key — a mismatch is a forgery/inconsistency and is rejected.
    fn apply_write_tier_distribution(&mut self, wt: &WriteTierDistribution) -> Result<()> {
        let key = HybridSigningKey::from_seed64(&wt.seed64()?);
        match self.epochs.get_mut(&wt.amk_version) {
            Some(state) => {
                let derived_pub = key.verifying_key();
                if let Some(attested) = &state.write_tier_pub {
                    if *attested != derived_pub {
                        return Err(OpenMlsAuthorityError::Message(format!(
                            "write-tier delivery for epoch {} disagrees with the chain-attested \
                             public key",
                            wt.amk_version
                        )));
                    }
                } else {
                    // Joiner path: the Welcome carried no attestation; adopt the delivered key's
                    // public half (same committer trust as the AMK history batch).
                    state.write_tier_pub = Some(derived_pub);
                }
                state.write_tier_priv = Some(key);
                Ok(())
            }
            None => Err(OpenMlsAuthorityError::Message(format!(
                "write-tier delivery for unattested epoch {} (commit not yet processed)",
                wt.amk_version
            ))),
        }
    }

    /// Apply a received history entry: install a prior epoch's read-only state (AMK + write-tier
    /// public key), marking it present. Skips epochs the member already holds derived (idempotent)
    /// and any entry at/above the current ceiling (history is strictly prior).
    fn apply_history_entry(&mut self, entry: &AmkHistoryEntry) {
        if entry.amk_version >= self.ceiling || self.epochs.contains_key(&entry.amk_version) {
            // Current-or-future epoch (derived by the joiner) or already known — nothing to add.
            self.amk_held.insert(entry.amk_version);
            return;
        }
        self.epochs.insert(
            entry.amk_version,
            EpochState {
                write_tier_pub: Some(entry.write_tier_pub.clone()),
                write_tier_priv: None,
                amk: entry.amk_bytes,
            },
        );
        self.amk_held.insert(entry.amk_version);
    }

    // ── Accessors (S-X1 surface, preserved) ──────────────────────────────────

    /// This device's MLS participation identity.
    pub fn identity(&self) -> &MlsDeviceIdentity {
        &self.identity
    }

    /// The album's fixed history policy.
    pub fn history_policy(&self) -> HistoryPolicy {
        self.history_policy
    }

    /// The number of members currently in the group.
    pub fn member_count(&self) -> usize {
        self.group.members().count()
    }

    /// The epoch's **writer set** — the recipients of a [`WriteTierDistribution`].
    ///
    /// **Roles seam:** core has no roles model yet, so every member is a writer and this returns
    /// all leaves — which is what makes the group-encrypted application channel a faithful
    /// "writers-only" delivery today. When the roles model lands, this filter narrows to the
    /// role-holding leaves and [`build_write_tier_distribution`](Self::build_write_tier_distribution)'s
    /// send-path switches to per-writer delivery (the group channel is readable by every member,
    /// so a proper subset cannot ride it).
    pub fn writers(&self) -> Vec<LeafNodeIndex> {
        self.group.members().map(|m| m.index).collect()
    }

    /// The cached AMK content key for `epoch`, or `None` if this member holds no state for it.
    pub fn amk(&self, epoch: AmkVersion) -> Option<[u8; AMK_LEN]> {
        self.epochs.get(&epoch.0).map(|e| e.amk)
    }

    /// Mark an epoch's AMK content key as locally delivered — mirrors
    /// [`ReferenceAuthority::mark_amk_present`](super::ReferenceAuthority::mark_amk_present).
    pub fn mark_amk_present(&mut self, epoch: AmkVersion) {
        if self.epochs.contains_key(&epoch.0) {
            self.amk_held.insert(epoch.0);
        }
    }

    /// The write-tier **signing** key for `epoch` — the private half a writer signs manifests
    /// with. `None` unless this member minted the key (committer) or received the epoch's
    /// [`WriteTierDistribution`]: signing capability is **never derivable from group state** —
    /// no distribution, no sign-capable handle. Also `None` for a read-only prior epoch delivered
    /// through a join-time history bundle.
    pub fn write_tier_signing_key(&self, epoch: AmkVersion) -> Option<&HybridSigningKey> {
        self.epochs
            .get(&epoch.0)
            .and_then(|e| e.write_tier_priv.as_ref())
    }

    /// The current (ceiling) epoch's write-tier signing key — the one new writes are signed under.
    ///
    /// Panics if this member holds no signing capability for the current epoch (its
    /// [`WriteTierDistribution`] has not arrived) — committer-side convenience; a receive-side
    /// caller must use [`write_tier_signing_key`](Self::write_tier_signing_key) and treat `None`
    /// as "cannot write yet" (pending, analogous to `has_amk`).
    pub fn current_write_tier(&self) -> &HybridSigningKey {
        self.epochs
            .get(&self.ceiling)
            .and_then(|e| e.write_tier_priv.as_ref())
            .expect("current epoch write-tier distribution not yet received: no signing capability")
    }

    /// The live MLS group epoch (`u64`). `amk_version` is always this plus one.
    pub fn mls_epoch(&self) -> u64 {
        self.group.epoch().as_u64()
    }

    /// The ciphersuite the group is actually running — asserted `== PINNED_CIPHERSUITE` in tests.
    pub fn ciphersuite(&self) -> Ciphersuite {
        self.group.ciphersuite()
    }

    // ── Durable persistence (export / import) ────────────────────────────────

    /// Export the full durable group state to a self-contained byte blob: the OpenMLS storage
    /// keyspace (ratchet tree, epoch secrets, queued proposals, the leaf signer, key-package
    /// bundles) plus Capsule's epoch ledger and this device's identity. Round-trips through
    /// [`import_state`](Self::import_state). *Where* the blob is stored (a `library.sqlite` row, a
    /// file) is the caller's lifecycle concern.
    #[tracing::instrument(skip_all, fields(album_id = %self.album_id, epoch = self.ceiling))]
    pub fn export_state(&self) -> Result<Vec<u8>> {
        let epochs = self
            .epochs
            .iter()
            .map(|(version, state)| PersistedEpoch {
                version: *version,
                write_tier_priv_seed64: state
                    .write_tier_priv
                    .as_ref()
                    .map(|k| serde_bytes::ByteBuf::from(k.to_seed_bytes().to_vec())),
                write_tier_pub: state.write_tier_pub.clone(),
                amk: state.amk,
                held: self.amk_held.contains(version),
            })
            .collect();
        let persisted = PersistedState {
            album_id: self.album_id,
            history_policy: self.history_policy,
            user_id: self.identity.user_id,
            device_id: self.identity.device_id,
            dsk_seed64: serde_bytes::ByteBuf::from(self.identity.dsk.to_seed_bytes().to_vec()),
            // Only the *public* MLS signer key is persisted here; the keypair itself lives in the
            // OpenMLS storage blob (it was `store`d on create/join), so it round-trips there and is
            // read back with `SignatureKeyPair::read` on import (the `private()` accessor is
            // `test-utils`-gated upstream, so we never touch the private bytes directly).
            mls_signer_public: serde_bytes::ByteBuf::from(
                self.identity.mls_signer.public().to_vec(),
            ),
            group_id: serde_bytes::ByteBuf::from(self.group.group_id().to_vec()),
            storage_blob: serde_bytes::ByteBuf::from(self.identity.provider.export_bytes()?),
            epochs,
            ceiling: self.ceiling,
            upgraded_from: self.upgraded_from.clone(),
            tombstoned: self.tombstoned,
            quiescence: self.quiescence.clone(),
            pending_writes: self
                .pending_writes
                .iter()
                .map(|w| serde_bytes::ByteBuf::from(w.clone()))
                .collect(),
            completed_intents: self.completed_intents.iter().copied().collect(),
            rekey_pending: self.rekey_pending.clone(),
        };
        crate::cbor::to_canonical_vec(&persisted)
            .map_err(|e| OpenMlsAuthorityError::Persist(format!("encode: {e}")))
    }

    /// Reconstruct an authority from an [`export_state`](Self::export_state) blob.
    #[tracing::instrument(skip_all)]
    pub fn import_state(bytes: &[u8]) -> Result<Self> {
        let persisted: PersistedState = crate::cbor::from_slice(bytes)
            .map_err(|e| OpenMlsAuthorityError::Persist(format!("decode: {e}")))?;

        let provider = CapsuleMlsProvider::import_bytes(&persisted.storage_blob)?;
        // Recover the MLS leaf signer from the storage keyspace by its public key.
        let mls_signer = SignatureKeyPair::read(
            <CapsuleMlsProvider as OpenMlsProvider>::storage(&provider),
            &persisted.mls_signer_public,
            PINNED_CIPHERSUITE.signature_algorithm(),
        )
        .ok_or_else(|| OpenMlsAuthorityError::Persist("mls signer not found in storage".into()))?;
        let identity = MlsDeviceIdentity {
            user_id: persisted.user_id,
            device_id: persisted.device_id,
            dsk: HybridSigningKey::from_seed64(&seed64(&persisted.dsk_seed64)?),
            mls_signer,
            provider,
        };
        let group = MlsGroup::load(
            <CapsuleMlsProvider as OpenMlsProvider>::storage(&identity.provider),
            &GroupId::from_slice(&persisted.group_id),
        )
        .map_err(|e| OpenMlsAuthorityError::Persist(format!("group load: {e:?}")))?
        .ok_or_else(|| OpenMlsAuthorityError::Persist("group not found in storage".into()))?;

        let mut epochs = BTreeMap::new();
        let mut amk_held = BTreeSet::new();
        for pe in persisted.epochs {
            if pe.held {
                amk_held.insert(pe.version);
            }
            let write_tier_priv = pe
                .write_tier_priv_seed64
                .map(|s| {
                    Ok::<_, OpenMlsAuthorityError>(HybridSigningKey::from_seed64(&seed64(&s)?))
                })
                .transpose()?;
            epochs.insert(
                pe.version,
                EpochState {
                    write_tier_pub: pe.write_tier_pub,
                    write_tier_priv,
                    amk: pe.amk,
                },
            );
        }
        Ok(Self {
            album_id: persisted.album_id,
            identity,
            group,
            history_policy: persisted.history_policy,
            epochs,
            amk_held,
            ceiling: persisted.ceiling,
            // A pending (staged, unmerged) commit does not survive a restart; neither does the
            // write-tier keypair minted for it.
            staged_write_tier: None,
            upgraded_from: persisted.upgraded_from,
            tombstoned: persisted.tombstoned,
            quiescence: persisted.quiescence,
            pending_writes: persisted
                .pending_writes
                .into_iter()
                .map(serde_bytes::ByteBuf::into_vec)
                .collect(),
            completed_intents: persisted.completed_intents.into_iter().collect(),
            rekey_pending: persisted.rekey_pending,
        })
    }
}

/// Convert a persisted seed byte string back into a fixed 64-byte seed pair.
fn seed64(bytes: &[u8]) -> Result<[u8; WRITE_TIER_SEED_LEN]> {
    bytes
        .try_into()
        .map_err(|_| OpenMlsAuthorityError::Persist("persisted seed is not 64 bytes".into()))
}

/// One epoch's ledger row in a [`PersistedState`].
#[derive(Serialize, Deserialize)]
struct PersistedEpoch {
    version: u32,
    write_tier_priv_seed64: Option<serde_bytes::ByteBuf>,
    write_tier_pub: Option<HybridVerifyingKey>,
    amk: [u8; AMK_LEN],
    held: bool,
}

/// The durable state blob for an [`OpenMlsAuthority`].
#[derive(Serialize, Deserialize)]
struct PersistedState {
    album_id: Uuid,
    history_policy: HistoryPolicy,
    user_id: Uuid,
    device_id: Uuid,
    dsk_seed64: serde_bytes::ByteBuf,
    mls_signer_public: serde_bytes::ByteBuf,
    group_id: serde_bytes::ByteBuf,
    storage_blob: serde_bytes::ByteBuf,
    epochs: Vec<PersistedEpoch>,
    ceiling: u32,
    // ── S-X3 ceremony state ──────────────────────────────────────────────────
    upgraded_from: Option<UpgradeLineage>,
    tombstoned: Option<Uuid>,
    quiescence: Option<Quiescence>,
    pending_writes: Vec<serde_bytes::ByteBuf>,
    completed_intents: Vec<Uuid>,
    rekey_pending: Option<RekeyState>,
}

/// Recover an album [`Uuid`] from an MLS [`GroupId`] set to the album id's 16 bytes.
fn uuid_from_group_id(group_id: &GroupId) -> Result<Uuid> {
    let bytes: [u8; 16] = group_id
        .as_slice()
        .try_into()
        .map_err(|_| OpenMlsAuthorityError::Welcome("group id is not a 16-byte album id".into()))?;
    Ok(Uuid::from_bytes(bytes))
}

/// A short human label for an unexpected processed-message content, for error diagnostics.
fn describe_content(content: &ProcessedMessageContent) -> &'static str {
    match content {
        ProcessedMessageContent::ApplicationMessage(_) => "application message",
        ProcessedMessageContent::ProposalMessage(_) => "proposal",
        ProcessedMessageContent::ExternalJoinProposalMessage(_) => "external-join proposal",
        ProcessedMessageContent::StagedCommitMessage(_) => "commit",
    }
}

/// Turn an incoming MLS message into a [`ProtocolMessage`] (a commit or application message) or a
/// typed error. Uses OpenMLS's public `try_into_protocol_message` (the `into_protocol_message`
/// convenience is `test-utils`-gated upstream).
fn protocol_message(message: MlsMessageIn) -> Result<openmls::prelude::ProtocolMessage> {
    message.try_into_protocol_message().map_err(|e| {
        OpenMlsAuthorityError::UnexpectedMessage(format!("not a protocol message: {e:?}"))
    })
}

/// Decode a commit's authenticated data into the [`CommitAad`] the committer attached, or a typed
/// error if the commit carries no valid Capsule attestation (never expected on a well-formed chain).
fn parse_commit_aad(aad: &[u8]) -> Result<CommitAad> {
    crate::cbor::from_slice(aad).map_err(|e| {
        OpenMlsAuthorityError::ProcessMessage(format!(
            "commit carries no valid Capsule attestation: {e}"
        ))
    })
}

impl AlbumAuthority for OpenMlsAuthority {
    fn album_id(&self) -> Uuid {
        self.album_id
    }

    fn epoch_ceiling(&self) -> AmkVersion {
        AmkVersion(self.ceiling)
    }

    fn write_tier_pubkey(&self, epoch: AmkVersion) -> Option<HybridVerifyingKey> {
        // `None` both for an unattested epoch and for the narrow joiner window where the current
        // epoch's write-tier delivery has not yet been applied (no attested key to verify against).
        self.epochs
            .get(&epoch.0)
            .and_then(|e| e.write_tier_pub.clone())
    }

    fn has_amk(&self, epoch: AmkVersion) -> bool {
        self.amk_held.contains(&epoch.0)
    }

    fn admin_chain_verifies(&self) -> bool {
        // The live group *is* the admin-signed commit chain: OpenMLS validated every commit as it
        // was processed, so a group we hold is trusted iff it is the pinned suite, still active
        // (not evicted), and has attested at least the genesis epoch with a consistent ceiling.
        self.group.ciphersuite() == PINNED_CIPHERSUITE
            && self.group.is_active()
            && self.ceiling >= 1
            && self.ceiling == self.epochs.keys().copied().max().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests;
