package com.justin13888.capsule.feature.importer

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.AlbumRepository
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.ImportRepository
import com.justin13888.capsule.core.data.SyncRepository
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.ImportCandidate
import com.justin13888.capsule.core.model.ImportDecision
import com.justin13888.capsule.core.model.ImportId
import com.justin13888.capsule.core.model.ImportPlan
import com.justin13888.capsule.core.model.UploadPolicy
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/**
 * The plan-and-confirm screen's state machine.
 *
 * Planning and confirming are two calls because planning is pure and confirming is not: the
 * free-space probe is I/O, and I/O inside the planner would let a user confirm one set of
 * numbers and watch another execute. This view model keeps that split visible — it re-derives
 * the plan from the recorded sources (deterministic, so the same plan every time) and only
 * then attaches the probe's verdict.
 */
class ImportPlanViewModel(
    private val imports: ImportRepository,
    private val albums: AlbumRepository,
    private val sync: SyncRepository,
    private val freeSpace: FreeSpaceProbe,
    private val plans: ImportPlanStore,
    private val importId: ImportId,
) : ViewModel() {
    private val mutableState = MutableStateFlow(ImportPlanUiState())

    /** Everything the confirm screen draws. */
    val state: StateFlow<ImportPlanUiState> = mutableState.asStateFlow()

    init {
        reload()
    }

    /** Re-derives the plan and re-probes free space. Also the error state's remediation. */
    fun reload() {
        mutableState.update { ImportPlanUiState(isLoading = true, mode = it.mode) }
        viewModelScope.launch {
            val recorded = plans.get(importId)
            if (recorded == null) {
                mutableState.update { it.copy(isLoading = false, hasLocalFailure = true) }
                return@launch
            }
            runCatching { derive(recorded) }
                .onSuccess { derived -> mutableState.update { derived.copy(mode = it.mode) } }
                .onFailure { throwable -> mutableState.update { it.copy(isLoading = false).failed(throwable) } }
        }
    }

    /** Chooses whether the source files survive the run. */
    fun setMode(mode: ImportMode) {
        mutableState.update { it.copy(mode = mode) }
    }

    /**
     * Resolves the streaming-versus-staged refusal by taking the device off staged uploads.
     *
     * Offered as an explicit action rather than applied silently: staged uploads are a
     * deliberate low-data choice, and quietly undoing one to get an import moving would spend
     * the user's mobile data for them.
     */
    fun switchToFullUploads() {
        viewModelScope.launch {
            runCatching { sync.setUploadPolicy(UploadPolicy.Full) }
                .onSuccess { mutableState.update { it.copy(uploadPolicy = UploadPolicy.Full) } }
                .onFailure { throwable -> mutableState.update { it.failed(throwable) } }
        }
    }

    /**
     * Confirms the plan and starts the run.
     *
     * The probe's verdict is attached here, at confirmation, which is the one place it may be:
     * the plan the user read is the plan that executes, plus the one field the pure planner is
     * forbidden to set.
     */
    fun confirm() {
        val current = mutableState.value
        val plan = current.plan ?: return
        if (!current.canConfirm) return
        mutableState.update { it.copy(isConfirming = true, failure = null, hasLocalFailure = false) }
        viewModelScope.launch {
            runCatching { imports.confirm(plan.copy(streamingRecommended = current.isStreaming)) }
                .onSuccess { started ->
                    mutableState.update { it.copy(isConfirming = false, startedImportId = started) }
                }.onFailure { throwable ->
                    mutableState.update { it.copy(isConfirming = false).failed(throwable) }
                }
        }
    }

    /** Forgets the started run, so returning to this screen does not navigate again. */
    fun clearStarted() {
        mutableState.update { it.copy(startedImportId = null) }
    }

    private suspend fun derive(recorded: PlannedImport): ImportPlanUiState {
        val plan = imports.plan(recorded.sourceLocators, recorded.destination)
        plans.put(recorded.copy(plan = plan))
        val available = freeSpace.availableBytes()
        val verdict = verdictFor(plan, available)
        val policy = sync.uploadPolicy().first()
        val destinationId = plan.added.firstOrNull()?.let { candidateDestination(it) } ?: recorded.destination
        val name =
            destinationId?.let { id ->
                albums
                    .albums()
                    .first()
                    .firstOrNull { it.id == id }
                    ?.name
            }
        return ImportPlanUiState(
            isLoading = false,
            plan = plan.copy(streamingRecommended = verdict is ImportSpaceVerdict.StreamingRecommended),
            destinationName = name,
            space = verdict,
            uploadPolicy = policy,
        )
    }

    /**
     * What the probe says about this plan.
     *
     * The single-file check runs first and wins. Streaming bounds peak usage to the window but
     * still has to materialize the largest asset whole, so a file bigger than the disk is a
     * refusal no window size answers — and saying so now beats stalling mid-stream.
     */
    private fun verdictFor(
        plan: ImportPlan,
        availableBytes: Long,
    ): ImportSpaceVerdict {
        val largest = plan.added.maxByOrNull(ImportCandidate::sizeBytes)
        if (largest != null && largest.sizeBytes > availableBytes) {
            return ImportSpaceVerdict.SingleFileTooLarge(largest.sourceLocator, largest.sizeBytes, availableBytes)
        }
        val needed = plan.totalSize + plan.totalSize / HEADROOM_DIVISOR
        return if (needed > availableBytes) {
            ImportSpaceVerdict.StreamingRecommended(availableBytes, plan.totalSize)
        } else {
            ImportSpaceVerdict.Fits
        }
    }

    private fun candidateDestination(candidate: ImportCandidate): AlbumId? {
        val decision = candidate.decision as? ImportDecision.Import
        return decision?.destination
    }

    private fun ImportPlanUiState.failed(throwable: Throwable): ImportPlanUiState =
        copy(
            failure = (throwable as? CapsuleError)?.code,
            hasLocalFailure = throwable !is CapsuleError || throwable.code == null,
        )

    private companion object {
        /**
         * The headroom margin, as a divisor of the import's own size.
         *
         * Ten percent, because a volume filled to the last byte by an import is a volume the
         * OS cannot write its own journals to; the probe is meant to keep the device usable,
         * not merely to make the arithmetic fit.
         */
        const val HEADROOM_DIVISOR = 10L
    }
}
