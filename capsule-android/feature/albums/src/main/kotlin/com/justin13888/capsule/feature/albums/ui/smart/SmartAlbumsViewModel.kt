package com.justin13888.capsule.feature.albums.ui.smart

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.SearchRepository
import com.justin13888.capsule.core.model.SmartAlbumDefinition
import com.justin13888.capsule.core.model.SmartAlbumId
import com.justin13888.capsule.core.model.SmartAlbumValidation
import com.justin13888.capsule.core.model.SmartAlbumValidator
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.persistentListOf
import kotlinx.collections.immutable.toImmutableList
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/** One saved search, as the list shows it. */
data class SmartAlbumRow(
    val id: SmartAlbumId,
    /** The display name, or empty when the register holds a stamped absence. */
    val name: String,
    /**
     * What this build may do with the definition.
     *
     * Carried rather than reduced to a boolean because the three outcomes are three different
     * rows: one is evaluated, one is preserved-but-not-evaluated and must say so, and one is
     * structurally invalid and must not be opened in the editor.
     */
    val validation: SmartAlbumValidation,
    /** How many assets it matches right now, or `null` when it cannot be evaluated. */
    val matchCount: Int?,
    /** Whether [matchCount] hit the counting cap rather than the real total. */
    val isMatchCountCapped: Boolean,
)

/** What the smart album list is showing. */
sealed interface SmartAlbumsUiState {
    /** Reading definitions and evaluating them. */
    data object Loading : SmartAlbumsUiState

    /** The read failed; [code] is the stable catalogue code. */
    data class Failed(
        val code: String?,
    ) : SmartAlbumsUiState

    /** The definitions the owner holds, including the ones this build cannot evaluate. */
    data class Content(
        val rows: ImmutableList<SmartAlbumRow> = persistentListOf(),
    ) : SmartAlbumsUiState
}

/**
 * The saved searches that present themselves as albums.
 *
 * Definitions from a later grammar are listed rather than filtered out, and that is the
 * design's instruction rather than a nicety: such a definition must be preserved verbatim and
 * synced onward, so hiding it would be the first step towards a client that eventually
 * strips it.
 */
class SmartAlbumsViewModel(
    private val search: SearchRepository,
) : ViewModel() {
    private val state = MutableStateFlow<SmartAlbumsUiState>(SmartAlbumsUiState.Loading)

    /** What the list is showing right now. */
    val uiState: StateFlow<SmartAlbumsUiState> = state.asStateFlow()

    init {
        observe()
    }

    /** Deletes a definition — a stamped absence, converging like any other edit. */
    fun delete(id: SmartAlbumId) {
        viewModelScope.launch {
            runCatching { search.deleteSmartAlbum(id) }.onFailure { failure ->
                if (failure is CapsuleError) state.value = SmartAlbumsUiState.Failed(failure.code?.value)
            }
        }
    }

    private fun observe() {
        viewModelScope.launch {
            search.smartAlbums().collect { definitions ->
                state.value = SmartAlbumsUiState.Content(definitions.map { toRow(it) }.toImmutableList())
            }
        }
    }

    private suspend fun toRow(definition: SmartAlbumDefinition): SmartAlbumRow {
        val validation = SmartAlbumValidator.validate(definition)
        // Only an evaluable definition is evaluated: guessing at terms this build does not
        // understand would compute a different membership than the device that wrote them.
        val matches =
            if (validation is SmartAlbumValidation.Evaluable) {
                runCatching { search.evaluate(definition.id, 0, MATCH_CAP).size }.getOrNull()
            } else {
                null
            }
        return SmartAlbumRow(
            id = definition.id,
            name = definition.displayName.value.orEmpty(),
            validation = validation,
            matchCount = matches,
            isMatchCountCapped = matches == MATCH_CAP,
        )
    }

    private companion object {
        /**
         * How many matches the list will count before it stops.
         *
         * The list shows a figure per row, so an uncapped count would evaluate every
         * definition against the whole library every time the list is opened.
         */
        const val MATCH_CAP = 512
    }
}
