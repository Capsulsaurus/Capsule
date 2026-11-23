package com.justin13888.capsule.feature.security

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.SessionRepository
import com.justin13888.capsule.core.model.Device
import com.justin13888.capsule.core.model.DeviceId
import com.justin13888.capsule.core.model.SessionId
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/**
 * The session ledger's state machine.
 *
 * The two revokes are separate methods with separate shapes on purpose, and the shapes carry
 * the whole point of the design: [revokeSession] is authenticated by any active session and
 * needs nothing from the caller, while [confirmRevokeAll] goes and gets a master-key proof
 * first. A single `revoke(scope)` would hide the asymmetry that stops a stolen session token
 * from locking the real user out of every other device.
 */
class SessionsViewModel(
    private val sessions: SessionRepository,
    private val proofSource: MasterKeyProofSource,
) : ViewModel() {
    private val mutableState = MutableStateFlow(SessionsUiState())

    /** Everything the sessions screen draws. */
    val state: StateFlow<SessionsUiState> = mutableState.asStateFlow()

    private var collection: Job? = null

    init {
        reload()
    }

    /** Restarts the reading. Also the remediation behind the error state's action. */
    fun reload() {
        collection?.cancel()
        mutableState.update { it.copy(isLoading = true, failure = null) }
        collection =
            viewModelScope.launch {
                combine(
                    sessions.sessions(),
                    sessions.cohorts(),
                    sessions.devices(),
                ) { ledger, cohorts, devices ->
                    SessionsUiState(
                        isLoading = false,
                        groups = SessionCohorts.group(ledger, cohorts),
                        devices = devices.sortedWith(DEVICE_ORDER),
                    )
                }.catch { throwable -> mutableState.update { it.failed(throwable) } }
                    .collect { next ->
                        mutableState.update { current ->
                            next.copy(revokeAll = current.revokeAll, supportReport = current.supportReport)
                        }
                    }
            }
    }

    /** Ends one session. Any active session may do this; nothing else is required. */
    fun revokeSession(id: SessionId) {
        viewModelScope.launch {
            runCatching { sessions.revokeSession(id) }
                .onFailure { throwable -> mutableState.update { it.failed(throwable) } }
        }
    }

    /** Retires one device. Its directory row stays, because the history it signed depends on it. */
    fun revokeDevice(id: DeviceId) {
        viewModelScope.launch {
            runCatching { sessions.revokeDevice(id) }
                .onFailure { throwable -> mutableState.update { it.failed(throwable) } }
        }
    }

    /** Opens the confirmation that explains why this revoke costs more than the others. */
    fun askRevokeAll() {
        mutableState.update { it.copy(revokeAll = RevokeAllState.Confirming) }
    }

    /** Closes the confirmation without revoking anything. */
    fun dismissRevokeAll() {
        mutableState.update { it.copy(revokeAll = RevokeAllState.Idle) }
    }

    /**
     * Ends every session, presenting a master-key proof rather than this session's token.
     *
     * A refusal is reported as a refusal of the whole thing. The call is all-or-nothing by
     * contract, so there is never a half-cleared local state to reconcile, and the screen must
     * not imply otherwise by leaving some rows crossed out.
     */
    fun confirmRevokeAll() {
        if (mutableState.value.revokeAll == RevokeAllState.Proving) return
        mutableState.update { it.copy(revokeAll = RevokeAllState.Proving) }
        viewModelScope.launch {
            runCatching {
                val proof = proofSource.proveMasterKey()
                sessions.revokeAllSessions(proof)
            }.onSuccess { mutableState.update { it.copy(revokeAll = RevokeAllState.Done) } }
                .onFailure { throwable ->
                    mutableState.update {
                        it.copy(revokeAll = RevokeAllState.Refused(SecurityFailure.of(throwable)))
                    }
                }
        }
    }

    /**
     * Builds the report for one cohort — the whole of the dispute path.
     *
     * Nothing is recorded against the cohort itself, deliberately: the user cannot adjudicate
     * a hash, so there is nothing here for them to assert. What they can do is hand a person
     * the exact identifiers, and that is what this produces.
     */
    fun openSupportReport(cohortHash: String?) {
        val group = mutableState.value.group(cohortHash) ?: return
        mutableState.update { it.copy(supportReport = SupportBundle.render(group)) }
    }

    /** Closes the support report sheet. */
    fun dismissSupportReport() {
        mutableState.update { it.copy(supportReport = null) }
    }

    private fun SessionsUiState.failed(throwable: Throwable): SessionsUiState =
        copy(isLoading = false, failure = SecurityFailure.of(throwable))

    private companion object {
        /**
         * This device, then the rest of the live directory, then the retired rows.
         *
         * Retired devices sort last rather than being hidden: their rows are never pruned, so
         * the list would otherwise open on history the user has to scroll past to find the
         * devices they can actually act on.
         */
        val DEVICE_ORDER =
            compareByDescending<Device> { it.isThisDevice }
                .thenByDescending { it.isActive }
                .thenByDescending { it.addedAt }
    }
}
