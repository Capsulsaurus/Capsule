package com.justin13888.capsule.feature.importer

import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.ImportId
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.ImmutableSet
import kotlinx.collections.immutable.persistentListOf
import kotlinx.collections.immutable.persistentSetOf

/** One album the destination question can be answered with. */
data class ImportAlbumChoice(
    /** The container's id. */
    val id: AlbumId,
    /** Its name, or empty for the nameless default album. */
    val name: String,
    /** Whether this is the owner's default album. */
    val isDefault: Boolean,
)

/**
 * The one-time question a source asks the first time anything is imported from it.
 *
 * Modelled as state rather than raised as an event so it survives a rotation and cannot be
 * shown twice: an automated import must never invent a destination, and a prompt that is
 * lost mid-answer would leave the user with exactly that.
 */
data class ScopeMappingPrompt(
    /** The source being asked about. */
    val source: ImportSource,
    /** The album currently highlighted, or `null` until the user picks one. */
    val chosen: AlbumId? = null,
)

/** How far a scan of one source has got. */
data class ScanProgress(
    /** The source being read. */
    val locator: String,
    /** Candidate files counted so far. */
    val filesFound: Int,
)

/**
 * Everything the source picker draws.
 *
 * Selection is a set of **locators** rather than of indices or bucket ids, which is the same
 * identity the scope carries: on Android that is the bucket's relative path, so a selection
 * survives the media store renumbering its buckets.
 */
data class ImportSourcesUiState(
    /** The source list is still being read. */
    val isLoading: Boolean = true,
    /** Every source this device can import from. */
    val sources: ImmutableList<ImportSource> = persistentListOf(),
    /** Which sources are ticked, by locator. */
    val selected: ImmutableSet<String> = persistentSetOf(),
    /** Containers the destination question can be answered with. */
    val albums: ImmutableList<ImportAlbumChoice> = persistentListOf(),
    /** A scan in flight, or `null`. */
    val scan: ScanProgress? = null,
    /** The destination question awaiting an answer, or `null`. */
    val prompt: ScopeMappingPrompt? = null,
    /** The plan that is ready to be confirmed, once scanning has produced one. */
    val plannedImportId: ImportId? = null,
    /** The coded failure to localize, or `null`. */
    val failure: ErrorCode? = null,
    /** Set when a failure carries no catalogue code — a purely local refusal. */
    val hasLocalFailure: Boolean = false,
) {
    /** Whether the picker has nothing at all to offer. */
    val isEmpty: Boolean get() = !isLoading && sources.isEmpty()

    /** Whether an error state should replace the content. */
    val hasFailed: Boolean get() = sources.isEmpty() && (failure != null || hasLocalFailure)

    /** Whether scanning may start. */
    val canScan: Boolean get() = selected.isNotEmpty() && scan == null && prompt == null

    /** The ticked sources, in list order. */
    val selectedSources: List<ImportSource> get() = sources.filter { it.locator in selected }

    /** The first ticked source that has never been asked where its photos should go. */
    val unmappedSelection: ImportSource? get() = selectedSources.firstOrNull { !it.isMapped }
}
