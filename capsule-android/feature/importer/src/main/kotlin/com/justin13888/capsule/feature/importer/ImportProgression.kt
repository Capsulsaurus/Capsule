package com.justin13888.capsule.feature.importer

import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.ImportCandidate
import com.justin13888.capsule.core.model.ImportFileState
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.toImmutableList

/** One file of a run, and how far along the pipeline it has got. */
data class ImportFileRow(
    /** The source path, as the platform spells it. */
    val locator: String,
    /** Bytes on disk. */
    val sizeBytes: Long,
    /** Where this file currently is. */
    val state: ImportFileState,
) {
    /** The file's own name, which is what a row shows; the full path is the supporting text. */
    val displayName: String get() = locator.substringAfterLast('/')

    /** Whether the file has stopped moving, one way or the other. */
    val isTerminal: Boolean get() = state.isTerminal
}

/**
 * The per-file ladder of a run in flight, advanced one tick at a time.
 *
 * A pure value with an [advance] step rather than a coroutine that sleeps, for two reasons.
 * It makes the whole progression testable without waiting — a virtual clock in the view model
 * drives it, so a test asserts the exact sequence of states rather than racing a timer. And it
 * keeps the *sliding window* honest: streaming's whole point is that peak local disk is
 * bounded to the files in flight, which is a property of this data structure rather than of
 * whatever the executor happens to do.
 *
 * The mock repository's `confirm` returns an already-finished progress because it moves no
 * bytes, so this is what actually renders queued → processing → encrypting → uploading →
 * done. It is presentation of a real pipeline's stages, never a claim that bytes moved.
 */
data class ImportProgression(
    /** Every admitted file, in the order the plan listed it. */
    val rows: ImmutableList<ImportFileRow>,
    /**
     * How many files may be materialized locally at once.
     *
     * The bound streaming mode exists for: past this many, a file is not admitted into the
     * library at all, so peak local disk is the window rather than the whole import.
     */
    val windowSize: Int,
    /** Whether the run has stopped admitting and advancing work — a lost server, normally. */
    val isPaused: Boolean = false,
    /** The code that halted the run, or `null` while it is healthy. */
    val haltCode: ErrorCode? = null,
) {
    /** Files that have reached a terminal state. */
    val completed: Int get() = rows.count(ImportFileRow::isTerminal)

    /** Files the plan admitted. */
    val total: Int get() = rows.size

    /** Files that stopped with a failure. */
    val failed: Int get() = rows.count { it.state == ImportFileState.Failed }

    /** Files that finished cleanly. */
    val imported: Int get() = rows.count { it.state == ImportFileState.Done }

    /** Whether every admitted file has stopped moving. */
    val isFinished: Boolean get() = rows.all(ImportFileRow::isTerminal)

    /** How far through the run this is, `0f..1f`; a run with nothing in it is finished. */
    val fraction: Float get() = if (total == 0) 1f else completed.toFloat() / total.toFloat()

    /** The files currently materialized on this device — what the window is bounding. */
    val inFlight: ImmutableList<ImportFileRow>
        get() = rows.filterNot(ImportFileRow::isTerminal).take(windowSize).toImmutableList()

    /**
     * One step of the pipeline for every file in the window.
     *
     * Paused and halted runs return themselves unchanged. That is the pause-on-disconnect
     * rule in one line: a streaming import stops rather than importing ahead into space it
     * does not have, and in-flight uploads simply wait for the link to come back instead of
     * being torn down.
     */
    fun advance(): ImportProgression {
        if (isPaused || haltCode != null || isFinished) return this
        val window =
            rows
                .filterNot(ImportFileRow::isTerminal)
                .take(windowSize)
                .map(ImportFileRow::locator)
                .toSet()
        return copy(rows = rows.map { row -> if (row.locator in window) row.stepped() else row }.toImmutableList())
    }

    /**
     * Halts the run on [code], failing whatever was in flight.
     *
     * Only the window fails. Files still queued behind it never started, so reporting them as
     * failures would tell the user work was lost that was never begun — and the plan is
     * deterministic, so a resumed run re-derives them untouched.
     */
    fun halt(code: ErrorCode): ImportProgression {
        val window = inFlight.map(ImportFileRow::locator).toSet()
        val next = rows.map { row -> if (row.locator in window) row.copy(state = ImportFileState.Failed) else row }
        return copy(rows = next.toImmutableList(), haltCode = code)
    }

    /**
     * Stops the run where it stands, without failing anything.
     *
     * A cancelled run is a shorter run rather than a rollback: finished files stay imported,
     * and files that never started are simply not part of it any more.
     */
    fun cancelled(): ImportProgression = copy(rows = rows.filter(ImportFileRow::isTerminal).toImmutableList())

    /** This progression with [paused] in force. */
    fun paused(paused: Boolean): ImportProgression = copy(isPaused = paused)

    private fun ImportFileRow.stepped(): ImportFileRow =
        copy(
            state =
                when (state) {
                    ImportFileState.Queued -> ImportFileState.Processing
                    ImportFileState.Processing -> ImportFileState.Encrypting
                    ImportFileState.Encrypting -> ImportFileState.Uploading
                    ImportFileState.Uploading -> ImportFileState.Done
                    ImportFileState.Done, ImportFileState.Failed -> state
                },
        )

    companion object {
        /**
         * How many files a normal import keeps in flight.
         *
         * Scan, encrypt and derivative generation parallelize across files, so a window of one
         * would leave the device idle between stages; four is enough to keep them busy without
         * making the progress list unreadable.
         */
        const val DEFAULT_WINDOW = 4

        /**
         * How many a streaming import keeps in flight.
         *
         * Deliberately tighter. Streaming exists because the whole import does not fit, so the
         * window is the peak disk it will occupy, and it must still hold the largest single
         * file whole.
         */
        const val STREAMING_WINDOW = 2

        /** A run over [candidates], nothing started yet. */
        fun of(
            candidates: List<ImportCandidate>,
            windowSize: Int = DEFAULT_WINDOW,
        ): ImportProgression =
            ImportProgression(
                rows =
                    candidates
                        .map { ImportFileRow(it.sourceLocator, it.sizeBytes, ImportFileState.Queued) }
                        .toImmutableList(),
                windowSize = windowSize,
            )
    }
}
