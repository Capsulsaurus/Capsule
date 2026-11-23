package com.justin13888.capsule.feature.albums.ui.trash

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.TimelineRepository
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.DeletionState
import com.justin13888.capsule.feature.albums.model.RetentionCountdown
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.persistentListOf
import kotlinx.collections.immutable.toImmutableList
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import java.time.Clock
import java.time.Instant

/** One trashed asset and how much of its signed retention window is left. */
data class TrashedItem(
    val asset: Asset,
    /** When the delete manifest was signed. */
    val deletedAt: Instant,
    /** The signed floor on the purge. */
    val retentionUntil: Instant,
    /** How long remains, as words rather than a duration. */
    val countdown: RetentionCountdown,
) {
    /** Whether a hard purge is permitted yet. Anything else is refused by the server. */
    val isPurgeable: Boolean get() = countdown is RetentionCountdown.Expired
}

/** What Recently Deleted is showing. */
sealed interface RecentlyDeletedUiState {
    /** Reading the window. */
    data object Loading : RecentlyDeletedUiState

    /** The read failed; [code] is the stable catalogue code. */
    data class Failed(
        val code: String?,
    ) : RecentlyDeletedUiState

    /** The trash, most recently deleted first. */
    data class Content(
        val items: ImmutableList<TrashedItem> = persistentListOf(),
        /** Whether anything is past its window, and so whether emptying would do anything. */
        val hasPurgeable: Boolean = false,
        /** A one-off refusal, e.g. a purge attempted before the window ended. */
        val refusedPurge: Boolean = false,
    ) : RecentlyDeletedUiState
}

/**
 * Recently Deleted, behind the shell's local-auth gate.
 *
 * The countdown is computed from the asset's own `retention_until`, which was **signed into
 * the delete manifest at delete time** rather than looked up from server config at purge
 * time. Reading it off the asset is the client-side shape of that guarantee: the deadline
 * travels with the deletion, so nothing downstream — including a hostile server — can
 * shorten it, and this screen never has to ask anyone how long is left.
 *
 * Emptying the trash purges only what is already past its window, and says so before it runs.
 * A client that purged everything would be doing exactly what the signed floor exists to
 * prevent.
 */
class RecentlyDeletedViewModel(
    private val timeline: TimelineRepository,
    private val clock: Clock,
) : ViewModel() {
    private val state = MutableStateFlow<RecentlyDeletedUiState>(RecentlyDeletedUiState.Loading)

    /** What the screen is showing right now. */
    val uiState: StateFlow<RecentlyDeletedUiState> = state.asStateFlow()

    init {
        load()
    }

    /** Re-reads the trash window. */
    fun load() {
        viewModelScope.launch {
            state.value =
                try {
                    val now = clock.instant()
                    val items = timeline.trash(0, PAGE_SIZE).mapNotNull { asset -> toItem(asset, now) }
                    RecentlyDeletedUiState.Content(
                        items = items.toImmutableList(),
                        hasPurgeable = items.any(TrashedItem::isPurgeable),
                    )
                } catch (failure: CapsuleError) {
                    RecentlyDeletedUiState.Failed(failure.code?.value)
                }
        }
    }

    /** Recovers an asset within its retention window; the delete record stays in the chain. */
    fun restore(id: AssetId) {
        viewModelScope.launch {
            runCatching { timeline.restore(id) }
            load()
        }
    }

    /**
     * Purges everything already past its signed window, and nothing else.
     *
     * The expired set is asked for rather than derived from the loaded page, so an item the
     * page did not reach is still purged and an item whose window has not passed still is not.
     */
    fun emptyTrash() {
        viewModelScope.launch {
            val expired = runCatching { timeline.expiredTrash(clock.instant()) }.getOrNull().orEmpty()
            var refused = false
            expired.forEach { asset ->
                runCatching { timeline.purge(asset.id) }.onFailure { refused = true }
            }
            load()
            if (refused) markRefused()
        }
    }

    /** Clears the refusal notice after the screen has shown it. */
    fun dismissRefusal() {
        val current = state.value as? RecentlyDeletedUiState.Content ?: return
        state.value = current.copy(refusedPurge = false)
    }

    private fun markRefused() {
        val current = state.value as? RecentlyDeletedUiState.Content ?: return
        state.value = current.copy(refusedPurge = true)
    }

    /** Drops anything the trash window handed back that is not actually trashed. */
    private fun toItem(
        asset: Asset,
        now: Instant,
    ): TrashedItem? {
        val trashed = asset.deletion as? DeletionState.Trashed ?: return null
        return TrashedItem(
            asset = asset,
            deletedAt = trashed.deletedAt,
            retentionUntil = trashed.retentionUntil,
            countdown = RetentionCountdown.of(trashed, now),
        )
    }

    private companion object {
        /** How much of the trash is read at once. */
        const val PAGE_SIZE = 256
    }
}
