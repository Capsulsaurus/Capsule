package com.justin13888.capsule.feature.albums.ui.smart

import com.justin13888.capsule.core.model.PredicateViolation
import com.justin13888.capsule.core.model.SortDirection
import com.justin13888.capsule.core.model.SortKey
import com.justin13888.capsule.feature.albums.builder.DraftNodeId
import com.justin13888.capsule.feature.albums.builder.EditRefusal
import com.justin13888.capsule.feature.albums.builder.PredicateDraft
import com.justin13888.capsule.feature.albums.data.SampledMatchCount
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.persistentListOf

/**
 * The rule under construction, and everything that decides whether it may be saved.
 *
 * [violations] and [incomplete] are separate lists because they are separate problems.
 * A violation is a structural rejection the definition validator would return — the rule is
 * malformed and would be refused wherever it landed. An incomplete node is a half-typed
 * operand, which is the editor's ordinary working state and not an error at all. Merging
 * them would make "you are still typing" look like "your rule is wrong".
 */
data class SmartAlbumBuilderUiState(
    /** Reading an existing definition. */
    val isLoading: Boolean = false,
    /**
     * True when the definition was authored under a later grammar than this build knows.
     *
     * The editor refuses to open it. It has no controls for terms it does not understand, so
     * anything it could show would be the other device's rule with pieces missing — and a
     * save from that screen would overwrite their work with the truncation.
     */
    val isForwardSchema: Boolean = false,
    /** The display name, bounded at 256 bytes by the grammar. */
    val name: String = "",
    /** The rule tree. */
    val draft: PredicateDraft = PredicateDraft.empty(),
    /** What the results are ordered by. */
    val sortKey: SortKey = SortKey.CaptureTimestamp,
    /** Which way. */
    val sortDirection: SortDirection = SortDirection.Descending,
    /** The live match count, or `null` before the first one has been computed. */
    val matchCount: SampledMatchCount? = null,
    /** Whether a recount is in flight. */
    val isCounting: Boolean = false,
    /** Every structural rejection the validator found, not just the first. */
    val violations: ImmutableList<PredicateViolation> = persistentListOf(),
    /** Conditions whose operand does not parse yet. */
    val incomplete: ImmutableList<DraftNodeId> = persistentListOf(),
    /** The last edit the editor would not apply, until the screen has shown it. */
    val refusal: EditRefusal? = null,
    /** Set once the definition has been written, so the caller can leave. */
    val isSaved: Boolean = false,
) {
    /**
     * Whether saving is allowed.
     *
     * Deliberately conjunctive rather than "no violations": a rule with a half-typed date has
     * no violations *because* it has no tree yet, and saving it would silently drop the
     * condition the user was in the middle of writing.
     */
    val canSave: Boolean
        get() = !isForwardSchema && name.isNotBlank() && violations.isEmpty() && incomplete.isEmpty()
}
