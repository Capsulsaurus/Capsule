package com.justin13888.capsule.feature.importer

import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.ImportId
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.persistentListOf

/**
 * Everything the execution screen draws.
 *
 * The per-file ladder lives in [progression] rather than being flattened into this record so
 * the window bound stays visible: a streaming run's honesty rests on "only these files are on
 * the device right now", and that is a property of the progression, not a label beside it.
 */
data class ImportRunUiState(
    /** The run being watched. */
    val importId: ImportId? = null,
    /** The run is still being attached to. */
    val isLoading: Boolean = true,
    /** Where every admitted file has got to. */
    val progression: ImportProgression = ImportProgression(persistentListOf(), ImportProgression.DEFAULT_WINDOW),
    /** Whether this run bounds its peak local disk to the window. */
    val isStreaming: Boolean = false,
    /**
     * How many candidates the plan passed over.
     *
     * Carried from the plan rather than derived from the run: skipped files were never
     * admitted, so nothing in the progression knows about them, and a summary that omitted
     * them would leave the user counting photos that are neither imported nor accounted for.
     */
    val skippedCount: Int = 0,
    /** Whether the user stopped the run. */
    val isCancelled: Boolean = false,
    /** The coded failure to localize, or `null`. */
    val failure: ErrorCode? = null,
    /** Set when a failure carries no catalogue code — a purely local refusal. */
    val hasLocalFailure: Boolean = false,
) {
    /** Files currently materialized on this device. */
    val inFlight: ImmutableList<ImportFileRow> get() = progression.inFlight

    /** Whether the run has stopped because the server went away. */
    val isPaused: Boolean get() = progression.isPaused

    /** Whether every admitted file has reached a terminal state. */
    val isFinished: Boolean get() = !isLoading && progression.isFinished

    /** Whether the summary should be shown in place of the running list. */
    val showsSummary: Boolean get() = isFinished || isCancelled

    /** Whether an error state should replace the content. */
    val hasFailed: Boolean get() = progression.rows.isEmpty() && (failure != null || hasLocalFailure)
}
