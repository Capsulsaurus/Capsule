package com.justin13888.capsule.feature.moderation

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.model.ModerationEvent
import com.justin13888.capsule.core.model.SuspensionEffects

/** Where an appeal has got to. */
enum class AppealPhase {
    /** The user is writing it. */
    Editing,

    /** The master key is signing the server's challenge. */
    Proving,

    /** The signed appeal is on its way to the admin queue. */
    Sending,

    /** It arrived. */
    Sent,

    /** It did not, and why is in [AppealUiState.failure] or [AppealUiState.proofRejected]. */
    Failed,
}

/**
 * Everything the appeal screen draws, as one value.
 *
 * The record and the appeal live on one screen rather than two because they are one question:
 * a user who arrives here wants to know what was done, what it cost them, and how to contest
 * it, and splitting those across destinations makes each half read as less than it is.
 */
@Immutable
data class AppealUiState(
    /** The record entry being contested, as the destination named it. */
    val caseId: String = "",
    /** Whether the first reading is still in flight. */
    val isLoading: Boolean = true,
    /** The failure to explain, or `null`. */
    val failure: ModerationFailure? = null,
    /** What the account can still do, or `null` when it is in good standing. */
    val effects: SuspensionEffects? = null,
    /** Every action taken against this account, newest first. */
    val events: List<ModerationEvent> = emptyList(),
    /** What the user wants the administrators to know. */
    val statement: String = "",
    /** Whether the last attempt was refused for saying nothing. */
    val statementMissing: Boolean = false,
    /** Where the submission has got to. */
    val phase: AppealPhase = AppealPhase.Editing,
    /** Whether the master key failed to sign the challenge, so nothing was sent. */
    val proofRejected: Boolean = false,
    /** The handle the user can quote when following up. */
    val reference: String? = null,
) {
    /** The entry this appeal is against, or `null` when the id names nothing on the record. */
    val case: ModerationEvent? get() = events.firstOrNull { it.id.value == caseId }

    /** Whether the account is under an administrative suspension right now. */
    val isSuspended: Boolean get() = effects != null

    /** The record has been read and holds nothing. */
    val isEmpty: Boolean get() = !isLoading && failure == null && events.isEmpty()

    /** The record was read, but the id the destination carried is not on it. */
    val caseMissing: Boolean get() = !isLoading && failure == null && events.isNotEmpty() && case == null

    /** Whether a proof can be gathered and an appeal sent. */
    val canSubmit: Boolean
        get() = case != null && phase != AppealPhase.Proving && phase != AppealPhase.Sending
}
