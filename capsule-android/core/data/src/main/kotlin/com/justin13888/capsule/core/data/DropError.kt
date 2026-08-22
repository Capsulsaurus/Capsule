package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.ErrorCode

/**
 * Failures on the web-upload drop path - the guest side and the inbox.
 *
 * A guest is unauthenticated by construction, so every refusal here has to be safe to hand to a
 * stranger - none of them reveals anything about the inbox behind the link.
 */
sealed class DropError(
    code: String,
    detail: String,
) : CapsuleError(ErrorCode(code), detail) {
    /**
     * HTTP 409 on POST /u/{opaque-id}/drop (web-upload, invariant 26): a cumulative per-link cap
     * (total bytes or file count) is already exhausted for an otherwise-live link.
     */
    data object CapExceeded : DropError(
        code = "error.drop.cap_exceeded",
        detail = "This upload link is full.",
    )

    /**
     * HTTP 400 on POST /u/{opaque-id}/drop (web-upload, invariant 30): the DropDescriptor is
     * structurally malformed, its kem_ct length is wrong for the link's crypto suite, or the request
     * carried an album/manifest/provenance field a drop must never carry.
     */
    data object MalformedDescriptor : DropError(
        code = "error.drop.malformed_descriptor",
        detail = "This upload couldn't be accepted.",
    )

    /**
     * HTTP 409 on POST /drops/{id}/adopt (web-upload, invariant 32): the manifest's ciphertext_hash
     * does not reference a pending drop in the caller's own inbox.
     */
    data object NotInInbox : DropError(
        code = "error.drop.not_in_inbox",
        detail = "This drop is no longer available to adopt.",
    )

    /**
     * HTTP 403 on POST /u/{opaque-id}/drop (web-upload): the link carries an Argon2id abuse-gate
     * verifier and the guest supplied no proof, or a proof that did not verify.
     */
    data object PassphraseRequired : DropError(
        code = "error.drop.passphrase_required",
        detail = "This upload link needs its passphrase.",
    )

    /**
     * HTTP 429 on POST /u/{opaque-id}/drop (web-upload, invariant 31): the per-{opaque-id} or per-
     * source-IP drop-session rate limit engaged.
     */
    data object RateLimited : DropError(
        code = "error.drop.rate_limited",
        detail = "Too many attempts. Please wait and try again.",
    )

    companion object {
        /** Every variant, so a test can hold this family to the message catalogue. */
        val ALL: List<DropError> =
            listOf(
                CapExceeded,
                MalformedDescriptor,
                NotInInbox,
                PassphraseRequired,
                RateLimited,
            )
    }
}
