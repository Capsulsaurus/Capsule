package com.justin13888.capsule.feature.albums.model

import com.justin13888.capsule.core.model.DeletionState
import java.time.Duration
import java.time.Instant

/**
 * How much of a trashed asset's signed retention window is left.
 *
 * A closed set rather than a formatted string so the screen owns the wording and this owns
 * the arithmetic, and so a test can assert the boundary rather than a translation. The
 * boundary that matters is [Expired]: past it the purge worker may proceed, and the UI must
 * stop implying the photograph is still recoverable.
 */
sealed interface RetentionCountdown {
    /** More than a day remains. */
    data class Days(
        val days: Long,
    ) : RetentionCountdown

    /** Less than a day remains — the last stretch, where hours are what the user needs. */
    data class Hours(
        val hours: Long,
    ) : RetentionCountdown

    /** The signed `retention_until` has passed; a hard purge is now permitted. */
    data object Expired : RetentionCountdown

    companion object {
        /**
         * The countdown for [state] as of [now].
         *
         * Rounded **down**, so "1 day left" never becomes an over-promise: the user is told
         * the floor on what remains, not the ceiling.
         */
        fun of(
            state: DeletionState.Trashed,
            now: Instant,
        ): RetentionCountdown {
            val remaining = state.retentionRemaining(now)
            return when {
                remaining <= Duration.ZERO -> Expired
                remaining.toDays() >= 1 -> Days(remaining.toDays())
                else -> Hours(remaining.toHours())
            }
        }
    }
}
