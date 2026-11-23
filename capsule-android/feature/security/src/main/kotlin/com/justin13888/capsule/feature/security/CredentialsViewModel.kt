package com.justin13888.capsule.feature.security

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/**
 * The state machine behind the passkey list and the authenticator section.
 *
 * One view model over both, because the only interesting cross-check on this screen spans
 * them: whether removing a passkey would leave the account with nothing but its master key.
 * Splitting them would leave that warning reading half the answer.
 */
class CredentialsViewModel(
    private val credentials: CredentialRepository,
) : ViewModel() {
    private val mutableState = MutableStateFlow(CredentialsUiState())

    /** Everything the passkey and authenticator surfaces draw. */
    val state: StateFlow<CredentialsUiState> = mutableState.asStateFlow()

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
                combine(credentials.passkeys(), credentials.totp()) { passkeys, totp -> passkeys to totp }
                    .catch { throwable -> mutableState.update { it.failed(throwable) } }
                    .collect { (passkeys, totp) ->
                        mutableState.update {
                            it.copy(isLoading = false, failure = null, passkeys = passkeys, totp = totp)
                        }
                    }
            }
    }

    /** Registers a passkey with the platform authenticator. */
    fun addPasskey() {
        write { credentials.addPasskey() }
    }

    /**
     * Removes one passkey.
     *
     * Not refused when it is the last one. The screen warns first, and then it is the user's
     * decision: their account key still opens the account, and a client that vetoed the
     * removal of a credential the user owns would be deciding something that is not its to
     * decide.
     */
    fun deletePasskey(id: PasskeyId) {
        write { credentials.deletePasskey(id) }
    }

    /** Records what the user has typed into the six-digit field. */
    fun onCodeChange(code: String) {
        mutableState.update { it.copy(code = code.filter(Char::isDigit).take(CODE_LENGTH), failure = null) }
    }

    /** Starts an enrollment and hands over the secret to put in an authenticator. */
    fun beginTotpEnrollment() {
        write { credentials.beginTotpEnrollment() }
    }

    /** Proves the authenticator generates matching codes, which is what activates the factor. */
    fun confirmTotp() {
        val code = mutableState.value.code
        write(clearCodeOnSuccess = true) { credentials.confirmTotp(code) }
    }

    /** Abandons an unconfirmed enrollment, discarding its secret. */
    fun cancelTotpEnrollment() {
        write(clearCodeOnSuccess = true) { credentials.cancelTotpEnrollment() }
    }

    /** Turns the factor off, which costs a current code exactly as turning it on did. */
    fun disableTotp() {
        val code = mutableState.value.code
        write(clearCodeOnSuccess = true) { credentials.disableTotp(code) }
    }

    /**
     * Runs one credential write, reporting its outcome in the state.
     *
     * The code field survives a failure and is cleared on success: a user who mistyped one
     * digit should be able to fix that digit, and a user who succeeded should not be left
     * looking at a code that is already spent.
     */
    private fun write(
        clearCodeOnSuccess: Boolean = false,
        block: suspend () -> Unit,
    ) {
        if (mutableState.value.isBusy) return
        mutableState.update { it.copy(isBusy = true, failure = null) }
        viewModelScope.launch {
            runCatching { block() }
                .onSuccess {
                    mutableState.update {
                        it.copy(isBusy = false, code = if (clearCodeOnSuccess) "" else it.code)
                    }
                }.onFailure { throwable ->
                    mutableState.update { it.copy(isBusy = false, failure = SecurityFailure.of(throwable)) }
                }
        }
    }

    private fun CredentialsUiState.failed(throwable: Throwable): CredentialsUiState =
        copy(isLoading = false, failure = SecurityFailure.of(throwable))

    private companion object {
        /** How many digits an authenticator code has. */
        const val CODE_LENGTH = 6
    }
}
