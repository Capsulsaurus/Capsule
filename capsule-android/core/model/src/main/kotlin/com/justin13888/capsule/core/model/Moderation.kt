package com.justin13888.capsule.core.model

import java.time.Instant

/**
 * An administrative action recorded against an account or an asset.
 *
 * The log is append-only and a lift is its own entry rather than a mutation of the original,
 * so "this was taken down on X and restored on Y" stays readable — the same discipline the
 * provenance chain applies to deletes.
 */
enum class ModerationEventKind(
    override val wire: String,
) : WireEnum {
    /** An asset stopped being served. Its bytes are not deleted. */
    Takedown("takedown"),

    /** A takedown was reversed. */
    TakedownLifted("takedown_lifted"),

    /**
     * An asset is held for a legal obligation.
     *
     * Distinct from [Takedown] because it is lifted only when the obligation ends, never at
     * administrative discretion.
     */
    LegalHold("legal_hold"),

    /** The account was suspended. An access-level action; the user's data is untouched. */
    Suspended("suspended"),

    /** A suspension was lifted. Reversibility is the default. */
    Unsuspended("unsuspended"),
    ;

    /** Whether this entry reverses an earlier one. */
    val isReversal: Boolean get() = this == TakedownLifted || this == Unsuspended

    companion object : WireCodec<ModerationEventKind>({ entries })
}

/**
 * One entry of the moderation log, as surfaced to the affected user.
 *
 * [assetId] is absent for account-level actions, which is the only structural difference
 * between the two kinds of entry.
 */
data class ModerationEvent(
    /** The entry's handle — UUIDv7, so the log is time-ordered by id alone. */
    val id: ModerationEventId,
    /** The affected account. */
    val userId: UserId,
    /** The affected asset; absent for a suspension or its lift. */
    val assetId: AssetId? = null,
    /** What happened. */
    val kind: Wire<ModerationEventKind>,
    /**
     * The administrator's stated reason, where policy permits surfacing it.
     *
     * Deliberately free text rather than a closed set: the design pins the *mechanics* of a
     * report — authentication, rate limiting, and that it names a content hash and never
     * plaintext — and leaves categorisation to the deployment's policy.
     */
    val reason: String? = null,
    /** When the action was applied. */
    val createdAt: Instant,
)

/**
 * What a suspended account may still do.
 *
 * Suspension is an access-level action, not a data-level one. The account keeps every read,
 * and — critically — keeps the ability to secure itself: revoking all sessions is gated by
 * proof of master-key possession, not by account standing, so a user whose account was
 * suspended after a compromise can still lock the attacker out.
 */
data class SuspensionEffects(
    /** New upload sessions are refused. */
    val canUpload: Boolean = false,
    /** New shares and links are refused; existing share links keep serving until they expire. */
    val canCreateLinks: Boolean = false,
    /** Existing assets remain readable. */
    val canRead: Boolean = true,
    /** Master-key-gated session revocation stays available. */
    val canRevokeAllSessions: Boolean = true,
)

/**
 * A block the user placed on another account.
 *
 * Scoped to this user and this user only: it rides an MLS removal and a key-epoch bump, and
 * it does **not** propagate as a server-wide federation block. Conflating a personal block
 * with an operator decision would let one user's choice silently become policy for everyone
 * on the server.
 */
data class UserBlock(
    /** The blocked account. */
    val blockedUserId: UserId,
    /** When the block was placed. Re-blocking is idempotent. */
    val createdAt: Instant,
)
