//! The album authorization seam — exactly what [`verify_asset`] needs to learn from MLS
//! about an album, behind a trait so the real OpenMLS group state can drop in later.
//!
//! [`verify_asset`] needs only three facts about an album, and the *authority* on all
//! three is the album's admin-signed MLS commit chain — never the server:
//! 1. the monotonic **epoch ceiling** (the highest `amk_version` the chain attests),
//! 2. the **write-tier public key** for a given epoch (only writers at that epoch held the
//!    private half), and
//! 3. whether the **AMK content key** for an epoch is *locally held* (to tell a key still
//!    in flight apart from a forged epoch).
//!
//! Two backends implement this seam:
//! - [`ReferenceAuthority`] — the deterministic, admin-signature-backed offline/test authority.
//!   It preserves every property `verify_asset` tests for with no MLS dependency, and remains
//!   available on every build (including the wasm sealing build).
//! - [`OpenMlsAuthority`] — the design-target backend over a live OpenMLS group pinned to the
//!   X-Wing ciphersuite `0x004D` (slice S-X1). Host-only, behind the `mls` feature (implied by
//!   `native`, excluded from wasm since its libcrux provider does not target wasm32).
//!
//! Because `verify_asset` consumes only `&dyn AlbumAuthority`, the two are interchangeable; the
//! [`Authority`] enum lets a caller store either without naming a concrete backend.
//!
//! [`verify_asset`]: crate::crypto::verify_asset
//! SSoT for the rules this seam encodes: [Keys — Write Authorization].
//!
//! [Keys — Write Authorization]: https://docs/design/cryptography/keys/#write-authorization

mod reference;

#[cfg(feature = "mls")]
mod openmls_authority;

#[cfg(feature = "mls")]
pub use openmls_authority::{
    AddOutcome, AlbumHistoryBundle, AlbumKeyDistribution, AmkHistoryEntry, HistoryPolicy,
    MlsDeviceIdentity, OpenMlsAuthority, OpenMlsAuthorityError, PINNED_CIPHERSUITE_ID,
    RemoveOutcome, WriteTierDistribution,
};
pub use reference::ReferenceAuthority;
use uuid::Uuid;

use crate::crypto::keys::{AmkVersion, HybridVerifyingKey};

/// One album's MLS-attested authorization state, as needed by `verify_asset`.
///
/// An instance represents a single album. All methods reflect the album's admin-signed
/// commit chain; an implementation must never let server-asserted state substitute for it.
pub trait AlbumAuthority {
    /// The album this authority speaks for.
    fn album_id(&self) -> Uuid;

    /// The monotonic epoch ceiling: the highest `amk_version` the admin chain attests. A
    /// manifest claiming a higher epoch is terminal-rejected (the server cannot fabricate
    /// a future epoch a client will honor).
    fn epoch_ceiling(&self) -> AmkVersion;

    /// The write-tier public key for `epoch`, or `None` if the chain attests no such epoch.
    /// `verify_asset` checks the manifest's `write_sig` against this key.
    fn write_tier_pubkey(&self, epoch: AmkVersion) -> Option<HybridVerifyingKey>;

    /// Whether the AMK *content key* for `epoch` is held locally. When an epoch is within
    /// the attested range but its AMK has not yet arrived over MLS, the asset is *pending*,
    /// not forged.
    fn has_amk(&self, epoch: AmkVersion) -> bool;

    /// Whether the admin-signed attestation chain itself verifies. If this is false, the
    /// authority is untrusted and `verify_asset` must terminal-reject everything — an
    /// implementation must never trust an unsigned or forged ledger.
    fn admin_chain_verifies(&self) -> bool;
}

/// A concrete album authority behind the [`AlbumAuthority`] seam, so a holder (e.g.
/// [`Workspace`](crate::lifecycle::Workspace)) can store either backend without naming a
/// concrete type. `Reference` is the offline/test default present on every build; `OpenMls` is
/// the live MLS backend, available only under the `mls` feature (host-only). The enum itself
/// implements [`AlbumAuthority`], so `&Authority` coerces to `&dyn AlbumAuthority` at every
/// `verify_asset` call site.
pub enum Authority {
    /// The offline, admin-signed epoch-ledger authority.
    Reference(Box<ReferenceAuthority>),
    /// The live OpenMLS-group authority (X-Wing suite `0x004D`).
    #[cfg(feature = "mls")]
    OpenMls(Box<OpenMlsAuthority>),
}
// Both variants are boxed: each backend owns substantial per-album state (the reference ledger,
// or a whole MLS group + libcrux provider), so indirection keeps `Authority` a single word and
// avoids a large size difference between variants.

impl Authority {
    /// The reference backend, mutably, if this is the offline/reference authority. Returns
    /// `None` for the live MLS backend — whose epoch advances are self-update commits, not
    /// ledger attestations, and so ride the MLS ceremonies (S-X2) rather than this path.
    pub fn as_reference_mut(&mut self) -> Option<&mut ReferenceAuthority> {
        match self {
            Authority::Reference(a) => Some(a.as_mut()),
            #[cfg(feature = "mls")]
            Authority::OpenMls(_) => None,
        }
    }
}

impl From<ReferenceAuthority> for Authority {
    fn from(a: ReferenceAuthority) -> Self {
        Authority::Reference(Box::new(a))
    }
}

#[cfg(feature = "mls")]
impl From<OpenMlsAuthority> for Authority {
    fn from(a: OpenMlsAuthority) -> Self {
        Authority::OpenMls(Box::new(a))
    }
}

impl AlbumAuthority for Authority {
    fn album_id(&self) -> Uuid {
        match self {
            Authority::Reference(a) => a.album_id(),
            #[cfg(feature = "mls")]
            Authority::OpenMls(a) => a.album_id(),
        }
    }

    fn epoch_ceiling(&self) -> AmkVersion {
        match self {
            Authority::Reference(a) => a.epoch_ceiling(),
            #[cfg(feature = "mls")]
            Authority::OpenMls(a) => a.epoch_ceiling(),
        }
    }

    fn write_tier_pubkey(&self, epoch: AmkVersion) -> Option<HybridVerifyingKey> {
        match self {
            Authority::Reference(a) => a.write_tier_pubkey(epoch),
            #[cfg(feature = "mls")]
            Authority::OpenMls(a) => a.write_tier_pubkey(epoch),
        }
    }

    fn has_amk(&self, epoch: AmkVersion) -> bool {
        match self {
            Authority::Reference(a) => a.has_amk(epoch),
            #[cfg(feature = "mls")]
            Authority::OpenMls(a) => a.has_amk(epoch),
        }
    }

    fn admin_chain_verifies(&self) -> bool {
        match self {
            Authority::Reference(a) => a.admin_chain_verifies(),
            #[cfg(feature = "mls")]
            Authority::OpenMls(a) => a.admin_chain_verifies(),
        }
    }
}
