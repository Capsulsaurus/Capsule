//! "Log out of all devices" — the global-revocation HTTP surface (slice `S-C23`; SSoT:
//! [Authentication — Explicit Revocation] item 3).
//!
//! Two endpoints under `logout/all`, mirroring the asymmetry the doc requires:
//!
//! - `POST /logout/all/challenge` is **session-authed**: any active access token names the
//!   account and gets a single-use challenge. Handing out a challenge authorizes nothing.
//! - `POST /logout/all` is **not** session-authed. The [`RevokeAllProof`] — an identity-key
//!   signature over that challenge — is the entire credential, exactly as the doc specifies
//!   ("authenticated by proof of master-key possession, not by an active session token"). A
//!   stolen session token therefore buys an attacker a challenge they cannot sign, never a
//!   denial-of-service against every other device.
//!
//! **No confirmation without proof.** A missing or unreadable proof is `401
//! error.auth.revoke_proof_required`; a proof that does not verify is `401
//! error.auth.revoke_proof_invalid`, with every underlying reason collapsed into that one
//! code so the endpoint is not an oracle. Neither revokes anything — there is no partial
//! success, and a client must not clear local state on either.
//!
//! On success **every** session is invalidated, the calling one included — that is the point
//! of a global revoke, so the caller is logged out too rather than exempted.
//!
//! [Authentication — Explicit Revocation]: https://docs/design/authentication/#explicit-revocation

use capsule_i18n::error_codes;
use salvo::prelude::*;
use serde::Serialize;

use crate::errors::ClaimValidationError;
use crate::models::errors::ApiError;
use crate::revocation::{self, RevokeAllProof};
use crate::state::AppState;
use crate::utils::headers::get_token_from_headers;

/// A freshly issued revoke-all challenge for the client to sign with its identity key.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct RevokeAllChallengeResponse {
    /// The opaque challenge. The client signs the domain-separated canonical-CBOR encoding of
    /// `(domain, user_id, challenge)` over this exact string and echoes it back in the proof.
    pub challenge: String,
    /// The account the challenge is bound to, echoed because it is folded into the signed
    /// bytes. It is the caller's own id (the access token's `sub`) — returned so a client
    /// never has to crack open a JWT to build the signing input, and so a mismatch is caught
    /// before signing rather than as an opaque refusal.
    pub user_id: String,
    /// RFC 3339 expiry. Single-use: it is spent by the first proof presented against it.
    pub expires_at: String,
}

/// The acknowledgement for a completed global revoke.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct RevokeAllResponse {
    /// How many sessions were invalidated, the calling session included.
    pub revoked_sessions: u32,
}

/// Resolve the caller's account id from the bearer access token, or the auth error to surface.
/// Used only to *issue* a challenge — never to authorize the revoke itself.
fn authenticate(req: &Request, state: &AppState) -> Result<String, ClaimValidationError> {
    let token = get_token_from_headers(req.headers())?;
    use secrecy::ExposeSecret;
    let claims = state.auth_service.get_claims(token.expose_secret())?;
    claims.validate_access_token()?;
    Ok(claims.sub)
}

// ── challenge ────────────────────────────────────────────────────────────────

/// Responses for challenge issuance.
pub(super) enum ChallengeResponses {
    /// Challenge issued.
    Ok(RevokeAllChallengeResponse),
    /// No/invalid bearer token.
    Unauthorized(ClaimValidationError),
    /// Server fault.
    Internal,
}

capsule_wire::salvo_responses! {
    ChallengeResponses {
        Ok(body) => 200, json(body),
            doc("Single-use revoke-all challenge issued", schema = RevokeAllChallengeResponse);
        Unauthorized(e) => _, delegate(e), undocumented();
        Internal {} => 500, json(ApiError::new("Internal server error")), undocumented();
    }
    delegated {
        401 => "Missing or invalid access token",
    }
}

/// Issue a single-use challenge for the caller's account to sign with its identity key.
///
/// Session-authed: this only names the account. It authorizes no revocation on its own — the
/// signature over it does.
#[endpoint(operation_id = "revoke_all_challenge", tags("auth"), security(("bearer" = [])))]
pub async fn revoke_all_challenge(req: &mut Request, depot: &mut Depot) -> ChallengeResponses {
    let state = depot
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");

    let user_id = match authenticate(req, state) {
        Ok(id) => id,
        Err(e) => return ChallengeResponses::Unauthorized(e),
    };

    match revocation::issue(&state.session_manager, &user_id, revocation::CHALLENGE_TTL).await {
        Ok(issued) => match jiff::Timestamp::from_second(issued.expires_at) {
            Ok(ts) => ChallengeResponses::Ok(RevokeAllChallengeResponse {
                challenge: issued.challenge,
                user_id,
                expires_at: ts.to_string(),
            }),
            Err(e) => {
                tracing::error!("revoke-all challenge expiry timestamp error: {e}");
                ChallengeResponses::Internal
            }
        },
        Err(e) => {
            tracing::error!("revoke-all challenge issue error: {e}");
            ChallengeResponses::Internal
        }
    }
}

// ── revoke ───────────────────────────────────────────────────────────────────

/// Responses for the global revoke. Both refusals are hard: nothing is revoked.
pub(super) enum RevokeAllResponses {
    /// Every session invalidated, the caller's included.
    Ok(RevokeAllResponse),
    /// No readable proof was presented (`401 error.auth.revoke_proof_required`).
    ProofRequired(String),
    /// A proof was presented but did not verify (`401 error.auth.revoke_proof_invalid`).
    /// Every underlying reason is surfaced identically — no oracle.
    ProofInvalid,
    /// Server fault.
    Internal,
}

capsule_wire::salvo_responses! {
    RevokeAllResponses {
        Ok(body) => 200, json(body),
            doc("Every session revoked, the calling session included", schema = RevokeAllResponse);
        ProofRequired(detail) => 401, json(ApiError::with_code(
            format!("Master-key proof required to revoke all sessions: {detail}"),
            error_codes::AUTH_REVOKE_PROOF_REQUIRED,
        )), doc(
            "Missing proof (error.auth.revoke_proof_required) or a proof that did not \
             verify (error.auth.revoke_proof_invalid); nothing is revoked either way"
        );
        ProofInvalid {} => 401, json(ApiError::with_code(
            "Master-key proof did not verify; no sessions were revoked",
            error_codes::AUTH_REVOKE_PROOF_INVALID,
        )), undocumented();
        Internal {} => 500, json(ApiError::new("Internal server error")), undocumented();
    }
}

/// Collapse any refusal into the one indistinguishable wire outcome, recording *which*
/// refusal it was in a single queryable log line. The reason stays server-side: a client that
/// could tell "wrong key" from "spent challenge" would have an oracle.
fn refused(refusal: revocation::Refusal) -> RevokeAllResponses {
    tracing::warn!(?refusal, "revoke-all refused: nothing was revoked");
    RevokeAllResponses::ProofInvalid
}

/// Revoke every session for the account named by a signed challenge.
///
/// The body is the canonical-CBOR [`RevokeAllProof`]. Deliberately **not** session-authed: the
/// identity-key signature is the whole credential, so a stolen session token cannot log the
/// legitimate user out of their other devices.
#[endpoint(operation_id = "revoke_all_sessions", tags("auth"))]
pub async fn revoke_all_sessions(req: &mut Request, depot: &mut Depot) -> RevokeAllResponses {
    let state = depot
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");

    // No decodable proof at all — not even an attempt. Refuse before touching any state.
    let body = match req.payload_with_max_size(revocation::MAX_PROOF_BYTES).await {
        Ok(bytes) if !bytes.is_empty() => bytes.to_vec(),
        Ok(_) => return RevokeAllResponses::ProofRequired("empty body".into()),
        Err(e) => return RevokeAllResponses::ProofRequired(format!("unreadable body: {e}")),
    };
    let proof: RevokeAllProof = match capsule_core::cbor::from_slice(&body) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("revoke-all refused: proof document did not decode: {e}");
            return RevokeAllResponses::ProofRequired("undecodable proof document".into());
        }
    };

    // Burn the challenge before deciding anything: a spent challenge cannot be replayed and
    // cannot be ground against.
    let user_id =
        match revocation::consume_challenge(&state.session_manager, &proof.challenge).await {
            Ok(Some(id)) => id,
            Ok(None) => return refused(revocation::Refusal::UnknownOrExpiredChallenge),
            Err(e) => {
                tracing::error!("revoke-all challenge lookup error: {e}");
                return RevokeAllResponses::Internal;
            }
        };

    // The anchor: the account's own published, monotonic device directory.
    let directory = match service::directory::Query::fetch(&state.conn, &user_id).await {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("revoke-all directory fetch error: {e}");
            return RevokeAllResponses::Internal;
        }
    };

    if let Err(refusal) = revocation::verify_proof(&user_id, &proof, directory.as_deref()) {
        return refused(refusal);
    }

    match state.session_manager.revoke_all_for_user(&user_id).await {
        Ok(revoked) => {
            tracing::info!(
                user_id = %user_id,
                revoked,
                "revoke-all completed: every session invalidated, caller included"
            );
            RevokeAllResponses::Ok(RevokeAllResponse {
                revoked_sessions: u32::try_from(revoked).unwrap_or(u32::MAX),
            })
        }
        Err(e) => {
            tracing::error!("revoke-all session invalidation error: {e}");
            RevokeAllResponses::Internal
        }
    }
}
