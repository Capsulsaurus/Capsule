package com.justin13888.capsule.feature.importer

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.ImportRepository
import com.justin13888.capsule.core.data.SyncRepository
import com.justin13888.capsule.core.model.ConnectionClass
import com.justin13888.capsule.core.model.ImportId
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.time.Duration

/**
 * The execution screen's state machine.
 *
 * The per-file ladder is driven **here**, on this view model's own clock, rather than read
 * out of the repository. The mock backend moves no bytes, so its `confirm` returns an
 * already-finished progress; rendering that directly would show every file jumping from
 * queued to done in one frame and would say nothing true about the pipeline. Driving
 * [ImportProgression] over a `delay`-based tick keeps the per-file states honest and leaves
 * the fixture untouched — and under `runTest` the tick is virtual, so the sequence is
 * asserted exactly rather than waited for.
 *
 * The repository stays authoritative for the two things it actually knows: whether the run
 * exists, and the coded failure that stopped it.
 */
class ImportRunViewModel(
    private val imports: ImportRepository,
    private val sync: SyncRepository,
    private val plans: ImportPlanStore,
    private val importId: ImportId,
    private val tick: Duration = DEFAULT_TICK,
) : ViewModel() {
    private val mutableState = MutableStateFlow(ImportRunUiState(importId = importId))

    /** Everything the execution screen draws. */
    val state: StateFlow<ImportRunUiState> = mutableState.asStateFlow()

    private var driver: Job? = null

    init {
        attach()
    }

    /** Re-attaches to the run. Also the error state's remediation. */
    fun attach() {
        driver?.cancel()
        val recorded = plans.get(importId)
        if (recorded == null) {
            mutableState.update { it.copy(isLoading = false, hasLocalFailure = true) }
            return
        }
        val streaming = recorded.plan.streamingRecommended == true
        val window = if (streaming) ImportProgression.STREAMING_WINDOW else ImportProgression.DEFAULT_WINDOW
        mutableState.update {
            ImportRunUiState(
                importId = importId,
                isLoading = false,
                progression = ImportProgression.of(recorded.plan.added, window),
                isStreaming = streaming,
                skippedCount = recorded.plan.skipped.size,
            )
        }
        driver =
            viewModelScope.launch {
                launch { watchFailures() }
                launch { watchConnection() }
                drive()
            }
    }

    /**
     * Stops the run.
     *
     * Files already imported stay imported. A cancelled run is a shorter run, not a rollback,
     * and under Move mode no source file is released for a file that did not finish.
     */
    fun cancel() {
        viewModelScope.launch {
            runCatching { imports.cancel(importId) }
                .onFailure { throwable -> mutableState.update { it.failed(throwable) } }
            mutableState.update { it.copy(isCancelled = true, progression = it.progression.cancelled()) }
        }
    }

    /**
     * Advances the ladder one rung per tick, and stops scheduling entirely while paused.
     *
     * Driven off the paused flag rather than polling it, so a paused run has no timer pending
     * at all. That is not just tidiness: a pause exists because the device must not take on
     * work it cannot release, and a loop that kept waking to re-check would keep the radio and
     * the scheduler alive for exactly as long as the pause lasted.
     */
    private suspend fun drive() {
        mutableState
            .map { it.progression.isPaused }
            .distinctUntilChanged()
            .collectLatest { paused ->
                if (paused) return@collectLatest
                while (!mutableState.value.progression.isFinished && !mutableState.value.isCancelled) {
                    delay(tick.toMillis())
                    mutableState.update { it.copy(progression = it.progression.advance()) }
                }
            }
    }

    private suspend fun watchFailures() {
        imports.progress(importId).collect { progress ->
            val code = progress.failure ?: return@collect
            mutableState.update { it.copy(failure = code, progression = it.progression.halt(code)) }
        }
    }

    /**
     * Pauses a streaming run while the server is unreachable.
     *
     * Streaming releases each local original only once the server confirms it holds it, so a
     * run that kept admitting files while offline would refill the very disk the mode exists
     * to spare. A non-streaming run has already reserved its room and simply carries on.
     */
    private suspend fun watchConnection() {
        sync.connectionClass().collect { connection ->
            val paused = mutableState.value.isStreaming && connection == ConnectionClass.Offline
            mutableState.update { it.copy(progression = it.progression.paused(paused)) }
        }
    }

    private fun ImportRunUiState.failed(throwable: Throwable): ImportRunUiState =
        copy(
            failure = (throwable as? CapsuleError)?.code,
            hasLocalFailure = throwable !is CapsuleError || throwable.code == null,
        )

    companion object {
        /**
         * How long one rung of the per-file ladder takes to render.
         *
         * Slow enough that a reader can see a file move through processing, encrypting and
         * uploading rather than watching a blur, and fast enough that a small import does not
         * outlast the user's patience.
         */
        val DEFAULT_TICK: Duration = Duration.ofMillis(400)
    }
}
