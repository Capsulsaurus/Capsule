package com.justin13888.capsule.feature.onboarding

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/**
 * Signing in — or registering — along whichever path the server offered and the user picked.
 *
 * One state machine for all four paths rather than four screens, because they differ only in
 * which control the user touches: each ends in the same session, and each surfaces its
 * refusals through the same `error.*` code. Splitting them would duplicate the failure
 * handling three times, which is where the subtle divergence would start.
 */
class SignInViewModel(
    private val gateway: AuthGateway,
    private val session: OnboardingSession,
) : ViewModel() {
    private val mutableState =
        MutableStateFlow(
            SignInUiState(
                method = session.progress.value.authMethod ?: AuthMethod.PasswordTotp,
                stage = stageFor(session.progress.value.authMethod ?: AuthMethod.PasswordTotp),
            ),
        )

    /** Everything the sign-in screen draws. */
    val state: StateFlow<SignInUiState> = mutableState.asStateFlow()

    /** Records the typed handle. */
    fun emailChanged(email: String) = mutableState.update { it.copy(email = email, failure = null) }

    /** Records the typed password. */
    fun passwordChanged(password: String) = mutableState.update { it.copy(password = password, failure = null) }

    /** Records the typed one-time code. */
    fun totpCodeChanged(code: String) = mutableState.update { it.copy(totpCode = code, failure = null) }

    /**
     * Switches between using an existing account and creating one.
     *
     * The typed handle survives the switch — a user who discovers mid-form that they have no
     * account should not have to type their address again to make one.
     */
    fun toggleRegistering() {
        mutableState.update {
            it.copy(isRegistering = !it.isRegistering, password = "", failure = null)
        }
    }

    /** Submits the first factor, or the new account. */
    fun submitCredentials() {
        val current = mutableState.value
        if (!current.canSubmitCredentials) return
        perform {
            if (current.isRegistering) {
                gateway.register(current.email, current.password)
                AuthStep.Complete
            } else {
                gateway.signIn(current.email, current.password)
            }
        }
    }

    /** Answers the time-based challenge. */
    fun submitTotp() {
        if (!mutableState.value.canSubmitTotp) return
        val code = mutableState.value.totpCode
        perform {
            gateway.verifyTotp(code)
            AuthStep.Complete
        }
    }

    /** Runs the platform passkey ceremony. */
    fun authenticateWithPasskey() =
        perform {
            gateway.authenticateWithPasskey()
            AuthStep.Complete
        }

    /** Completes the hand-off to the external identity provider. */
    fun completeOidc() =
        perform {
            gateway.completeOidc()
            AuthStep.Complete
        }

    /** Clears a refusal so the user can try the same path again. */
    fun dismissFailure() = mutableState.update { it.copy(failure = null) }

    /**
     * Runs one authentication call and folds its answer into the state.
     *
     * Every path funnels through here so that "working", "refused" and "there is a session
     * now" are decided in exactly one place. A per-path copy of this would be four chances to
     * forget to clear the in-flight flag on the failure branch.
     */
    private fun perform(call: suspend () -> AuthStep) {
        mutableState.update { it.copy(isWorking = true, failure = null) }
        viewModelScope.launch {
            runCatching { call() }
                .onSuccess { step -> mutableState.update { it.advanced(step) } }
                .onFailure { failure ->
                    mutableState.update { it.copy(isWorking = false, failure = OnboardingFailure.of(failure)) }
                }
        }
    }

    private fun SignInUiState.advanced(step: AuthStep): SignInUiState =
        when (step) {
            AuthStep.TotpRequired -> copy(isWorking = false, stage = SignInStage.Totp, failure = null)
            AuthStep.Complete -> {
                session.signedIn()
                copy(isWorking = false, isComplete = true, failure = null)
            }
        }

    private companion object {
        /** The first stage of [method]. */
        fun stageFor(method: AuthMethod): SignInStage =
            when (method) {
                AuthMethod.Passkey -> SignInStage.Passkey
                AuthMethod.Oidc -> SignInStage.Oidc
                AuthMethod.PasswordTotp -> SignInStage.Credentials
            }
    }
}
