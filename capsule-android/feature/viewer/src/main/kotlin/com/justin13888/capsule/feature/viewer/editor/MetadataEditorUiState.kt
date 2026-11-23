package com.justin13888.capsule.feature.viewer.editor

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetTags
import com.justin13888.capsule.core.model.Caption
import com.justin13888.capsule.feature.viewer.ViewerDegradation
import com.justin13888.capsule.feature.viewer.ViewerFailure
import com.justin13888.capsule.feature.viewer.intrinsicDegradations
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.toImmutableList

/**
 * Everything the editor edits, and whether it is allowed to.
 *
 * The caption and the two tag sets come from a different contract than the flags because
 * they merge differently — a last-writer-wins register with a displaced-value log against
 * two observed-remove sets — and a single `setMetadata` would erase the distinction that
 * makes promoting a suggestion different from typing the same word.
 */
@Immutable
data class MetadataEditorState(
    /** The asset whose flags are being edited. */
    val asset: Asset,
    /** The winning caption and the values concurrent edits displaced. */
    val caption: Caption,
    /** Both tag namespaces, kept structurally apart. */
    val tags: AssetTags,
    /** The conditions the record establishes; one of them turns editing off. */
    val degradations: ImmutableList<ViewerDegradation>,
    /**
     * The last write that was refused, or `null`.
     *
     * Held rather than thrown away because a refusal here is nearly always actionable — a
     * quota past its grace window, an asset from a newer peer — and a control that silently
     * did nothing would be indistinguishable from a broken one.
     */
    val refusal: ViewerFailure? = null,
) {
    /** Whether this client may re-sign the asset's sidecar at all. */
    val isEditable: Boolean get() = degradations.none(ViewerDegradation::blocksEditing)

    companion object {
        /** Builds the editor's value from the three reads that feed it. */
        fun of(
            asset: Asset,
            caption: Caption,
            tags: AssetTags,
            refusal: ViewerFailure? = null,
        ): MetadataEditorState =
            MetadataEditorState(
                asset = asset,
                caption = caption,
                tags = tags,
                degradations = intrinsicDegradations(asset).toImmutableList(),
                refusal = refusal,
            )
    }
}

/** What the metadata editor is showing. */
@Immutable
sealed interface MetadataEditorUiState {
    /** The reads have not landed yet. */
    data object Loading : MetadataEditorUiState

    /** The asset is not in this library. */
    data object Missing : MetadataEditorUiState

    /** A read failed with a code the user can act on. */
    data class Failed(
        val failure: ViewerFailure,
    ) : MetadataEditorUiState

    /** The editor. */
    data class Ready(
        val editor: MetadataEditorState,
    ) : MetadataEditorUiState
}
