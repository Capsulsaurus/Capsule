package com.justin13888.capsule.feature.albums.model

import java.time.Duration
import java.time.Instant

/** Why an in-flight album upgrade stopped short of the fork. */
enum class UpgradeAbortReason {
    /**
     * The intent's deadline passed while the album was still draining.
     *
     * Evaluated against the server's trusted clock, so a skewed member clock can neither
     * extend nor shorten the window. The album returns to its old version and the writes
     * queued during quiescence are flushed back into it.
     */
    DeadlineExpired,

    /**
     * A member recomputed a different `frozen_state_hash` from the proposer's.
     *
     * That means at least one member's view of the album diverges, and a fork taken from a
     * disputed state would bake the divergence in. Each member aborts independently.
     */
    FrozenStateMismatch,

    /** The proposing admin abandoned the ceremony. */
    AbandonedByUser,
}

/**
 * Where an album's tombstone-plus-fork upgrade has got to.
 *
 * The ceremony is modelled as an explicit closed set rather than as a pile of booleans
 * because the *order* is the safety property: writes are quiesced before the drain, the
 * drain completes before the tombstone, and the tombstone is the single atomic cutover. A
 * representation that could express "forking while still draining" would be a
 * representation a bug could reach.
 *
 * Every phase after [Idle] carries the `intent_id`. That is the idempotency key for the
 * whole ceremony: it is what makes a retried proposal a no-op instead of a second fork, and
 * what lets a client that crashed mid-way resume rather than restart.
 */
sealed interface UpgradePhase {
    /** Normal operation at the album's current version. */
    data object Idle : UpgradePhase

    /**
     * The intent is out and members have stopped sending writes.
     *
     * In-flight uploads are allowed to reach a terminal state; new writes are held locally
     * with the intent id against them, never sent.
     */
    data class Quiescing(
        val intentId: String,
        val deadline: Instant,
        val queuedWrites: Int,
    ) : UpgradePhase

    /** Waiting for the album's remaining upload sessions to reach a terminal state. */
    data class Draining(
        val intentId: String,
        val deadline: Instant,
        val queuedWrites: Int,
        val inFlightUploads: Int,
    ) : UpgradePhase

    /** Drained; the tombstone commit is out and every member is checking its state hash. */
    data class Tombstoning(
        val intentId: String,
        val frozenStateHash: String,
        val queuedWrites: Int,
    ) : UpgradePhase

    /** The new group exists at the target version and members are being added to it. */
    data class Forking(
        val intentId: String,
        val toVersion: String,
        val queuedWrites: Int,
    ) : UpgradePhase

    /** The fork is active and the queued writes have been replayed against it. */
    data class Complete(
        val intentId: String,
        val toVersion: String,
    ) : UpgradePhase

    /** The album is back at its old version; nothing was lost. */
    data class Aborted(
        val intentId: String,
        val reason: UpgradeAbortReason,
    ) : UpgradePhase

    /** The intent this phase belongs to, or `null` while idle. */
    val intentIdOrNull: String?
        get() =
            when (this) {
                is Idle -> null
                is Quiescing -> intentId
                is Draining -> intentId
                is Tombstoning -> intentId
                is Forking -> intentId
                is Complete -> intentId
                is Aborted -> intentId
            }

    /** Whether the ceremony is still running and therefore still abortable. */
    val isRunning: Boolean
        get() = this is Quiescing || this is Draining || this is Tombstoning || this is Forking
}

/**
 * The pure transition function behind the album upgrade ceremony.
 *
 * Written as a state machine over [UpgradePhase] with no I/O of its own, because the thing
 * that has to be right here is the *ordering*, and ordering is exactly what a test can pin
 * down when it is separated from the network. Everything that would really be an MLS commit
 * or a server poll is a parameter to [advance].
 */
object UpgradeCeremony {
    /** The default expiry the design gives an upgrade intent. */
    val DEFAULT_DEADLINE: Duration = Duration.ofDays(7)

    /**
     * Issues the freeze proposal, entering quiescence.
     *
     * Refuses while another intent is in flight for the same album. Only one upgrade may run
     * at a time, or two proposals could produce two divergent forks of the same album; that
     * is the whole reason every step is keyed by an intent id.
     */
    fun propose(
        current: UpgradePhase,
        intentId: String,
        now: Instant,
        deadline: Duration = DEFAULT_DEADLINE,
    ): UpgradePhase? {
        if (current.isRunning) return null
        return UpgradePhase.Quiescing(intentId = intentId, deadline = now.plus(deadline), queuedWrites = 0)
    }

    /**
     * Moves the ceremony on one step.
     *
     * @param inFlightUploads how many upload sessions the server still reports for this
     *   album. The drain cannot finish while any of them is live.
     * @param frozenStateHash the hash every member recomputes over the album's frozen state.
     * @param membersAgree whether they all got the same answer.
     * @param toVersion the version the fork is taken at.
     * @param now the server-attested instant the deadline is judged against.
     */
    @Suppress("LongParameterList")
    fun advance(
        current: UpgradePhase,
        inFlightUploads: Int,
        frozenStateHash: String,
        membersAgree: Boolean,
        toVersion: String,
        now: Instant,
    ): UpgradePhase =
        when (current) {
            is UpgradePhase.Idle, is UpgradePhase.Complete, is UpgradePhase.Aborted -> current

            is UpgradePhase.Quiescing ->
                expireOr(current, current.deadline, now) {
                    UpgradePhase.Draining(
                        intentId = current.intentId,
                        deadline = current.deadline,
                        queuedWrites = current.queuedWrites,
                        inFlightUploads = inFlightUploads,
                    )
                }

            is UpgradePhase.Draining ->
                expireOr(current, current.deadline, now) {
                    if (inFlightUploads > 0) {
                        current.copy(inFlightUploads = inFlightUploads)
                    } else {
                        UpgradePhase.Tombstoning(
                            intentId = current.intentId,
                            frozenStateHash = frozenStateHash,
                            queuedWrites = current.queuedWrites,
                        )
                    }
                }

            // The cutover. Until this commit is applied a client is operating at the old
            // version; after it, at the new one. A member whose hash disagrees refuses here
            // rather than forking from a state it does not share.
            is UpgradePhase.Tombstoning ->
                if (membersAgree) {
                    UpgradePhase.Forking(
                        intentId = current.intentId,
                        toVersion = toVersion,
                        queuedWrites = current.queuedWrites,
                    )
                } else {
                    UpgradePhase.Aborted(current.intentId, UpgradeAbortReason.FrozenStateMismatch)
                }

            is UpgradePhase.Forking ->
                UpgradePhase.Complete(intentId = current.intentId, toVersion = current.toVersion)
        }

    /**
     * Abandons a running ceremony.
     *
     * Only meaningful before the tombstone commit, which is the atomic cutover — after it
     * there is nothing to abort, because the album is already the new one.
     */
    fun abort(current: UpgradePhase): UpgradePhase =
        when (current) {
            is UpgradePhase.Quiescing, is UpgradePhase.Draining, is UpgradePhase.Tombstoning ->
                UpgradePhase.Aborted(
                    intentId = current.intentIdOrNull.orEmpty(),
                    reason = UpgradeAbortReason.AbandonedByUser,
                )

            else -> current
        }

    /** Records a write the user made while the album was quiesced, so it can be replayed. */
    fun queueWrite(current: UpgradePhase): UpgradePhase =
        when (current) {
            is UpgradePhase.Quiescing -> current.copy(queuedWrites = current.queuedWrites + 1)
            is UpgradePhase.Draining -> current.copy(queuedWrites = current.queuedWrites + 1)
            is UpgradePhase.Tombstoning -> current.copy(queuedWrites = current.queuedWrites + 1)
            is UpgradePhase.Forking -> current.copy(queuedWrites = current.queuedWrites + 1)
            else -> current
        }

    /**
     * Aborts on an expired deadline, otherwise takes the step.
     *
     * The check is folded into one helper so every phase that can time out does it the same
     * way; a phase that forgot would silently hold the album frozen past its window, which
     * is the failure the deadline exists to prevent.
     */
    private inline fun expireOr(
        current: UpgradePhase,
        deadline: Instant,
        now: Instant,
        next: () -> UpgradePhase,
    ): UpgradePhase =
        if (!now.isBefore(deadline)) {
            UpgradePhase.Aborted(current.intentIdOrNull.orEmpty(), UpgradeAbortReason.DeadlineExpired)
        } else {
            next()
        }
}
