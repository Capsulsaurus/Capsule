package com.justin13888.capsule.feature.onboarding

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/** Everything onboarding has learned so far, as one value. */
data class OnboardingProgress(
    /** The mode the user chose, or `null` before they have chosen. */
    val mode: OnboardingMode? = null,
    /** The address typed at the server screen, once its probe succeeded. */
    val serverAddress: String? = null,
    /** What that server said about itself. */
    val serverInfo: ServerInfo? = null,
    /** The auth path the user picked out of what the server offers. */
    val authMethod: AuthMethod? = null,
    /** Whether a session now exists. Always `false` in [OnboardingMode.LocalOnly]. */
    val signedIn: Boolean = false,
)

/**
 * The state onboarding carries between its screens.
 *
 * A process-wide holder rather than a shared view model because the app's navigation gives
 * each destination its own `ViewModelStore`: a view model resolved on the server screen is a
 * different instance from the one the sign-in screen resolves, so a chosen auth path stored
 * in one would be invisible to the other. Onboarding is also a once-per-install flow whose
 * answers must survive moving forwards and backwards through it, which is exactly the
 * lifetime a singleton has.
 *
 * Writes are single-field updates rather than a `set(progress)` so that a screen can only
 * record the answer it owns, and cannot silently roll back one it never asked about.
 */
class OnboardingSession {
    private val mutableProgress = MutableStateFlow(OnboardingProgress())

    /** What onboarding knows, observable so a screen recomposes when an earlier one answers. */
    val progress: StateFlow<OnboardingProgress> = mutableProgress.asStateFlow()

    /**
     * Records the mode choice.
     *
     * Choosing [OnboardingMode.LocalOnly] clears any server the user had already named, so
     * backing out of the server screen and choosing local does not leave a half-configured
     * server behind that the completion screen would then claim is connected.
     */
    fun chooseMode(mode: OnboardingMode) {
        mutableProgress.update { current ->
            if (mode == OnboardingMode.LocalOnly) {
                OnboardingProgress(mode = mode)
            } else {
                current.copy(mode = mode)
            }
        }
    }

    /** Records the server that answered, and the path the user picked out of what it offers. */
    fun chooseServer(
        address: String,
        info: ServerInfo,
        method: AuthMethod,
    ) {
        mutableProgress.update {
            it.copy(
                mode = OnboardingMode.ServerBacked,
                serverAddress = address,
                serverInfo = info,
                authMethod = method,
            )
        }
    }

    /** Records that a session now exists. */
    fun signedIn() {
        mutableProgress.update { it.copy(signedIn = true) }
    }
}
