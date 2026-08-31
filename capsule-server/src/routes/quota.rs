//! `GET /v1/quota` — the storage-quota snapshot (slice `S-C6`).
//!
//! Read-only, and deliberately not an enforcement point. The hard check runs at
//! `POST /v1/upload` session creation; this exists so a client can show a user what is full and
//! what to delete *before* an import fails, which is what turns a quota from an opaque
//! mid-transfer error into a remediable state. [`crate::quota`] owns the accounting and the
//! state machine.
//!
//! **The limits come back as `null` when the deployment runs unlimited**, rather than as
//! `u64::MAX`. A self-hosted server with no quota should render as "no limit" in a client that
//! knows nothing about sentinels, and a number that is not a limit is a number some client will
//! eventually put in a progress bar.
//!
//! # `S-C28` audit
//!
//! | Salvo status | Verdict |
//! | --- | --- |
//! | `200` | kept |
//! | `401` | kept, and now the framework's |
//! | `500` | kept, with `error.quota.unavailable` |

use capsule_i18n::error_codes;
use kynos::prelude::*;
use serde::{Deserialize, Serialize};

use crate::auth::AccessToken;
use crate::quota::{QuotaContext, QuotaLimits, state_of};
use crate::store::UserId;

/// The quota surface: what a user is using, and what happens next.
#[derive(Tag)]
#[tag(
    name = "quota",
    description = "Reporting storage use so a client can act before an import fails."
)]
pub struct QuotaTag;

/// A user's quota snapshot.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct QuotaResponse {
    /// Bytes charged to the caller.
    pub used: u64,
    /// Where the warning starts, or absent on an unlimited deployment.
    pub soft_limit: Option<u64>,
    /// Where uploads stop, or absent on an unlimited deployment.
    pub hard_limit: Option<u64>,
    /// The classified state: `ok`, `soft_warning`, `hard_exceeded`, `grace_expired`.
    pub state: String,
}

/// Why no snapshot was returned.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum QuotaRejection {
    /// The ledger could not answer.
    #[error("the quota could not be read")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Report the authenticated uploader's storage-quota snapshot.
///
/// Scoped to the caller, and to nobody else: quota is accounted to the *uploader*, and one
/// account's storage use is not another's business.
#[kynos::get("/v1/quota", operation_id = "get_quota", tag = QuotaTag)]
pub async fn get_quota(
    Inject(quota): Inject<QuotaContext>,
    Auth(credential): Auth<AccessToken>,
) -> Result<Json<QuotaResponse>, QuotaRejection> {
    let user = UserId::new(credential.user.as_str());
    let usage = quota.quotas().usage(&user).await.map_err(|error| {
        tracing::error!(%error, %user, "the quota ledger could not answer");
        QuotaRejection::Unavailable {
            code: error_codes::QUOTA_UNAVAILABLE,
        }
    })?;

    let limits = quota.limits();
    let state = state_of(usage.used, usage.over_since, quota.clock().now(), limits);
    Ok(Json(QuotaResponse {
        used: usage.used,
        soft_limit: limit(limits.soft_limit),
        hard_limit: limit(limits.hard_limit),
        state: state.as_str().to_owned(),
    }))
}

/// A limit, or `None` when the deployment does not have one.
fn limit(value: u64) -> Option<u64> {
    (value != QuotaLimits::unlimited().hard_limit).then_some(value)
}
