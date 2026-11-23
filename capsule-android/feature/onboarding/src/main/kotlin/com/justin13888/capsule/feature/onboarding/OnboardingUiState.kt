package com.justin13888.capsule.feature.onboarding

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.model.ErrorCode

/**
 * A failure reduced to the one thing a screen may show: its catalogue code.
 *
 * The transport status and the English detail are deliberately dropped. A user cannot act on
 * `401`, and the same condition is spelled differently by two deployments; the `error.*` code
 * is the contract, and the client localizes it. The code is nullable because a purely local
 * failure genuinely has none.
 */
@Immutable
data class OnboardingFailure(
    /** The catalogue code to localize, or `null` for a device-local failure. */
    val code: ErrorCode?,
) {
    internal companion object {
        /** Keeps only what the user can act on out of whatever a port threw. */
        fun of(throwable: Throwable): OnboardingFailure = OnboardingFailure((throwable as? CapsuleError)?.code)
    }
}

/**
 * The welcome carousel and the mode choice behind it.
 *
 * One state rather than two screens because they are one decision with a preamble: the tour
 * exists to make the mode choice legible, and a user who skips it lands on the same choice
 * rather than somewhere else.
 */
@Immutable
data class WelcomeUiState(
    /** Which pane of the tour is showing. */
    val page: Int = 0,
    /** How many panes there are. */
    val paneCount: Int = WELCOME_PANES,
    /**
     * Whether the tour is behind the user and the mode choice is on screen.
     *
     * Reached either by paging to the end or by skipping, because skipping the tour is not
     * skipping the decision — there is nothing further on until the mode is chosen.
     */
    val choosingMode: Boolean = false,
) {
    /** Whether the tour has a pane after the current one. */
    val hasNextPane: Boolean get() = page < paneCount - 1
}

/** How many panes the welcome tour has. */
const val WELCOME_PANES: Int = 3

/** Where the server screen is in its probe-then-choose sequence. */
@Immutable
data class ServerUiState(
    /** What the user has typed. */
    val address: String = "",
    /** Whether a probe is in flight. */
    val isProbing: Boolean = false,
    /** Whether the typed address is not something this app can probe at all. */
    val isAddressInvalid: Boolean = false,
    /** What the server said, once it answered. */
    val info: ServerInfo? = null,
    /** The path the user picked out of [ServerInfo.authMethods]. */
    val selectedMethod: AuthMethod? = null,
    /** The failure to explain, or `null`. */
    val failure: OnboardingFailure? = null,
) {
    /** The server answered but offers nothing this build can drive. */
    val hasNoUsableMethod: Boolean get() = info != null && info.authMethods.isEmpty()

    /** Whether the screen may move on to signing in. */
    val canContinue: Boolean get() = info != null && selectedMethod != null
}

/** Which part of the chosen auth path the user is on. */
enum class SignInStage {
    /** Entering the first factor, or the details for a new account. */
    Credentials,

    /** Answering the time-based challenge the password path raised. */
    Totp,

    /** The platform passkey ceremony is open. */
    Passkey,

    /** The hand-off to an external identity provider is open. */
    Oidc,
}

/** Everything the sign-in screen draws. */
@Immutable
data class SignInUiState(
    /** The auth path chosen on the server screen. */
    val method: AuthMethod = AuthMethod.PasswordTotp,
    /** Which part of that path is on screen. */
    val stage: SignInStage = SignInStage.Credentials,
    /** Whether the form creates an account rather than using an existing one. */
    val isRegistering: Boolean = false,
    /** The typed handle. */
    val email: String = "",
    /** The typed password. */
    val password: String = "",
    /** The typed one-time code. */
    val totpCode: String = "",
    /** Whether a call is in flight. */
    val isWorking: Boolean = false,
    /** The failure to explain, or `null`. */
    val failure: OnboardingFailure? = null,
    /** Whether a session now exists and the flow may move on. */
    val isComplete: Boolean = false,
) {
    /** Whether the credential form has enough to submit. */
    val canSubmitCredentials: Boolean
        get() = email.isNotBlank() && password.isNotBlank() && !isWorking

    /** Whether the one-time code field has enough to submit. */
    val canSubmitTotp: Boolean get() = totpCode.isNotBlank() && !isWorking
}

/** The media-permission primer and the hand-off out of onboarding. */
@Immutable
data class CompletionUiState(
    /** The mode the library will run in. */
    val mode: OnboardingMode = OnboardingMode.LocalOnly,
    /** What this app may read right now. */
    val access: MediaAccess = MediaAccess.None,
    /** Whether the system prompt has been raised at least once. */
    val hasAsked: Boolean = false,
) {
    /** Whether the primer — rather than an outcome — is what to show. */
    val showingPrimer: Boolean get() = !hasAsked && access == MediaAccess.None
}
