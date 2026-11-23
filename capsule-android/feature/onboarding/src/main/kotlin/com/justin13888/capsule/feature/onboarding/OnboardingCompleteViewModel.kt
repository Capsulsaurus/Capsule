package com.justin13888.capsule.feature.onboarding

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/**
 * The media-permission primer and the hand-off into the library.
 *
 * The primer sits at the end rather than at the start because it is the one prompt whose
 * answer the user can only judge once they know what the app is for. It is also the last
 * place a mode is still visible, which is why the completion copy is chosen from it: telling
 * a never-signed-in user that their server is connected would be the first lie the app tells.
 */
class OnboardingCompleteViewModel(
    private val probe: MediaAccessProbe,
    session: OnboardingSession,
) : ViewModel() {
    private val mutableState =
        MutableStateFlow(
            CompletionUiState(
                mode = session.progress.value.mode ?: OnboardingMode.LocalOnly,
                access = probe.current(),
            ),
        )

    /** Everything the completion screen draws. */
    val state: StateFlow<CompletionUiState> = mutableState.asStateFlow()

    /** The permissions to hand the system prompt on this OS version. */
    fun requestedPermissions(): List<String> = probe.requestedPermissions()

    /**
     * Re-reads the platform after a prompt closed.
     *
     * The prompt's own result is deliberately ignored in favour of asking the platform again:
     * on Android 14 a user who picks photos rather than granting everything produces a
     * result map full of denials while the app has, in fact, gained partial access.
     */
    fun promptClosed() {
        viewModelScope.launch {
            val access = probe.current()
            mutableState.update { it.copy(access = access, hasAsked = true) }
        }
    }
}
