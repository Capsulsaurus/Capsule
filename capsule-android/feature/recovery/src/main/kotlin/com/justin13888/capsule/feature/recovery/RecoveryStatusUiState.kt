package com.justin13888.capsule.feature.recovery

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.model.RecoveryCadence
import com.justin13888.capsule.core.model.RecoveryState

/**
 * Everything the recovery hub draws, as one value.
 *
 * There is deliberately no field here that any other flow could read as a gate. The
 * verification cadence is advisory by design — it never blocks sync, unlock, or anything else
 * — and the way that invariant survives future edits is that this state exposes nothing a
 * caller could mistake for permission. What it exposes is a prompt and some actions.
 */
@Immutable
data class RecoveryStatusUiState(
    /** Whether the first reading is still in flight. */
    val isLoading: Boolean = true,
    /** The failure to explain, or `null`. */
    val failure: RecoveryFailure? = null,
    /** Where the user stands, as the client derives it. */
    val recovery: RecoveryState? = null,
    /** Whether a snooze is being recorded right now. */
    val snoozing: Boolean = false,
) {
    /** Whether the prompt can still be deferred, or has become a standing badge. */
    val canSnooze: Boolean
        get() =
            when (val current = recovery) {
                is RecoveryState.VerificationDue -> true
                is RecoveryState.Snoozed -> current.consecutiveSnoozes < RecoveryCadence.MAX_CONSECUTIVE_SNOOZES
                else -> false
            }

    /**
     * Whether the reminder has stopped being dismissible.
     *
     * Past the snooze cap the prompt stays on screen rather than disappearing — which is the
     * strongest thing this feature is allowed to do, and considerably weaker than the lockout
     * a reader might otherwise assume "persistent" means.
     */
    val isPersistentBadge: Boolean
        get() =
            (recovery as? RecoveryState.Snoozed)?.let {
                it.consecutiveSnoozes >= RecoveryCadence.MAX_CONSECUTIVE_SNOOZES
            } ?: false

    /** Whether the guided re-wrap is the thing to offer first. */
    val rewrapRecommended: Boolean get() = recovery is RecoveryState.RewrapRecommended

    /** Nothing is set up, so the whole hub is an invitation rather than a status. */
    val isNotConfigured: Boolean get() = recovery is RecoveryState.NotConfigured
}
