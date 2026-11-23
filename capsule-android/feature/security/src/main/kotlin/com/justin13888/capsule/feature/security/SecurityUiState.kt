package com.justin13888.capsule.feature.security

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.model.Device
import com.justin13888.capsule.core.model.ErrorCode

/**
 * A failure a security screen has to explain.
 *
 * Three shapes rather than a nullable code, because the three call for different copy and the
 * screen must not have to guess: a coded refusal is localized from the catalogue, a rejected
 * authenticator code has its own message, and anything else falls back to the generic one.
 */
@Immutable
sealed interface SecurityFailure {
    /** A refusal the server named. Localized from the `error.*` catalogue. */
    data class Coded(
        /** The stable catalogue key. */
        val code: ErrorCode,
    ) : SecurityFailure

    /** The six-digit code did not match. */
    data object TotpCode : SecurityFailure

    /** Something with no code and no specific advice. */
    data object Unclassified : SecurityFailure

    companion object {
        /** Keeps only what the user can act on out of whatever a repository threw. */
        fun of(throwable: Throwable): SecurityFailure {
            if (throwable is TotpCodeRejected) return TotpCode
            val code = (throwable as? CapsuleError)?.code
            return if (code == null) Unclassified else Coded(code)
        }
    }
}

/** How far the global revoke has got. */
@Immutable
sealed interface RevokeAllState {
    /** Not asked for. */
    data object Idle : RevokeAllState

    /** The confirmation is open and explaining why this one needs the account key. */
    data object Confirming : RevokeAllState

    /** The proof is being produced and presented. */
    data object Proving : RevokeAllState

    /** Every session was ended. */
    data object Done : RevokeAllState

    /**
     * It was refused, and nothing was revoked.
     *
     * The failure is carried here rather than in the screen's general failure slot because the
     * all-or-nothing property is the point: there is no partial success to reconcile, and the
     * confirmation has to be able to say so where the user is looking.
     */
    data class Refused(
        /** Why. */
        val failure: SecurityFailure,
    ) : RevokeAllState
}

/**
 * Everything the sessions-and-devices screen draws.
 *
 * The cohort groups are derived once here rather than in the composition so that "what is on
 * screen" and "what a support report would say" are decided in the same place — a report
 * assembled from a different grouping than the one the user is looking at would be worse than
 * no report.
 */
@Immutable
data class SessionsUiState(
    /** Whether a reading is in flight and nothing has arrived yet. */
    val isLoading: Boolean = true,
    /** A failure to explain, or `null`. */
    val failure: SecurityFailure? = null,
    /** Sessions grouped by the device each appears to come from. */
    val groups: List<SessionCohortGroup> = emptyList(),
    /** The signed device directory, revoked rows included. */
    val devices: List<Device> = emptyList(),
    /** Where the global revoke stands. */
    val revokeAll: RevokeAllState = RevokeAllState.Idle,
    /** The support report the user asked for, or `null` when the sheet is closed. */
    val supportReport: String? = null,
) {
    /** No sessions at all — signed out, or a ledger that has not arrived. */
    val hasNoSessions: Boolean get() = !isLoading && failure == null && groups.isEmpty()

    /** How many sessions are listed, across every group. */
    val sessionCount: Int get() = groups.sumOf(SessionCohortGroup::sessionCount)

    /** How many devices the directory still counts as active. */
    val activeDeviceCount: Int get() = devices.count(Device::isActive)

    /** One group by its hash, for a callback that only holds the key. */
    fun group(cohortHash: String?): SessionCohortGroup? = groups.firstOrNull { it.cohortHash == cohortHash }
}

/**
 * Everything the passkeys list and the TOTP section draw.
 *
 * One state across both because they answer one question — "what proves it is you?" — and the
 * warning before deleting the last passkey depends on whether TOTP is on. Two states would let
 * that warning read one half of an answer.
 */
@Immutable
data class CredentialsUiState(
    /** Whether a reading is in flight and nothing has arrived yet. */
    val isLoading: Boolean = true,
    /** A failure to explain, or `null`. */
    val failure: SecurityFailure? = null,
    /** Registered passkeys, newest first. */
    val passkeys: List<Passkey> = emptyList(),
    /** The current enrollment, confirmed or not, or `null` when TOTP is off. */
    val totp: TotpEnrollment? = null,
    /** What the user has typed into the code field. */
    val code: String = "",
    /** Whether a credential write is in flight. */
    val isBusy: Boolean = false,
) {
    /** Nothing registered — the state a password-only account is in. */
    val hasNoPasskeys: Boolean get() = !isLoading && failure == null && passkeys.isEmpty()

    /** Whether the authenticator factor is actually active. */
    val isTotpActive: Boolean get() = totp?.isConfirmed == true

    /** Whether a secret has been handed over and is waiting to be proved. */
    val isEnrolling: Boolean get() = totp != null && !totp.isConfirmed

    /**
     * Whether removing a passkey would leave the account with no second factor at all.
     *
     * Worth warning about and deliberately not worth blocking: the account key still opens the
     * account, and a client that refused to let a user remove their own credential would be
     * deciding something that is theirs to decide.
     */
    val isLastFactor: Boolean get() = passkeys.size == 1 && !isTotpActive
}
