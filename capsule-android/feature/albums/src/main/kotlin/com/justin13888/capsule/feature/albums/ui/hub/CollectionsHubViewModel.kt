package com.justin13888.capsule.feature.albums.ui.hub

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.AlbumRepository
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.SearchRepository
import com.justin13888.capsule.core.model.Album
import com.justin13888.capsule.core.model.AlbumCapability
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.AlbumKind
import com.justin13888.capsule.core.model.asWire
import com.justin13888.capsule.core.model.knownOrNull
import com.justin13888.capsule.feature.albums.data.FederatedAlbumSource
import com.justin13888.capsule.feature.albums.data.LibrarySample
import com.justin13888.capsule.feature.albums.model.SystemViewKind
import kotlinx.collections.immutable.toImmutableList
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.launch
import java.time.Clock
import java.util.UUID

/**
 * The Collections hub: every way into the library that is not the timeline.
 *
 * Four sections in one screen rather than four destinations, because the split the design
 * cares about is not navigational — it is between **containers**, which hold keys and own
 * assets, and **views**, which hold neither. Putting them in one place with that split
 * visible is what stops "share this album" becoming a question with no safe answer.
 *
 * Album membership arrives as a `Flow` and the media-type counts as a one-shot sample. That
 * asymmetry is the repository contract's, not a shortcut: a user has tens of albums and can
 * watch the list change, while a library of a million assets cannot be emitted as a list at
 * all.
 */
class CollectionsHubViewModel(
    private val albums: AlbumRepository,
    private val search: SearchRepository,
    private val federated: FederatedAlbumSource,
    private val sample: LibrarySample,
    private val clock: Clock,
) : ViewModel() {
    private val counts = MutableStateFlow<Map<SystemViewKind, Int>>(emptyMap())
    private val countsComplete = MutableStateFlow(false)

    private val state = MutableStateFlow<CollectionsHubUiState>(CollectionsHubUiState.Loading)

    /** What the hub is showing right now. */
    val uiState: StateFlow<CollectionsHubUiState> = state.asStateFlow()

    init {
        observe()
        refreshCounts()
    }

    /**
     * Creates a container album under [name].
     *
     * A container and never a view: a view holds no keys and owns no assets, so it could
     * never be the thing the user just said they wanted to put photographs into. The id is
     * minted here as a stand-in — the catalog mints the real UUIDv7 at insert time — and the
     * creator holds the admin tier, because someone has to be able to invite the second
     * member.
     */
    fun createAlbum(name: String) {
        val trimmed = name.trim()
        if (trimmed.isEmpty()) return
        viewModelScope.launch {
            val now = clock.instant()
            val album =
                Album(
                    id = AlbumId(UUID.randomUUID().toString()),
                    name = trimmed,
                    createdAt = now,
                    modifiedAt = now,
                    kind = AlbumKind.Container.asWire(),
                    viewerCapability = AlbumCapability.Admin.asWire(),
                )
            runCatching { albums.insertAlbum(album) }.onFailure { failure ->
                if (failure is CapsuleError) state.value = CollectionsHubUiState.Failed(failure.code?.value)
            }
        }
    }

    /** Re-reads the sample the media-type counts are drawn from. */
    fun refreshCounts() {
        viewModelScope.launch {
            val sampled =
                runCatching { sample.assets() }.getOrElse { failure ->
                    if (failure is CapsuleError) state.value = CollectionsHubUiState.Failed(failure.code?.value)
                    return@launch
                }
            counts.value =
                SystemViewKind.entries.associateWith { kind -> sampled.assets.count(kind::matches) }
            countsComplete.value = sampled.isComplete
        }
    }

    private fun observe() {
        viewModelScope.launch {
            combine(
                albums.albums(),
                search.smartAlbums(),
                federated.groups(),
                counts,
                countsComplete,
            ) { allAlbums, smartAlbums, groups, mediaCounts, complete ->
                CollectionsHubUiState.Content(
                    myAlbums =
                        allAlbums
                            .filter(::isContainer)
                            .sortedWith(ALBUM_ORDER)
                            .map(::toRow)
                            .toImmutableList(),
                    mediaTypes =
                        SystemViewKind.entries
                            .map { kind -> HubMediaType(kind, mediaCounts[kind] ?: 0, complete) }
                            .toImmutableList(),
                    viewAlbums = allAlbums.filterNot(::isContainer).map(::toRow).toImmutableList(),
                    federated = groups.toImmutableList(),
                    smartAlbumCount = smartAlbums.size,
                )
            }.catch { failure ->
                emit(CollectionsHubUiState.Content())
                if (failure is CapsuleError) state.value = CollectionsHubUiState.Failed(failure.code?.value)
            }.collect { content -> if (state.value !is CollectionsHubUiState.Failed) state.value = content }
        }
    }

    private fun isContainer(album: Album): Boolean = album.kind.knownOrNull == AlbumKind.Container

    private fun toRow(album: Album): HubAlbum =
        HubAlbum(
            id = album.id,
            name = album.name,
            isDefault = album.isDefault,
            assetCount = album.assetCount,
            capability = album.viewerCapability,
        )

    private companion object {
        /**
         * The default album leads, then the rest by name.
         *
         * Not by modification time: the nameless default is where unfiled imports land, so
         * it is the row the user reaches for most often and the one that must not move.
         */
        val ALBUM_ORDER: Comparator<Album> = compareByDescending<Album> { it.isDefault }.thenBy { it.name }
    }
}
