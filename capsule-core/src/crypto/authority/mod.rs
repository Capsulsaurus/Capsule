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
//! Real OpenMLS integration is deferred (its PQ ciphersuite is a non-final draft on a C
//! backend — see `DEFERRED.md`); [`ReferenceAuthority`] is a deterministic,
//! admin-signature-backed stand-in that preserves every property `verify_asset` tests for.
//! Because `verify_asset` consumes only `&dyn AlbumAuthority`, swapping in an
//! `OpenMlsAuthority` later is transparent.
//!
//! [`verify_asset`]: crate::crypto::verify_asset
//! SSoT for the rules this seam encodes: [Keys — Write Authorization].
//!
//! [Keys — Write Authorization]: https://docs/design/cryptography/keys/#write-authorization

mod reference;

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
