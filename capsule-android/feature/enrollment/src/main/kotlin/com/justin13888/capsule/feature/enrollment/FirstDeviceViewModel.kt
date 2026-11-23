package com.justin13888.capsule.feature.enrollment

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.collections.immutable.toImmutableList
import kotlinx.collections.immutable.toImmutableMap
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlin.random.Random

/**
 * First-device setup: five steps, a passphrase, and the gate that will not let go of the user
 * until they have written it down.
 *
 * The gate is the reason this is one view model rather than three screens with three of them.
 * "Setup is complete" is a single fact, and it becomes true at exactly one point — a correct
 * type-back — so anything that could complete setup without passing through here would be a
 * second definition of done.
 */
class FirstDeviceViewModel(
    private val ceremony: FirstDeviceCeremony,
    private val random: Random = Random.Default,
) : ViewModel() {
    private val mutableState = MutableStateFlow(FirstDeviceUiState())

    /** Everything the setup screens draw. */
    val state: StateFlow<FirstDeviceUiState> = mutableState.asStateFlow()

    private var run: Job? = null

    init {
        start()
    }

    /** Runs the whole ceremony from wherever it currently is. */
    fun start() {
        run?.cancel()
        run =
            viewModelScope.launch {
                for (step in EnrollmentStep.entries) {
                    if (mutableState.value.statusOf(step) == StepStatus.Done) continue
                    if (!runStep(step)) return@launch
                }
                val passphrase = ceremony.recoveryPassphrase()
                mutableState.update { it.copy(phase = SetupPhase.Passphrase, passphrase = passphrase) }
            }
    }

    /** Retries one step without re-running the ones that already succeeded. */
    fun retry(step: EnrollmentStep) {
        mutableState.update { it.with(step, StepStatus.Pending).copy(failure = null) }
        start()
    }

    /**
     * Moves from the reveal to the gate and draws the words it will ask for.
     *
     * The challenge is drawn here rather than up front so that it cannot be read off the state
     * while the words are still on screen — a user who knew in advance which three positions
     * mattered would write down three words.
     */
    fun continueToTypeBack() {
        val passphrase = mutableState.value.passphrase ?: return
        mutableState.update {
            it.copy(
                phase = SetupPhase.TypeBack,
                challenge = TypeBackChallenge.draw(passphrase.words.size, random),
                answers = emptyMap<Int, String>().toImmutableMap(),
                typeBackRejected = false,
            )
        }
    }

    /** Goes back to the words. Allowed, and not a way past the gate. */
    fun backToPassphrase() {
        mutableState.update { it.copy(phase = SetupPhase.Passphrase, typeBackRejected = false) }
    }

    /** Records the clipboard copy, so the screen can say what to do next with it. */
    fun passphraseCopied() = mutableState.update { it.copy(copied = true) }

    /** Records one typed-back word. */
    fun answerChanged(
        position: Int,
        value: String,
    ) {
        mutableState.update {
            it.copy(answers = (it.answers + (position to value)).toImmutableMap(), typeBackRejected = false)
        }
    }

    /**
     * Checks the typed words.
     *
     * A wrong answer keeps the user on the gate. There is no "continue anyway", because the
     * whole point of the step is that dismissing it is what a user does with an advisory
     * screen and this one is not advisory.
     */
    fun submitTypeBack() {
        val current = mutableState.value
        val passphrase = current.passphrase ?: return
        val challenge = current.challenge ?: return
        if (challenge.accepts(passphrase, current.answers)) {
            // The passphrase leaves memory at the same moment it stops being needed; it was
            // never written anywhere, and there is nothing left here to read it out of.
            mutableState.update {
                it.copy(
                    phase = SetupPhase.Complete,
                    passphrase = null,
                    challenge = null,
                    answers = emptyMap<Int, String>().toImmutableMap(),
                    typeBackRejected = false,
                )
            }
        } else {
            mutableState.update { it.copy(typeBackRejected = true) }
        }
    }

    /** Runs one step. Returns whether the ceremony may continue past it. */
    private suspend fun runStep(step: EnrollmentStep): Boolean {
        mutableState.update { it.with(step, StepStatus.Running) }
        return runCatching { ceremony.perform(step) }
            .fold(
                onSuccess = {
                    mutableState.update { it.with(step, StepStatus.Done) }
                    true
                },
                onFailure = { throwable ->
                    val failure = EnrollmentFailure.of(step, throwable)
                    val recoverable = step in RECOVERABLE_STEPS
                    mutableState.update {
                        it
                            .with(step, if (recoverable) StepStatus.Deferred else StepStatus.Failed)
                            .copy(failure = failure)
                    }
                    recoverable
                },
            )
    }

    private fun FirstDeviceUiState.statusOf(step: EnrollmentStep): StepStatus = steps.first { it.step == step }.status

    private fun FirstDeviceUiState.with(
        step: EnrollmentStep,
        status: StepStatus,
    ): FirstDeviceUiState {
        val next = steps.map { if (it.step == step) it.copy(status = status) else it }
        return copy(steps = next.toImmutableList())
    }

    private companion object {
        /**
         * The two steps whose failure the doc says setup survives.
         *
         * The directory upload retries in the background against keys that are already valid,
         * and the default album's identifier is derivable from the master key, so any device
         * recreates it before the first import.
         */
        val RECOVERABLE_STEPS = setOf(EnrollmentStep.DeviceDirectory, EnrollmentStep.DefaultAlbum)
    }
}
