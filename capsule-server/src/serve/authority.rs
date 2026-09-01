//! [`ReadAuthority`] — who may fetch a blob, and the `403` the contract asks for (`S-C39`).
//!
//! # The hole this closes
//!
//! Before this, `GET /v1/blob/{hash}` authorized on "a valid access token" and nothing else, on
//! both the Salvo surface and its Kynos port. **Any authenticated account could fetch any live
//! ciphertext whose address it could name.** That was defended as a capability model — a content
//! address is the hash of ciphertext, so producing one without holding the bytes is producing a
//! preimage — and the defence is not wrong, but it is not what the contract describes and the
//! difference is invisible until it matters. It also stacks badly: an address leaks through a
//! backup, a log, a screenshot of a debug tool, and the capability is permanent because the
//! address is.
//!
//! So the serve path now asks a question, and the question has a port.
//!
//! # Three answers, and the middle one is the whole disclosure argument
//!
//! | Answer | Status | What it tells the caller |
//! | --- | --- | --- |
//! | [`BlobReadAccess::Granted`] | `200`/`206` | the bytes |
//! | *(no variant yet — `S-C51`)* | `403` | *"you had this and you do not now"* — re-sync membership, then degrade |
//! | [`BlobReadAccess::Unrelated`] | `404` | nothing. Byte-identical to an address the server never heard of |
//!
//! **A `403` is a disclosure and a `404` is not**, which is why the boundary is drawn where it
//! is. Answering `403` to a caller with no relationship to an asset would confirm that the
//! address is referenced by *somebody* — an existence oracle over content addresses, handed out
//! to anyone who can name one. [Download & Sync] describes the `403` as the signal for an
//! authorization *change*, and a change presupposes a prior state: the caller has to be someone
//! the server can see once had access. Everyone else is told what an unknown address is told.
//!
//! # What the production authority can actually decide today, stated plainly
//!
//! [`OwnedAssetAuthority`] grants a fetch to the account the referencing asset is filed under,
//! and answers [`BlobReadAccess::Unrelated`] to everyone else. That is the whole of it, and it
//! is the whole of it because **this server has no record of album membership**:
//!
//! - Album sharing between accounts is an MLS group. The server holds no key and cannot read
//!   the roster, by design — that is the product, not a limitation of this module.
//! - The share and drop surfaces that *do* let a non-owner reach ciphertext serve it on their
//!   own routes, from their own capabilities: `/s/{id}/blob/{hash}` serves exactly the addresses
//!   its link record enumerates, and a revoked link is a `404` there. Neither of them routes
//!   through here.
//!
//! So the middle row has **no production source**, and this enum therefore does not carry a
//! variant for it and the blob route does not declare the `403`. That is the `S-C28` rule
//! applied to a status the author would have liked to have: an enum arm nothing produces and a
//! status nothing can reach are the same defect, and writing the taxonomy into a doc comment is
//! the honest way to keep the design without shipping the dead code.
//!
//! What changed is the **shape of the gap**. It was "there is no read authority", a hypothesis
//! about missing code — a plausible afternoon's work that would have been wrong. It is now
//! "there is no membership fact", a named thing the *write* path wants too:
//! `AlbumWriteAccess::Denied` has been unable to widen from owner to member since `S-C25` for
//! exactly the same reason. One fact unblocks both. Filed as `S-C51`.
//!
//! **The takedown `410` was deliberately left alone**, and it is worth saying why, because
//! owner-scoping dissolved the argument that put it there. `crate::serve` justifies collapsing a
//! serving hold into `410` partly on the grounds that a distinguishable answer would make the
//! path a moderation oracle *for an anonymous fetcher* — and after `S-C39` the only caller who
//! can reach a held asset's blob is the account that owns it, so there is no anonymous fetcher
//! left to protect from. A `403` would arguably serve that owner better: a takedown is
//! reversible, the bytes are untouched, and `410` tells a client to degrade permanently. It is
//! not changed here because design/moderation.md states the per-surface rule as *"takedown of
//! known content → `410`"* and changing a landed, tested contract on an inference is not this
//! slice's to do. Recorded on `S-C39` for whoever owns that question.
//!
//! [Download & Sync]: ../../../capsule-docs/src/content/docs/design/import/download-sync.md

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::index::BlobReference;
use crate::store::OwnerId;

/// The future a read-authority question returns.
///
/// Boxed for the same reason every store port's is: the authority is held as
/// `Arc<dyn ReadAuthority>` so the serving module is not generic over who decides.
pub type ReadAuthorityFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ReadAuthorityError>> + Send + 'a>>;

/// A collaborator could not answer, so nothing was decided.
///
/// Deliberately not a refusal: an authority that answered "denied" when it could not reach its
/// store would make an outage indistinguishable from a revocation, and the client actions for
/// those are opposite — retry versus re-sync and degrade.
#[derive(Debug, thiserror::Error)]
#[error("the read authority could not decide: {detail}")]
pub struct ReadAuthorityError {
    /// What went wrong, for the log line.
    detail: String,
}

impl ReadAuthorityError {
    /// A collaborator could not answer.
    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

/// Whether a caller may fetch the bytes behind a reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobReadAccess {
    /// Serve them.
    Granted,
    /// The caller has no relationship to the asset the server can see.
    ///
    /// Rendered as `404`, byte-identical to an address nothing references — which is the point.
    Unrelated,
}

/// Who may read a blob.
///
/// A port rather than a function on the serve context, for the reason
/// [`WriteAuthority`](crate::upload::WriteAuthority) is one: the facts it decides from live in
/// stores that will grow (membership, federation), and a serving path that reached into them
/// directly would have to grow with them.
pub trait ReadAuthority: fmt::Debug + Send + Sync {
    /// May `caller` fetch the bytes `reference` names?
    ///
    /// Takes the whole reference rather than an asset id so the decision comes from the same
    /// read that found it. An authority that re-looked-up the asset would open a window in
    /// which the two reads disagree, and would cost a round trip to do it.
    fn blob_read_access<'a>(
        &'a self,
        caller: &'a OwnerId,
        reference: &'a BlobReference,
    ) -> ReadAuthorityFuture<'a, BlobReadAccess>;
}

/// The authority the server runs on: an account reads its own assets' blobs.
///
/// Holds nothing. The fact it decides on travels on the reference, which is deliberate — the
/// alternative is a store lookup per fetch to learn something the index already read.
#[derive(Debug, Clone, Copy, Default)]
pub struct OwnedAssetAuthority;

impl OwnedAssetAuthority {
    /// The authority.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl ReadAuthority for OwnedAssetAuthority {
    fn blob_read_access<'a>(
        &'a self,
        caller: &'a OwnerId,
        reference: &'a BlobReference,
    ) -> ReadAuthorityFuture<'a, BlobReadAccess> {
        Box::pin(async move {
            if &reference.owner_id == caller {
                return Ok(BlobReadAccess::Granted);
            }
            // Not `Revoked`. The caller never had it, and saying otherwise would confirm the
            // address is live — see the module docs on why the boundary is here.
            tracing::info!(
                asset = %reference.asset_id,
                "a blob fetch named an address belonging to another account"
            );
            Ok(BlobReadAccess::Unrelated)
        })
    }
}

/// A convenience for wiring the production authority.
#[must_use]
pub fn owned_assets() -> Arc<dyn ReadAuthority> {
    Arc::new(OwnedAssetAuthority::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::AssetState;
    use crate::store::{AssetId, BlobRole};

    /// A reference to `owner`'s asset.
    fn reference(owner: &str) -> BlobReference {
        BlobReference {
            asset_id: AssetId::new("asset"),
            owner_id: OwnerId::new(owner),
            role: BlobRole::Original,
            state: AssetState::Visible,
            original_held: true,
            hold: None,
        }
    }

    #[tokio::test]
    async fn an_account_reads_its_own() {
        assert_eq!(
            OwnedAssetAuthority::new()
                .blob_read_access(&OwnerId::new("alice"), &reference("alice"))
                .await
                .expect("the authority decides"),
            BlobReadAccess::Granted
        );
    }

    #[tokio::test]
    async fn anybody_else_is_unrelated_rather_than_forbidden() {
        // The disclosure boundary, at the unit that decides it. `Unrelated` becomes a `404`
        // identical to an unknown address; a `403` here would confirm the reference exists.
        assert_eq!(
            OwnedAssetAuthority::new()
                .blob_read_access(&OwnerId::new("mallory"), &reference("alice"))
                .await
                .expect("the authority decides"),
            BlobReadAccess::Unrelated
        );
    }

    /// State the caller cannot see does not change the answer.
    ///
    /// A tombstoned or held asset of somebody else's is `Unrelated` exactly as a live one is —
    /// the authority decides on ownership alone, so no lifecycle fact leaks through it. The
    /// serving path relies on this by asking it **first**.
    #[tokio::test]
    async fn a_strangers_answer_does_not_vary_with_the_assets_state() {
        for state in [AssetState::Visible, AssetState::Tombstoned] {
            let mut reference = reference("alice");
            reference.state = state;
            reference.hold = Some(crate::index::ServingHold::Takedown);
            assert_eq!(
                OwnedAssetAuthority::new()
                    .blob_read_access(&OwnerId::new("mallory"), &reference)
                    .await
                    .expect("the authority decides"),
                BlobReadAccess::Unrelated,
                "a stranger's refusal must not vary with facts about the owner's asset"
            );
        }
    }
}
