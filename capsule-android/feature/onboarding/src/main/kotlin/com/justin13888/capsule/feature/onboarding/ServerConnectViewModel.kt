package com.justin13888.capsule.feature.onboarding

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/**
 * Naming a server, reading what it says about itself, and picking a way in.
 *
 * The probe runs in a job this view model owns rather than in a `stateIn` pipeline, because
 * the error state's remediation is to *re-run* it: a pipeline that had already terminated
 * would leave a retry button that did nothing.
 */
class ServerConnectViewModel(
    private val probe: ServerProbe,
    private val session: OnboardingSession,
) : ViewModel() {
    private val mutableState = MutableStateFlow(ServerUiState())

    /** Everything the server screen draws. */
    val state: StateFlow<ServerUiState> = mutableState.asStateFlow()

    private var probing: Job? = null

    /** Records what the user typed, discarding any answer the previous address produced. */
    fun addressChanged(address: String) {
        probing?.cancel()
        mutableState.update {
            ServerUiState(address = address)
        }
    }

    /**
     * Probes the typed address.
     *
     * A structurally impossible address is refused here rather than sent, and reported as a
     * field problem rather than as a server failure — telling a user their server is
     * unreachable when they typed `photos` with no scheme would send them debugging the
     * wrong machine.
     */
    fun connect() {
        val address = mutableState.value.address.trim()
        if (!isProbableAddress(address)) {
            mutableState.update { it.copy(isAddressInvalid = true, info = null, failure = null) }
            return
        }
        probing?.cancel()
        mutableState.update { it.copy(isProbing = true, isAddressInvalid = false, failure = null, info = null) }
        probing =
            viewModelScope.launch {
                runCatching { probe.probe(address) }
                    .onSuccess { info -> mutableState.update { it.answered(info) } }
                    .onFailure { failure ->
                        mutableState.update { it.copy(isProbing = false, failure = OnboardingFailure.of(failure)) }
                    }
            }
    }

    /** Chooses one of the paths the server offers. */
    fun selectMethod(method: AuthMethod) {
        mutableState.update { current ->
            if (current.info?.authMethods?.contains(method) != true) current else current.copy(selectedMethod = method)
        }
    }

    /** Commits the server and the chosen path to the shared flow state. */
    fun commit() {
        val current = mutableState.value
        val info = current.info ?: return
        val method = current.selectedMethod ?: return
        session.chooseServer(current.address.trim(), info, method)
    }

    /** Falls back to a local-only library, which loses nothing that was already set up. */
    fun useLocalOnly() = session.chooseMode(OnboardingMode.LocalOnly)

    /**
     * Preselects the first offered path.
     *
     * A picker that opened with nothing chosen would make "continue" inert on a deployment
     * that offers exactly one path, which is the common self-hosted case.
     */
    private fun ServerUiState.answered(info: ServerInfo): ServerUiState =
        copy(isProbing = false, info = info, selectedMethod = info.authMethods.firstOrNull(), failure = null)

    private companion object {
        /** Schemes a Capsule client will speak. Plain HTTP is not one of them. */
        val SCHEMES = listOf("https://")

        /** Whether [address] is worth sending at all. */
        fun isProbableAddress(address: String): Boolean =
            SCHEMES.any { address.startsWith(it, ignoreCase = true) } &&
                address.substringAfter("://").substringBefore('/').contains('.')
    }
}
