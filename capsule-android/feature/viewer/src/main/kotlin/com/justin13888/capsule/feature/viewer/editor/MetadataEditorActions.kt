package com.justin13888.capsule.feature.viewer.editor

import androidx.compose.runtime.Stable
import com.justin13888.capsule.core.model.AddId
import com.justin13888.capsule.core.model.CullFlag
import com.justin13888.capsule.core.model.SupersededCaption

/**
 * Every write the editor can make, in one value.
 *
 * Fourteen callbacks rather than a single `onEdit(Change)` because the operations are
 * genuinely different: a caption is a stamped register write, removing a user tag names the
 * add it undoes, and promoting a suggestion mints a fresh user-scoped add id. Folding them
 * into one channel would put a `when` in front of every one of those distinctions and lose
 * the compiler's help in keeping them apart.
 */
@Stable
@Suppress("LongParameterList")
class MetadataEditorActions(
    /** Writes a caption as the newest stamped value. */
    val onSetCaption: (String) -> Unit,
    /** Writes a stamped absence. */
    val onClearCaption: () -> Unit,
    /** Rewrites a displaced caption so it wins again everywhere. */
    val onRestoreCaption: (SupersededCaption) -> Unit,
    /** Sets the star rating, `0..5`. */
    val onSetRating: (Int) -> Unit,
    /** Sets the trinary review flag. */
    val onSetCull: (CullFlag) -> Unit,
    /** Hides or unhides the asset. */
    val onSetHidden: (Boolean) -> Unit,
    /** Adds a user tag. */
    val onAddUserTag: (String) -> Unit,
    /** Removes a user tag by the add it targets. */
    val onRemoveUserTag: (AddId) -> Unit,
    /** Copies a suggestion into the user set. */
    val onPromoteAiTag: (AddId) -> Unit,
    /** Removes a suggestion. */
    val onDismissAiTag: (AddId) -> Unit,
    /** Promotes an inferred fix to a confirmed one. */
    val onConfirmLocation: () -> Unit,
    /** Clears the stored fix. */
    val onClearLocation: () -> Unit,
    /** Acknowledges a refused write. */
    val onDismissRefusal: () -> Unit,
    /** Re-reads everything after a failed read. */
    val onRetry: () -> Unit,
)
