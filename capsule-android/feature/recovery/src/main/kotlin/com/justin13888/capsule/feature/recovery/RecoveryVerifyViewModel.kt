package com.justin13888.capsule.feature.recovery

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.RecoveryRepository
import com.justin13888.capsule.core.model.RecoveryState
import com.justin13888.capsule.core.model.RecoveryVerifyOutcome
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/**
 * The local verification's state machine.
 *
 * Two properties of this class are load-bearing and neither is visible from its API, so they
 * are stated here. First, nothing it does reaches the network: the repository's check is a
 * derived-tag comparison against a cached escrow blob, which is what lets a user confirm
 * their secret on a plane, and what stops the check being a guessing oracle for anyone who
 * can reach the server. Second, a miss changes nothing — no counter the rest of the app
 * reads, no lock, no refusal. The only thing repeated misses buy is an offer.
 *
 * The typed secret is handed over as a [CharArray] and wiped immediately afterwards. The
 * `String` in the UI state cannot be wiped, which is a limitation of every Compose text
 * field; clearing the array is still worth doing, because it is the copy that would otherwise
 * outlive this screen inside the repository's own frame.
 */
class RecoveryVerifyViewModel(
    private val recovery: RecoveryRepository,
) : ViewModel() {
    private val mutableState = MutableStateFlow(RecoveryVerifyUiState())

    /** Everything the screen draws. */
    val state: StateFlow<RecoveryVerifyUiState> = mutableState.asStateFlow()

    init {
        // The next-prompt date is the cadence's own derivation rather than one computed here:
        // two answers to "when will you ask me again" that could disagree is one too many.
        viewModelScope.launch {
            recovery
                .recoveryState()
                .catch { throwable -> mutableState.update { it.copy(failure = RecoveryFailure.of(throwable)) } }
                .collect { current ->
                    mutableState.update { it.copy(nextPromptAt = (current as? RecoveryState.Healthy)?.nextPromptAt) }
                }
        }
    }

    /** Edits the passphrase. */
    fun onSecretChange(value: String) {
        mutableState.update { it.copy(secret = value, secretMissing = false) }
    }

    /** Shows or masks what was typed, for a user checking a long phrase against paper. */
    fun onRevealChange(value: Boolean) {
        mutableState.update { it.copy(revealed = value) }
    }

    /**
     * Runs the check.
     *
     * The repository refreshes a stale escrow blob and retries once before reporting a miss,
     * so a rotation performed on another device cannot manufacture a false failure here. That
     * happens below this class, which is why there is no retry of its own — a second one would
     * double-count the attempt.
     */
    fun verify() {
        val current = mutableState.value
        if (current.phase == VerifyPhase.Checking) return
        if (current.secret.isBlank()) {
            mutableState.update { it.copy(secretMissing = true) }
            return
        }
        mutableState.update { it.copy(phase = VerifyPhase.Checking, failure = null, secretMissing = false) }
        val entered = current.secret.toCharArray()
        viewModelScope.launch {
            runCatching { recovery.verifyRecoverySecret(entered) }
                .also { entered.fill(BLANK) }
                .onSuccess { outcome -> mutableState.update { it.settled(outcome) } }
                .onFailure { throwable ->
                    mutableState.update {
                        it.copy(phase = VerifyPhase.Failed, failure = RecoveryFailure.of(throwable))
                    }
                }
        }
    }

    /** Clears the field and the outcome, for another attempt. */
    fun reset() {
        mutableState.update {
            it.copy(secret = "", phase = VerifyPhase.Editing, failure = null, secretMissing = false)
        }
    }

    private fun RecoveryVerifyUiState.settled(outcome: RecoveryVerifyOutcome): RecoveryVerifyUiState =
        when (outcome) {
            RecoveryVerifyOutcome.Verified ->
                copy(phase = VerifyPhase.Verified, secret = "", consecutiveFailures = 0)

            RecoveryVerifyOutcome.NotVerified ->
                copy(phase = VerifyPhase.NotVerified, consecutiveFailures = consecutiveFailures + 1)
        }

    private companion object {
        /** What the wiped character array is filled with. */
        const val BLANK = ' '
    }
}
