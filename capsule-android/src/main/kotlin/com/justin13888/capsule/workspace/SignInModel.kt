package com.justin13888.capsule.workspace

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/** Why the sign-in form is showing a message, each a catalog string the screen names. */
enum class SignInMessage {
    /** The server field names no `https` origin. */
    InvalidAddress,

    /** `error.auth.invalid_credentials`. */
    CredentialsRejected,

    /** `error.auth.rate_limited`. */
    RateLimited,

    /** `error.auth.account_locked`. */
    AccountLocked,

    /** `error.auth.totp_invalid_code`. */
    CodeInvalid,

    /** `error.auth.totp_challenge_invalid`: the code step expired, so the password is asked again. */
    ChallengeExpired,

    /** `error.protocol.version_unsupported`. */
    ProtocolUnsupported,

    /** Anything else: the server could not be reached or did not say why. */
    Unavailable,
    ;

    companion object {
        /** The message for [failure], from its stable code where it has one. */
        fun of(failure: SignInFailure): SignInMessage =
            when {
                failure.kind == SignInFailure.Kind.InvalidAddress -> InvalidAddress
                else ->
                    when (failure.code) {
                        "error.auth.invalid_credentials" -> CredentialsRejected
                        "error.auth.rate_limited" -> RateLimited
                        "error.auth.account_locked" -> AccountLocked
                        "error.auth.totp_invalid_code" -> CodeInvalid
                        "error.auth.totp_challenge_invalid" -> ChallengeExpired
                        "error.protocol.version_unsupported" -> ProtocolUnsupported
                        else -> Unavailable
                    }
            }
    }
}

/** What the sign-in form draws. */
data class SignInState(
    val server: String = "",
    val handle: String = "",
    val password: String = "",
    val code: String = "",
    /** Whether the password verified and the form now asks for the second-factor code. */
    val askingForCode: Boolean = false,
    val isSubmitting: Boolean = false,
    val message: SignInMessage? = null,
    /** Signed in: the screen closes. */
    val isDone: Boolean = false,
)

/**
 * The sign-in form over [sessions]: the server, handle and password, then the
 * authenticator code when the account asks for one.
 *
 * The password is cleared from the state as soon as it is sent, whichever way the
 * attempt ends, so it is held only while it is being typed.
 */
class SignInModel(
    private val sessions: ServerSessions,
) {
    private val mutable = MutableStateFlow(SignInState(server = sessions.lastAddress?.origin.orEmpty()))
    private var pending: SignInStep.SecondFactorRequired? = null

    val state: StateFlow<SignInState> = mutable.asStateFlow()

    fun updateServer(value: String) = mutable.update { it.copy(server = value, message = null) }

    fun updateHandle(value: String) = mutable.update { it.copy(handle = value, message = null) }

    fun updatePassword(value: String) = mutable.update { it.copy(password = value, message = null) }

    fun updateCode(value: String) = mutable.update { it.copy(code = value, message = null) }

    /** Send the form: the password while none has verified, the code once one has. */
    suspend fun submit() {
        val current = mutable.value
        if (current.isSubmitting || current.isDone) return
        val step = pending
        if (step == null) signIn(current) else verify(step, current.code)
    }

    private suspend fun signIn(form: SignInState) {
        val server = ServerAddress.parse(form.server)
        if (server == null) {
            mutable.update { it.copy(message = SignInMessage.InvalidAddress) }
            return
        }
        mutable.update { it.copy(server = server.origin, password = "", isSubmitting = true, message = null) }
        try {
            when (val step = sessions.signIn(server, form.handle.trim(), form.password)) {
                SignInStep.Done -> mutable.update { it.copy(isSubmitting = false, isDone = true) }
                is SignInStep.SecondFactorRequired -> {
                    pending = step
                    mutable.update { it.copy(isSubmitting = false, askingForCode = true) }
                }
            }
        } catch (failure: SignInFailure) {
            mutable.update { it.copy(isSubmitting = false, message = SignInMessage.of(failure)) }
        }
    }

    private suspend fun verify(
        step: SignInStep.SecondFactorRequired,
        code: String,
    ) {
        mutable.update { it.copy(isSubmitting = true, message = null) }
        try {
            sessions.verifySecondFactor(step, code)
            pending = null
            mutable.update { it.copy(code = "", isSubmitting = false, isDone = true) }
        } catch (failure: SignInFailure) {
            val message = SignInMessage.of(failure)
            if (message == SignInMessage.ChallengeExpired) {
                // The challenge is good once and for five minutes: start again from the password.
                pending = null
                mutable.update { it.copy(code = "", askingForCode = false, isSubmitting = false, message = message) }
            } else {
                mutable.update { it.copy(isSubmitting = false, message = message) }
            }
        }
    }
}
