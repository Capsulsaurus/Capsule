package com.justin13888.capsule.feature.storage

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.SyncScope

/**
 * A failure the storage screens have to explain.
 *
 * Carries the stable `error.*` code rather than the exception, so a screen localizes a code
 * and never renders a transport status. Nullable because a purely local failure genuinely has
 * none, and inventing a key the catalogue does not hold would be worse than admitting it.
 */
@Immutable
data class StorageFailure(
    /** The catalogue code to localize, or `null` for a device-local failure. */
    val code: ErrorCode?,
) {
    internal companion object {
        /** Keeps only what the user can act on out of whatever the repositories threw. */
        fun of(throwable: Throwable): StorageFailure = StorageFailure((throwable as? CapsuleError)?.code)
    }
}

/** How a reclamation ended, so the screen can say what it actually did. */
@Immutable
sealed interface ReclaimOutcome {
    /** A sweep is running. */
    data object Running : ReclaimOutcome

    /** It finished, having freed [bytes] — zero when the cache was already inside budget. */
    data class Freed(
        val bytes: Long,
    ) : ReclaimOutcome
}

/**
 * Everything both storage destinations draw, as one value.
 *
 * One state across the quota screen and the local-storage screen rather than one each,
 * because the two are the same question asked of two authorities — "what is this library
 * occupying, and what is it allowed?" — and the remediation on the quota screen is an action
 * on the local screen. Splitting them would let the two disagree about how full the device is
 * while the user is looking at both halves of the same sentence.
 */
@Immutable
data class StorageUiState(
    /** Whether a reading is in flight and nothing has arrived yet. */
    val isLoading: Boolean = true,
    /** The failure to explain, or `null`. */
    val failure: StorageFailure? = null,
    /** The account's standing against its limits; `null` until the first reading lands. */
    val standing: QuotaStanding? = null,
    /** What the reclaimable cache currently occupies on this device. */
    val cacheBytes: Long = 0L,
    /** The ceiling the user chose for it. */
    val budget: CacheBudget = CacheBudget.DEFAULT,
    /** The largest local consumers, heaviest first. */
    val consumers: List<StorageConsumer> = emptyList(),
    /** How much of each new asset this device fetches. */
    val syncScope: SyncScope = SyncScope.MetadataAndThumbnails,
    /** The last reclamation, or `null` when none has been asked for. */
    val reclaim: ReclaimOutcome? = null,
) {
    /** Nothing is cached here — a device that has only ever browsed, or a fresh install. */
    val hasNoConsumers: Boolean get() = !isLoading && failure == null && consumers.isEmpty()

    /** Whether a sweep is in flight, so the button reports itself rather than looking dead. */
    val isReclaiming: Boolean get() = reclaim is ReclaimOutcome.Running

    /** The cache is over the ceiling the user set, so reclaiming would actually do something. */
    val isOverBudget: Boolean get() = cacheBytes > budget.bytes

    /** One consumer by id, for a test or a callback that only holds the identifier. */
    fun consumer(id: String): StorageConsumer? = consumers.firstOrNull { it.id.value == id }
}
