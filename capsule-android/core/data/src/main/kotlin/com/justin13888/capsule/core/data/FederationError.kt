package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.ErrorCode

/**
 * Failures exercising a capability against another home server.
 *
 * Every variant separates a capability problem from a connectivity problem, because the two call
 * for opposite client behaviour: one is worth retrying and the other never is.
 */
sealed class FederationError(
    code: String,
    detail: String,
) : CapsuleError(ErrorCode(code), detail) {
    /**
     * HTTP 403 on a federation pull (slice S-E2, threat-model invariant 19): the capability's aud
     * (an urn:capsule:album:UUID) does not match the album the peer is pulling.
     */
    data object AudienceMismatch : FederationError(
        code = "error.federation.audience_mismatch",
        detail = "This access grant is for a different album.",
    )

    /**
     * HTTP 401 on a federation pull (slice S-E2, threat-model invariant 19): the capability token is
     * past its exp.
     */
    data object CapabilityExpired : FederationError(
        code = "error.federation.capability_expired",
        detail = "This shared album's access has expired.",
    )

    /**
     * HTTP 401 on a federation pull (slice S-E2, threat-model invariant 19): the presented
     * capability token failed structural verification — a missing claim, a bad Ed25519 signature, an
     * unparseable RFC 3339 claim, a wrong issuer, a not-yet-valid (nbf) token, or an expiry window
     * wider than the 24h ceiling.
     */
    data object CapabilityInvalid : FederationError(
        code = "error.federation.capability_invalid",
        detail = "This shared album's access could not be verified.",
    )

    /**
     * HTTP 403 on a federation pull (slice S-E2, threat-model invariant 19): the capability's jti is
     * on the issuer's revocation list, OR the verifier is relying on a cached revocation list it can
     * no longer refresh past the 15-minute staleness bound and therefore cannot confirm the jti —
     * the fail-closed rule.
     */
    data object CapabilityRevoked : FederationError(
        code = "error.federation.capability_revoked",
        detail = "Access to this shared album has been revoked.",
    )

    /**
     * HTTP 429 on a federation pull (slice S-E2): the peer spent its per-peer error budget on
     * malformed input, tripping a circuit breaker that backs it off exponentially (5 / 30 / 60
     * minutes).
     */
    data object CircuitOpen : FederationError(
        code = "error.federation.circuit_open",
        detail = "This source is temporarily backed off after repeated errors.",
    )

    /**
     * HTTP 429 on a federation pull (slice S-E2, threat-model invariant 21): the peer exceeded one
     * of its per-peer transfer budgets — events/hour, bytes/hour, or CPU/hour.
     */
    data object RateBudgetExceeded : FederationError(
        code = "error.federation.rate_budget_exceeded",
        detail = "This source has reached its request limit. Please wait and try again.",
    )

    /**
     * HTTP 403 on a federation blob fetch (slice S-E2, threat-model invariant 19): a read-
     * derivative-only capability tried to fetch a blob whose server-visible role is 'original'.
     */
    data object ScopeInsufficient : FederationError(
        code = "error.federation.scope_insufficient",
        detail = "This access grant does not cover the requested content.",
    )

    companion object {
        /** Every variant, so a test can hold this family to the message catalogue. */
        val ALL: List<FederationError> =
            listOf(
                AudienceMismatch,
                CapabilityExpired,
                CapabilityInvalid,
                CapabilityRevoked,
                CircuitOpen,
                RateBudgetExceeded,
                ScopeInsufficient,
            )
    }
}
