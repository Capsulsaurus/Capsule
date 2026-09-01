//! The moderation record a user can read about their own account (slice `S-C8`).
//!
//! One operation, and it is the *subject's*, never an admin's. design/moderation.md's structural
//! rule is that there are **no silent operations**: a user whose asset stops serving or whose
//! account stops uploading is never left to guess why. This is where they find out.
//!
//! # Why there is no admin surface here
//!
//! The contract names an admin queue and an admin who acts on it, and specifies **no way for an
//! admin to authenticate**. Inventing one would be inventing the most sensitive authentication
//! surface on the server from nothing, so moderation *actions* stay behind
//! [`crate::moderation::ModerationStore`] — the shape [`crate::gc`] and [`crate::scrub`] already
//! use for operator work — and the gap is recorded rather than filled by guesswork.
//!
//! # Scoped to the caller, with no path parameter
//!
//! For the same reason the escrow is: the only account entitled to a moderation record is its
//! own, and an admin reading somebody else's is not this surface. Making it structural beats
//! enforcing it.

use capsule_i18n::error_codes;
use kynos::prelude::*;
use serde::{Deserialize, Serialize};

use crate::auth::AccessToken;
use crate::moderation::{ModerationContext, Standing};
use crate::store::UserId;

/// The moderation surface: what was done to this account, and whether it may write.
#[derive(Tag)]
#[tag(
    name = "moderation",
    description = "An account's own moderation record. No silent operations."
)]
pub struct ModerationTag;

/// One thing that was done to the account.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ModerationEventResponse {
    /// What was done: `suspended`, `reinstated`, `taken_down`, `legal_hold`, `hold_lifted`.
    pub action: String,
    /// The asset, when the action was about one rather than about the account.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<String>,
    /// When it happened, RFC 3339.
    pub at: String,
    /// Why, where policy permits.
    ///
    /// Absent is a real answer — a legal hold may come with an obligation not to disclose it —
    /// and reads as "we are not able to say", which is honest where a fabricated reason would
    /// not be.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The caller's moderation record.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ModerationRecordResponse {
    /// `active` or `suspended`.
    pub standing: String,
    /// When a suspension began, RFC 3339. Absent while the account is active.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suspended_since: Option<String>,
    /// Everything done to this account, oldest first.
    ///
    /// A reinstatement does not erase the suspension it lifted: the record is what a user reads
    /// to understand their own account, and one that deleted its own history would leave them
    /// unable to see that anything ever happened.
    pub events: Vec<ModerationEventResponse>,
}

/// Why the record could not be read.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum ModerationRecordRejection {
    /// The store could not answer.
    #[error("the moderation record could not be read")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Serve the caller's own moderation record.
#[kynos::get(
    "/v1/moderation/record",
    operation_id = "moderation_record",
    tag = ModerationTag
)]
pub async fn moderation_record(
    Inject(moderation): Inject<ModerationContext>,
    Auth(credential): Auth<AccessToken>,
) -> Result<Json<ModerationRecordResponse>, ModerationRecordRejection> {
    let user = UserId::new(credential.user.as_str());

    let standing = moderation.store().standing(&user).await.map_err(|error| {
        tracing::error!(%error, %user, "the moderation store could not answer a standing");
        ModerationRecordRejection::unavailable()
    })?;

    let events = moderation
        .store()
        .events_for_user(&user)
        .await
        .map_err(|error| {
            tracing::error!(%error, %user, "the moderation store could not answer a record");
            ModerationRecordRejection::unavailable()
        })?;

    let (name, since) = match &standing {
        Standing::Active => ("active", None),
        Standing::Suspended { since } => ("suspended", Some(since.to_string())),
    };

    tracing::debug!(%user, events = events.len(), "served a moderation record");
    Ok(Json(ModerationRecordResponse {
        standing: name.to_owned(),
        suspended_since: since,
        events: events
            .into_iter()
            .map(|event| ModerationEventResponse {
                action: event.action.as_str().to_owned(),
                asset_id: event.asset_id.map(|id| id.to_string()),
                at: event.at.to_string(),
                reason: event.reason,
            })
            .collect(),
    }))
}

impl ModerationRecordRejection {
    /// The store could not answer.
    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::MODERATION_UNAVAILABLE,
        }
    }
}
