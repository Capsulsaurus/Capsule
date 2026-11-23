package com.justin13888.capsule.feature.enrollment

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/** What the cross-device ceremony has established so far. */
data class EnrollmentProgress(
    /** Whether fresh physical control of this device has been proven this session. */
    val isAuthorized: Boolean = false,
    /** The code this device issued, if it is the one issuing. */
    val issuedCode: EnrollmentCode? = null,
    /** The code this device read or typed, if it is the one joining. */
    val redeemedCode: String? = null,
    /** How the ceremony ended, or `null` while it has not. */
    val outcome: EnrollmentOutcome? = null,
)

/**
 * The state the cross-device ceremony carries between its screens.
 *
 * A process-wide holder for the same reason onboarding has one: the app gives each
 * destination its own `ViewModelStore`, so a code issued on one screen would be invisible to
 * the screen that compares safety codes.
 *
 * Nothing here survives the process. That is deliberate — the doc's abandonment case is that
 * the code expires and no state is persisted, so a user who walks away starts over rather
 * than resuming a half-finished ceremony whose other end has long since timed out.
 */
class EnrollmentSession {
    private val mutableProgress = MutableStateFlow(EnrollmentProgress())

    /** What the ceremony knows, observable so a later screen sees an earlier screen's answer. */
    val progress: StateFlow<EnrollmentProgress> = mutableProgress.asStateFlow()

    /** Records that the platform confirmed physical control of this device. */
    fun authorized() = mutableProgress.update { it.copy(isAuthorized = true) }

    /** Records a freshly issued code, replacing any previous one. */
    fun issued(code: EnrollmentCode) = mutableProgress.update { it.copy(issuedCode = code) }

    /** Records the code this device read or typed. */
    fun redeemed(code: String) = mutableProgress.update { it.copy(redeemedCode = code) }

    /** Records how the ceremony ended. */
    fun finished(outcome: EnrollmentOutcome) = mutableProgress.update { it.copy(outcome = outcome) }

    /**
     * Throws away everything except the authorization.
     *
     * What "start over" means after a mismatch: the code and the channel are burned, but the
     * user has not stopped being in physical control of this device in the last few seconds,
     * so re-prompting for that would be friction with no security content.
     */
    fun restart() = mutableProgress.update { EnrollmentProgress(isAuthorized = it.isAuthorized) }
}
