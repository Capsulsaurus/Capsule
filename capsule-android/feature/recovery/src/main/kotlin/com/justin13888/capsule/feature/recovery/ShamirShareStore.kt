package com.justin13888.capsule.feature.recovery

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import java.time.Clock
import java.time.Instant

/**
 * One share of a split recovery secret, as the user deals with it.
 *
 * [payload] is what gets handed over; [holder] and [deliveredAt] are the user's own bookkeeping
 * about where it went. Distribution is tracked because an undistributed share is not a
 * recovery path — three envelopes still in the drawer beside the phone protect against
 * nothing that losing the phone would not also take.
 */
data class ShamirShare(
    /** The evaluation point, which is also how the share is named to the user. */
    val index: Int,
    /** The share itself, in the form the user copies or prints. */
    val payload: String,
    /** Who or where it went, in the user's own words. */
    val holder: String? = null,
    /** When the user said they had handed it over. */
    val deliveredAt: Instant? = null,
) {
    /** Whether this share is somewhere other than this device. */
    val isDelivered: Boolean get() = deliveredAt != null
}

/**
 * A whole `m`-of-`n` enrolment.
 *
 * The threshold rides along with the shares because it is not derivable from them — that is
 * the point of the scheme — and because every sentence the screen wants to say about progress
 * is a comparison against it.
 */
data class ShamirEnrollment(
    /** How many shares reconstruct the secret. */
    val threshold: Int,
    /** How many exist. */
    val total: Int,
    /** Them, in order. */
    val shares: List<ShamirShare>,
) {
    /** How many are somewhere other than this device. */
    val deliveredCount: Int get() = shares.count(ShamirShare::isDelivered)

    /** Whether enough are out that the secret could be rebuilt without this device. */
    val isDistributed: Boolean get() = deliveredCount >= threshold
}

/**
 * Where this device remembers the guardian shares it minted.
 *
 * A feature-owned port because no repository owns Shamir yet: the design fixes that
 * reconstruction is fully client-side and that the server never sees more than one share, so
 * the eventual home for this is `capsule-core`, not a new server surface. Declaring it as an
 * interface means the two screens that care — share setup, and the re-wrap that invalidates
 * them — already agree on the shape, and that swapping in the real store is a binding change.
 */
interface ShamirShareStore {
    /** The current enrolment, or `null` when the user has never split their secret. */
    val enrollment: StateFlow<ShamirEnrollment?>

    /** Replaces whatever was there. Minting a new set always discards the old one. */
    fun replace(enrollment: ShamirEnrollment)

    /** Records that one share has left this device. */
    fun markDelivered(
        index: Int,
        holder: String,
    )

    /**
     * Discards the enrolment because the secret it splits no longer opens anything.
     *
     * Called by the guided re-wrap. The old shares are not merely stale — they reconstruct a
     * secret whose escrow has been deleted — so leaving them listed would be listing a
     * recovery path that does not exist.
     */
    fun invalidate()
}

/**
 * The in-memory store this build ships with.
 *
 * Deliberately not persisted. A share that survived a reinstall would have to be written
 * somewhere on this device, and a share sitting beside the library it protects is the one
 * place the scheme says it must not be.
 */
class InMemoryShamirShareStore(
    private val clock: Clock = Clock.systemUTC(),
) : ShamirShareStore {
    private val state = MutableStateFlow<ShamirEnrollment?>(null)

    override val enrollment: StateFlow<ShamirEnrollment?> = state.asStateFlow()

    override fun replace(enrollment: ShamirEnrollment) {
        state.value = enrollment
    }

    override fun markDelivered(
        index: Int,
        holder: String,
    ) {
        state.update { current ->
            current?.copy(
                shares =
                    current.shares.map { share ->
                        if (share.index == index) {
                            share.copy(holder = holder, deliveredAt = clock.instant())
                        } else {
                            share
                        }
                    },
            )
        }
    }

    override fun invalidate() {
        state.value = null
    }
}
