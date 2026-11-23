package com.justin13888.capsule.feature.viewer.editor

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.AssetRepository
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.MetadataRepository
import com.justin13888.capsule.core.model.AddId
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.AssetTags
import com.justin13888.capsule.core.model.Caption
import com.justin13888.capsule.core.model.CullFlag
import com.justin13888.capsule.core.model.GpsSource
import com.justin13888.capsule.core.model.SupersededCaption
import com.justin13888.capsule.core.model.asWire
import com.justin13888.capsule.feature.viewer.ViewerFailure
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.launch

/**
 * The editor's state machine: three observed reads and ten stamped writes.
 *
 * Every mutator is one last-writer-wins register write or one observed-remove operation, so
 * each converges on its own no matter what order it reaches another device in — which is why
 * none of them needs a confirmation and all of them are reversible.
 *
 * Writes are not fire-and-forget. A refusal is kept on the state rather than logged, because
 * the interesting refusals here are ones the user can act on: a quota past its grace window,
 * or an asset a newer peer wrote that this build cannot re-sign.
 */
@Suppress("TooManyFunctions")
class MetadataEditorViewModel(
    private val assetId: AssetId,
    private val assets: AssetRepository,
    private val metadata: MetadataRepository,
) : ViewModel() {
    private val mutableState = MutableStateFlow<MetadataEditorUiState>(MetadataEditorUiState.Loading)
    private val refusal = MutableStateFlow<ViewerFailure?>(null)

    /** The one immutable value the screen renders. */
    val state: StateFlow<MetadataEditorUiState> = mutableState.asStateFlow()

    init {
        observe()
    }

    /** Re-reads everything — the remediation behind the failure state. */
    fun retry() {
        refusal.value = null
        mutableState.value = MetadataEditorUiState.Loading
        observe()
    }

    /** Acknowledges a refused write, so its banner leaves. */
    fun dismissRefusal() {
        refusal.value = null
    }

    /** Writes a caption, displacing the previous value into the superseded log. */
    fun setCaption(value: String) = write { metadata.setCaption(assetId, value) }

    /** Clears the caption — a stamped absence, converging like any other write. */
    fun clearCaption() = write { metadata.clearCaption(assetId) }

    /** Rewrites a displaced caption as the newest stamped value, so it wins everywhere. */
    fun restoreCaption(superseded: SupersededCaption) = write { metadata.restoreSupersededCaption(assetId, superseded) }

    /** Sets the star rating. Orthogonal to the cull flag by design. */
    fun setRating(rating: Int) = write { assets.setRating(assetId, rating) }

    /** Sets the trinary review flag. */
    fun setCull(flag: CullFlag) = write { assets.setCull(assetId, flag) }

    /** Hides or unhides. View-layer only: the asset stays in its album and keeps syncing. */
    fun setHidden(hidden: Boolean) = write { assets.setHidden(assetId, hidden) }

    /** Adds a user tag under a fresh add id. */
    fun addUserTag(tag: String) {
        val trimmed = tag.trim()
        if (trimmed.isEmpty()) return
        write { metadata.addUserTag(assetId, trimmed) }
    }

    /** Removes a user tag by the add it targets, never by its text. */
    fun removeUserTag(addId: AddId) = write { metadata.removeUserTag(assetId, addId) }

    /** Dismisses a model suggestion — an observed-remove on the AI set. */
    fun dismissAiTag(addId: AddId) = write { metadata.dismissAiTag(assetId, addId) }

    /**
     * Copies a suggestion into the user set and takes it off the suggestion list.
     *
     * Dismissing as part of promoting is the behaviour the affordance implies: a suggestion
     * the user has accepted is no longer a suggestion, and leaving it in both lists would ask
     * them to act on it twice.
     */
    fun promoteAiTag(addId: AddId) = write { metadata.promoteAiTag(assetId, addId, alsoDismiss = true) }

    /**
     * Promotes an inferred fix to a confirmed one.
     *
     * The coordinates are untouched — only the source changes. An inferred value is written to
     * the canonical field on explicit user confirmation and never before, the same promotion
     * rule that governs a suggested tag becoming a user tag.
     */
    fun confirmLocation() =
        write {
            val fix = assets.findByUuid(assetId)?.gps ?: return@write
            assets.setGps(assetId, fix.copy(source = GpsSource.Manual.asWire()))
        }

    /** Clears the stored fix. */
    fun clearLocation() = write { assets.setGps(assetId, null) }

    private fun observe() {
        viewModelScope.launch {
            combine(
                assets.observeAsset(assetId),
                metadata.caption(assetId),
                metadata.tags(assetId),
                refusal,
            ) { asset, caption, tags, lastRefusal -> toState(asset, caption, tags, lastRefusal) }
                .catch { cause ->
                    if (cause is CapsuleError) {
                        emit(MetadataEditorUiState.Failed(ViewerFailure.of(cause)))
                    } else {
                        throw cause
                    }
                }.collect { mutableState.value = it }
        }
    }

    private fun toState(
        asset: Asset?,
        caption: Caption,
        tags: AssetTags,
        lastRefusal: ViewerFailure?,
    ): MetadataEditorUiState =
        if (asset == null) {
            MetadataEditorUiState.Missing
        } else {
            MetadataEditorUiState.Ready(MetadataEditorState.of(asset, caption, tags, lastRefusal))
        }

    /**
     * Runs one write, keeping a coded refusal where the user can see it.
     *
     * Only a `CapsuleError` is caught. Anything else is a defect in an adapter, and turning it
     * into a banner would hide the one class of failure that ought to be loud.
     */
    private fun write(block: suspend () -> Unit) {
        viewModelScope.launch {
            try {
                refusal.value = null
                block()
            } catch (error: CapsuleError) {
                refusal.value = ViewerFailure.of(error)
            }
        }
    }
}
