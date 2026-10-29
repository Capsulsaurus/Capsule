package com.justin13888.capsule.screens.labels

import com.justin13888.capsule.screens.domain.AlbumId
import com.justin13888.capsule.screens.domain.AssetId
import com.justin13888.capsule.screens.domain.AssetRegion
import com.justin13888.capsule.screens.domain.LabelPort
import com.justin13888.capsule.screens.domain.LabelRefusal
import com.justin13888.capsule.screens.domain.LabelTarget
import com.justin13888.capsule.screens.domain.LabelTargetNotFound
import com.justin13888.capsule.screens.domain.RegionId
import com.justin13888.capsule.screens.domain.ShapeRefusal
import com.justin13888.capsule.screens.domain.ShapeRefusalReason
import com.justin13888.capsule.screens.domain.StackId
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import java.util.logging.Level
import java.util.logging.Logger

/** What the label editor is open on. */
sealed interface LabelSubject {
    /** An asset: its regions and their labels. */
    data class Asset(
        val asset: AssetId,
    ) : LabelSubject

    /** A stack's labels. */
    data class Stack(
        val stack: StackId,
    ) : LabelSubject

    /** An album's labels. */
    data class Album(
        val album: AlbumId,
    ) : LabelSubject
}

/** Why the last request was refused. */
sealed interface LabelFailure {
    /** The subject or region no longer exists. */
    data object NotFound : LabelFailure

    /** The label is empty once trimmed. */
    data object LabelEmpty : LabelFailure

    /** The label is past the byte bound. */
    data class LabelTooLong(
        val bytes: Int,
    ) : LabelFailure

    /** The drawn shape fell outside the image contract. */
    data class ShapeRefused(
        val reason: ShapeRefusalReason,
    ) : LabelFailure

    /** The port failed for a reason the editor cannot name. */
    data object Unavailable : LabelFailure
}

/** Everything the label editor draws. */
data class LabelEditorState(
    /** The labels of the selected region, or of the stack or album. */
    val labels: List<String> = emptyList(),
    /** The asset's live regions, oldest first; empty for a stack or album. */
    val regions: List<AssetRegion> = emptyList(),
    /** The region whose labels are shown and edited. */
    val selectedRegionId: RegionId? = null,
    /** The region being drawn, if one is. */
    val draft: RegionDraft? = null,
    /** A request is in flight; the editor takes no other until it answers. */
    val isWriting: Boolean = false,
    /** Why the last request was refused, cleared by the next one. */
    val failure: LabelFailure? = null,
)

/**
 * Drives the label and region editor: labels on a stack or an album, and on an asset,
 * regions drawn over its preview and labelled. The same model as the Apple client's
 * `LabelEditorModel`.
 *
 * Every write goes through [LabelPort] and the editor redraws from what the port
 * answers, never from what it asked for, so a label the core normalizes shows as
 * stored. A refused write leaves the editor's state as it was and names the refusal in
 * [LabelEditorState.failure]; nothing is retried behind the user's back.
 *
 * Main-confined: call every member from one thread (the UI's), as a ViewModel does.
 * An asset's own labels are its user tags, edited where the other organization edits
 * are, so on an asset this editor labels regions only.
 */
class LabelEditorModel(
    val subject: LabelSubject,
    private val port: LabelPort,
) {
    private val mutableState = MutableStateFlow(LabelEditorState())

    /** What the editor draws. */
    val state: StateFlow<LabelEditorState> = mutableState.asStateFlow()

    /**
     * Where a typed label goes: the selected region on an asset, or the stack or album
     * itself. `null` on an asset with no region selected.
     */
    val labelTarget: LabelTarget? get() = targetFor(mutableState.value.selectedRegionId)

    /** Read the subject's regions or labels from the port. */
    suspend fun load() =
        perform("load") {
            when (subject) {
                is LabelSubject.Asset -> {
                    val regions = port.regions(subject.asset)
                    mutableState.update { state ->
                        val kept = state.selectedRegionId?.let { id -> regions.firstOrNull { it.id == id } }
                        state.copy(regions = regions).selecting(kept ?: regions.firstOrNull())
                    }
                }
                is LabelSubject.Stack, is LabelSubject.Album -> {
                    val target = labelTarget ?: return@perform
                    val labels = port.labels(target)
                    mutableState.update { it.copy(labels = labels) }
                }
            }
        }

    /**
     * Show and edit the labels of [region]. Refused while a request is in flight, so its
     * answer lands on the region it was asked about.
     */
    fun selectRegion(region: RegionId?) {
        mutableState.update { state ->
            if (state.isWriting) state else state.selecting(region?.let { id -> state.regions.firstOrNull { it.id == id } })
        }
    }

    /** Add a label to the current target. */
    suspend fun addLabel(text: String) {
        val target = labelTarget ?: return
        perform("add label") { store(port.addLabel(text, target), target) }
    }

    /** Remove a label from the current target. */
    suspend fun removeLabel(text: String) {
        val target = labelTarget ?: return
        perform("remove label") { store(port.removeLabel(text, target), target) }
    }

    /** Start drawing a new region of [kind], replacing any draft. Refused while a request is in flight. */
    fun startDrawing(kind: RegionDraft.Kind) {
        if (subject !is LabelSubject.Asset) return
        mutableState.update { if (it.isWriting) it else it.copy(draft = RegionDraft.of(kind), failure = null) }
    }

    /** Replace the draft with [draft] after a gesture or an adjust action; nothing while none is drawn. */
    fun updateDraft(draft: RegionDraft) {
        mutableState.update { if (it.draft == null) it else it.copy(draft = draft) }
    }

    /** Drop the draft without writing anything. */
    fun cancelDrawing() {
        mutableState.update { it.copy(draft = null) }
    }

    /** Write the draft as a region with its first labels, and select it. */
    suspend fun finishDrawing(labels: List<String>) {
        if (subject !is LabelSubject.Asset) return
        val pending = mutableState.value.draft ?: return
        perform("add region") {
            val region = port.addRegion(pending.shape, labels, subject.asset)
            mutableState.update { state ->
                state
                    .copy(regions = state.regions + region, draft = if (state.draft == pending) null else state.draft)
                    .selecting(region)
            }
        }
    }

    /** Delete the selected region. Final: its id is never live again. */
    suspend fun deleteSelectedRegion() {
        if (subject !is LabelSubject.Asset) return
        val region = mutableState.value.selectedRegionId ?: return
        perform("delete region") {
            port.deleteRegion(region, subject.asset)
            mutableState.update { state ->
                val left = state.regions.filterNot { it.id == region }
                state.copy(regions = left).selecting(left.firstOrNull())
            }
        }
    }

    private fun targetFor(region: RegionId?): LabelTarget? =
        when (subject) {
            is LabelSubject.Asset -> region?.let { LabelTarget.Region(subject.asset, it) }
            is LabelSubject.Stack -> LabelTarget.Stack(subject.stack)
            is LabelSubject.Album -> LabelTarget.Album(subject.album)
        }

    private fun LabelEditorState.selecting(region: AssetRegion?): LabelEditorState =
        if (subject is LabelSubject.Asset) {
            copy(selectedRegionId = region?.id, labels = region?.labels.orEmpty())
        } else {
            copy(selectedRegionId = region?.id)
        }

    /** Keep the port's answer for [target], and show it only while [target] is still the one edited. */
    private fun store(
        answer: List<String>,
        target: LabelTarget,
    ) {
        mutableState.update { state ->
            val shown = if (targetFor(state.selectedRegionId) == target) state.copy(labels = answer) else state
            if (target is LabelTarget.Region) {
                shown.copy(regions = shown.regions.map { if (it.id == target.region) it.copy(labels = answer) else it })
            } else {
                shown
            }
        }
    }

    private suspend fun perform(
        operation: String,
        work: suspend () -> Unit,
    ) {
        if (mutableState.value.isWriting) return
        mutableState.update { it.copy(isWriting = true, failure = null) }
        try {
            work()
            log.fine { "label editor $operation on $subject: done" }
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (
            @Suppress("TooGenericExceptionCaught") error: Exception,
        ) {
            val failure = failureFor(error)
            log.log(Level.INFO, "label editor $operation on $subject refused: $failure", error)
            mutableState.update { it.copy(failure = failure) }
        } finally {
            mutableState.update { it.copy(isWriting = false) }
        }
    }

    companion object {
        private val log: Logger = Logger.getLogger("capsule.labels")

        /** The editor's name for a port failure. */
        fun failureFor(error: Throwable): LabelFailure =
            when (error) {
                is LabelTargetNotFound -> LabelFailure.NotFound
                is LabelRefusal.Empty -> LabelFailure.LabelEmpty
                is LabelRefusal.TooLong -> LabelFailure.LabelTooLong(error.bytes)
                is ShapeRefusal -> LabelFailure.ShapeRefused(error.reason)
                else -> LabelFailure.Unavailable
            }
    }
}
