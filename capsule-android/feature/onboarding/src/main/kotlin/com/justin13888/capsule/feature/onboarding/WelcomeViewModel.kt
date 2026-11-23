package com.justin13888.capsule.feature.onboarding

import androidx.lifecycle.ViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/**
 * The welcome tour and the mode choice it leads to.
 *
 * The mode choice is recorded in the shared [OnboardingSession] rather than kept here,
 * because the completion screen has to know which of the two libraries it is congratulating
 * the user on, and that screen is a different destination with its own view-model store.
 */
class WelcomeViewModel(
    private val session: OnboardingSession,
) : ViewModel() {
    private val mutableState = MutableStateFlow(WelcomeUiState())

    /** Everything the welcome screen draws. */
    val state: StateFlow<WelcomeUiState> = mutableState.asStateFlow()

    /** Moves to [page] of the tour, ignoring anything outside it. */
    fun showPane(page: Int) {
        if (page !in 0 until WELCOME_PANES) return
        mutableState.update { it.copy(page = page) }
    }

    /** Advances one pane, or reveals the mode choice from the last one. */
    fun advance() {
        mutableState.update { current ->
            if (current.hasNextPane) current.copy(page = current.page + 1) else current.copy(choosingMode = true)
        }
    }

    /**
     * Skips the tour.
     *
     * Skipping lands on the mode choice rather than past it. The tour is optional; deciding
     * how this library exists is not, and there is nothing beyond this screen until it is
     * decided.
     */
    fun skipTour() {
        mutableState.update { it.copy(choosingMode = true) }
    }

    /** Returns to the tour from the mode choice. */
    fun backToTour() {
        mutableState.update { it.copy(choosingMode = false) }
    }

    /** Records the chosen mode. Both branches are ordinary paths forward. */
    fun choose(mode: OnboardingMode) = session.chooseMode(mode)
}
