package com.justin13888.capsule.feature.importer

import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.ImportId
import com.justin13888.capsule.core.model.ImportPlan
import com.justin13888.capsule.core.model.UploadPolicy

/** Whether the source files survive the import. */
enum class ImportMode {
    /** The originals stay where they are. The safe default. */
    Copy,

    /**
     * Each original is deleted once its photo is durably stored.
     *
     * Never before: the release is gated on the server's durability verdict, so a file that
     * did not finish importing keeps its source.
     */
    Move,
}

/**
 * What the free-space probe concluded about this plan.
 *
 * Three outcomes rather than a boolean, because the third is not a worse version of the
 * second. Streaming bounds *peak* usage to the sliding window; it cannot make one file
 * smaller than the disk, so a single oversized asset is a hard refusal at confirmation
 * rather than something to discover halfway through.
 */
sealed interface ImportSpaceVerdict {
    /** The whole import fits locally with room to spare. */
    data object Fits : ImportSpaceVerdict

    /** It does not fit at once, but a sliding window would. */
    data class StreamingRecommended(
        /** Bytes available on the library volume. */
        val freeBytes: Long,
        /** Bytes the run would move. */
        val neededBytes: Long,
    ) : ImportSpaceVerdict

    /** One file alone is larger than the free space, so no window size helps. */
    data class SingleFileTooLarge(
        /** The offending file's source locator. */
        val locator: String,
        /** How large it is. */
        val sizeBytes: Long,
        /** How much room there is. */
        val freeBytes: Long,
    ) : ImportSpaceVerdict {
        /** How much must be freed before this file can be imported at all. */
        val shortfallBytes: Long get() = sizeBytes - freeBytes
    }
}

/**
 * Everything the plan-and-confirm screen draws.
 *
 * The plan itself is carried whole rather than pre-flattened into counts: the numbers the
 * user confirms have to be the numbers that execute, and re-deriving them from one object is
 * how that stays true when the screen grows another figure.
 */
data class ImportPlanUiState(
    /** The planner is still running. */
    val isLoading: Boolean = true,
    /** What it decided, once it has. */
    val plan: ImportPlan? = null,
    /** The destination album's name, resolved for display. */
    val destinationName: String? = null,
    /** Whether the sources survive the run. */
    val mode: ImportMode = ImportMode.Copy,
    /** What the free-space probe concluded. */
    val space: ImportSpaceVerdict = ImportSpaceVerdict.Fits,
    /** The device's upload policy, which streaming is mutually exclusive with. */
    val uploadPolicy: UploadPolicy = UploadPolicy.Full,
    /** A confirmation is in flight. */
    val isConfirming: Boolean = false,
    /** The run that started, once confirmation succeeded. */
    val startedImportId: ImportId? = null,
    /** The coded failure to localize, or `null`. */
    val failure: ErrorCode? = null,
    /** Set when a failure carries no catalogue code — a purely local refusal. */
    val hasLocalFailure: Boolean = false,
) {
    /** Whether this run has to stream to fit. */
    val isStreaming: Boolean get() = space is ImportSpaceVerdict.StreamingRecommended

    /**
     * Whether the run is refused because streaming met a staged upload policy.
     *
     * Streaming releases each original as soon as the server confirms it; staged defers
     * exactly that upload. The combination has no coherent behaviour, so it is refused at
     * confirmation rather than half-honoured.
     */
    val hasStagedConflict: Boolean get() = isStreaming && uploadPolicy == UploadPolicy.Staged

    /** Whether one file alone cannot be imported at any window size. */
    val isBlocked: Boolean get() = space is ImportSpaceVerdict.SingleFileTooLarge

    /** Whether the planner admitted nothing at all. */
    val isEmpty: Boolean get() = plan != null && plan.added.isEmpty() && plan.conflicts.isEmpty()

    /** Whether anything is still holding the confirm action back. */
    val canConfirm: Boolean
        get() =
            plan != null &&
                plan.isReady &&
                !isConfirming &&
                !isBlocked &&
                !hasStagedConflict &&
                failure == null &&
                !hasLocalFailure

    /** Whether an error state should replace the content. */
    val hasFailed: Boolean get() = plan == null && (failure != null || hasLocalFailure)
}
