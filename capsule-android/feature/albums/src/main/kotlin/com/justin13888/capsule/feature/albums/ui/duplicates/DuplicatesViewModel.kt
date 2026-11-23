package com.justin13888.capsule.feature.albums.ui.duplicates

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.TimelineRepository
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.DeletionState
import com.justin13888.capsule.feature.albums.data.DuplicateFinder
import com.justin13888.capsule.feature.albums.data.LibrarySample
import com.justin13888.capsule.feature.albums.model.DuplicateGroup
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.persistentListOf
import kotlinx.collections.immutable.toImmutableList
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import java.time.Clock

/** What the duplicates view is showing. */
sealed interface DuplicatesUiState {
    /** Comparing the sample. */
    data object Loading : DuplicatesUiState

    /** The read failed; [code] is the stable catalogue code. */
    data class Failed(
        val code: String?,
    ) : DuplicatesUiState

    /** The candidate groups, most confident first. */
    data class Content(
        val groups: ImmutableList<DuplicateGroup> = persistentListOf(),
        /** How many assets were compared. */
        val sampled: Int = 0,
        /** Whether the comparison covered the whole library. */
        val isComplete: Boolean = true,
    ) : DuplicatesUiState
}

/**
 * Candidate duplicates awaiting a keep-or-drop decision.
 *
 * "Candidate" is load-bearing. Nothing here deletes: discarding a copy is an ordinary soft
 * delete with the album's signed retention window, so a wrong call by the finder or by the
 * user costs a restore rather than a photograph. The screen never acts without being asked.
 */
class DuplicatesViewModel(
    private val sample: LibrarySample,
    private val timeline: TimelineRepository,
    private val clock: Clock,
) : ViewModel() {
    private val state = MutableStateFlow<DuplicatesUiState>(DuplicatesUiState.Loading)

    /** What the view is showing right now. */
    val uiState: StateFlow<DuplicatesUiState> = state.asStateFlow()

    init {
        scan()
    }

    /** Re-reads the sample and recomputes the groups. */
    fun scan() {
        viewModelScope.launch {
            state.value = DuplicatesUiState.Loading
            state.value =
                try {
                    val sampled = sample.assets()
                    DuplicatesUiState.Content(
                        groups = DuplicateFinder.find(sampled.assets).toImmutableList(),
                        sampled = sampled.assets.size,
                        isComplete = sampled.isComplete,
                    )
                } catch (failure: CapsuleError) {
                    DuplicatesUiState.Failed(failure.code?.value)
                }
        }
    }

    /**
     * Keeps [keep] and moves every other member of its group to the trash.
     *
     * The retention deadline is computed here and handed to the repository rather than left
     * to a server-side policy, because that is the guarantee: the floor on the purge is signed
     * into the delete manifest at delete time, so nothing downstream can shorten it.
     */
    fun keepOnly(
        group: DuplicateGroup,
        keep: AssetId,
    ) {
        val retentionUntil = clock.instant().plus(DeletionState.DEFAULT_RETENTION)
        viewModelScope.launch {
            group.members
                .filter { it.id != keep }
                .forEach { asset -> runCatching { timeline.softDelete(asset.id, retentionUntil) } }
            scan()
        }
    }
}
