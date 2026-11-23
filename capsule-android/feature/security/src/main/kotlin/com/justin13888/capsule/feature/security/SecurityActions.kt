package com.justin13888.capsule.feature.security

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.model.DeviceId
import com.justin13888.capsule.core.model.SessionId

/** Every decision the sessions screen hands back. */
@Suppress("LongParameterList")
@Immutable
class SessionsActions(
    /** Ends one session. Authenticated by any active session. */
    val onRevokeSession: (SessionId) -> Unit,
    /** Retires one enrolled device. Its directory row is kept. */
    val onRevokeDevice: (DeviceId) -> Unit,
    /** Opens the confirmation for the global revoke. */
    val onAskRevokeAll: () -> Unit,
    /** Produces the master-key proof and runs the global revoke. */
    val onConfirmRevokeAll: () -> Unit,
    /** Closes the confirmation without revoking anything. */
    val onDismissRevokeAll: () -> Unit,
    /** Builds the support report for one cohort. */
    val onReportCohort: (String?) -> Unit,
    /** Closes the support report. */
    val onDismissReport: () -> Unit,
    /** Re-reads the ledger after a failure. */
    val onRetry: () -> Unit,
)

/** Every decision the authenticator section hands back. */
@Immutable
class TotpActions(
    /** Starts an enrollment and reveals the secret. */
    val onBegin: () -> Unit,
    /** Records what the user typed into the six-digit field. */
    val onCodeChange: (String) -> Unit,
    /** Proves the authenticator produces matching codes. */
    val onConfirm: () -> Unit,
    /** Abandons an unconfirmed enrollment. */
    val onCancel: () -> Unit,
    /** Turns the factor off, which costs a code. */
    val onDisable: () -> Unit,
)
