package com.justin13888.capsule.feature.recovery

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.security.SecureRandom

/** The steps of the guided re-wrap, in the order they happen. */
enum class RewrapStage {
    /** Saying what this does and, more importantly, what it does not do. */
    Explaining,

    /** The new secret, shown once. */
    Revealing,

    /** The type-back gate, which is a gate rather than a suggestion. */
    TypingBack,

    /** Replacing the escrow. */
    Working,

    /** Done, with whatever the user still has to deal with. */
    Finished,

    /** It did not complete, and the old secret is still the live one. */
    Failed,
}

/**
 * Everything the guided re-wrap draws, as one value.
 *
 * The type-back challenge is a *sample* of the phrase rather than the whole thing: retyping
 * twenty-four words proves patience, whereas three words drawn at random prove the list was
 * written down, which is the thing that actually needs proving.
 */
@Immutable
data class RecoveryRewrapUiState(
    /** Which step is on screen. */
    val stage: RewrapStage = RewrapStage.Explaining,
    /** The minted secret, once there is one. Never persisted. */
    val secret: RecoverySecret? = null,
    /** The word positions the user must type back, one-based. */
    val challenges: List<Int> = emptyList(),
    /** How far through [challenges] the user is. */
    val challengeIndex: Int = 0,
    /** What the user has typed for the current challenge. */
    val typed: String = "",
    /** Whether the last answer was wrong. */
    val typedWrong: Boolean = false,
    /** Whether guardian shares existed and have been invalidated. */
    val sharesInvalidated: Boolean = false,
    /** The failure to explain, or `null`. */
    val failure: RecoveryFailure? = null,
) {
    /** The one-based position the user is being asked for, or `null` when there is none left. */
    val currentChallenge: Int? get() = challenges.getOrNull(challengeIndex)

    /** Whether an answer can be submitted. */
    val canConfirm: Boolean get() = typed.isNotBlank() && currentChallenge != null
}

/**
 * The guided re-wrap's state machine.
 *
 * The thing this class exists to keep true is stated in its first screen and enforced by its
 * shape: it replaces the *wrap*, not the key. There is no path through it that re-encrypts an
 * asset, rewrites a blob or touches the library, because the only side effects it can have
 * are minting a secret, replacing one escrow blob, and discarding guardian shares that the
 * replacement has just made meaningless.
 *
 * The type-back gate is enforced here rather than in the screen. A gate implemented in a
 * composable is a gate that a second entry point silently skips.
 */
class RecoveryRewrapViewModel(
    private val gateway: RecoveryRewrapGateway,
    private val random: SecureRandom = SecureRandom(),
) : ViewModel() {
    private val mutableState = MutableStateFlow(RecoveryRewrapUiState())

    /** Everything the screen draws. */
    val state: StateFlow<RecoveryRewrapUiState> = mutableState.asStateFlow()

    /**
     * Mints the new secret and shows it.
     *
     * The challenge positions are drawn now rather than when the type-back starts, so the user
     * cannot learn which words matter by leaving and coming back.
     */
    fun begin() {
        val secret = gateway.mint()
        val positions =
            generateSequence { random.nextInt(secret.words.size) + 1 }
                .distinct()
                .take(minOf(CHALLENGE_COUNT, secret.words.size))
                .sorted()
                .toList()
        mutableState.update {
            it.copy(
                stage = RewrapStage.Revealing,
                secret = secret,
                challenges = positions,
                challengeIndex = 0,
                typed = "",
                typedWrong = false,
                failure = null,
            )
        }
    }

    /** Leaves the reveal step for the type-back gate. */
    fun onSecretWrittenDown() {
        if (mutableState.value.secret == null) return
        mutableState.update { it.copy(stage = RewrapStage.TypingBack) }
    }

    /** Edits the answer to the current challenge. */
    fun onTypedChange(value: String) {
        mutableState.update { it.copy(typed = value, typedWrong = false) }
    }

    /**
     * Checks one answer, and runs the re-wrap once every challenge has been answered.
     *
     * A wrong answer costs the attempt and nothing else — the same word is asked again — because
     * the gate exists to catch a phrase that was never written down, not to catch a typo.
     */
    fun confirm() {
        val current = mutableState.value
        val secret = current.secret ?: return
        val position = current.currentChallenge ?: return
        val expected = secret.words[position - 1]
        if (!current.typed.trim().equals(expected, ignoreCase = true)) {
            mutableState.update { it.copy(typedWrong = true) }
            return
        }
        val next = current.challengeIndex + 1
        if (next < current.challenges.size) {
            mutableState.update { it.copy(challengeIndex = next, typed = "", typedWrong = false) }
            return
        }
        mutableState.update { it.copy(stage = RewrapStage.Working, typed = "", typedWrong = false) }
        viewModelScope.launch {
            runCatching { gateway.rewrap(secret) }
                .onSuccess { receipt ->
                    mutableState.update {
                        it.copy(stage = RewrapStage.Finished, sharesInvalidated = receipt.sharesInvalidated)
                    }
                }.onFailure { throwable ->
                    mutableState.update {
                        it.copy(stage = RewrapStage.Failed, failure = RecoveryFailure.of(throwable))
                    }
                }
        }
    }

    /** Starts over from the explanation, discarding a secret that was never committed. */
    fun reset() {
        mutableState.value = RecoveryRewrapUiState()
    }

    private companion object {
        /** How many words the user is asked to type back. */
        const val CHALLENGE_COUNT = 3
    }
}
