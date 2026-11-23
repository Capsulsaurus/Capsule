package com.justin13888.capsule.feature.drops

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.AlbumRepository
import com.justin13888.capsule.core.data.DropRepository
import com.justin13888.capsule.core.data.SharingRepository
import com.justin13888.capsule.core.model.Album
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.AlbumKind
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.DropId
import com.justin13888.capsule.core.model.PendingDrop
import com.justin13888.capsule.core.model.UploadLink
import com.justin13888.capsule.core.model.knownOrNull
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/** One album a drop may be adopted into. */
@Immutable
data class AlbumOption(
    /** The album's id. */
    val id: AlbumId,
    /** The album's name, or empty for the nameless default container. */
    val name: String,
)

/**
 * One pending deposit as the inbox renders it.
 *
 * [viaLinkOpaqueId] is the id that appeared in the guest's URL, resolved here so the
 * origin note the adoption records can name the link it came through. It is `null` when the
 * link has since been revoked, which is a fact worth showing rather than hiding: a deposit
 * whose door has already been closed is exactly the one a user should look at hardest.
 */
@Immutable
data class DropRow(
    /** The deposit itself. Every field on its descriptor is guest-supplied. */
    val drop: PendingDrop,
    /** The opaque id of the link it arrived through, when that link is still provisioned. */
    val viaLinkOpaqueId: String? = null,
)

/**
 * Everything the drop inbox draws, as one value.
 *
 * There is no adopted or discarded state here because there is no such row: both operations
 * remove the inbox entry in the same step that mints the asset or releases the bytes, so a
 * drop can never be observed half-adopted. What the screen tracks instead is which drop is
 * currently *being* decided about.
 */
@Immutable
data class DropInboxUiState(
    /** Whether the first reading is still in flight. */
    val isLoading: Boolean = true,
    /** Why the last operation or reading failed, or `null`. */
    val failure: DropsFailure? = null,
    /** Everything awaiting review, newest first. */
    val drops: List<DropRow> = emptyList(),
    /** The container albums a drop may be adopted into. */
    val albums: List<AlbumOption> = emptyList(),
    /** The drop whose album picker is open, or `null`. */
    val choosingAlbumFor: DropId? = null,
    /** The drop an adopt or discard is currently running for. */
    val busyDropId: DropId? = null,
) {
    /** Nothing has arrived. */
    val isEmpty: Boolean get() = !isLoading && drops.isEmpty()

    /**
     * Whether adoption can be offered at all.
     *
     * A drop has to land in a container album — that is where the key it gets rewrapped
     * under lives — so a library with no container has nowhere to put one.
     */
    val canAdopt: Boolean get() = albums.isNotEmpty()

    /** The drop the album picker is open for. */
    val pendingAdoption: DropRow? get() = drops.firstOrNull { it.drop.id == choosingAlbumFor }
}

/**
 * The drop inbox's state machine.
 *
 * The inbox is a **review surface**. Nothing here is in the library, nothing is applied
 * automatically, and the two operations this exposes are the only two futures a deposit has.
 * Adoption records the adopter as the asset's author and the guest's claim about it as an
 * unverified note; the guest is never a signer, on any path.
 */
class DropInboxViewModel(
    private val drops: DropRepository,
    private val albums: AlbumRepository,
    private val sharing: SharingRepository,
) : ViewModel() {
    private val mutableState = MutableStateFlow(DropInboxUiState())

    /** Everything the inbox draws. */
    val state: StateFlow<DropInboxUiState> = mutableState.asStateFlow()

    private var collection: Job? = null

    init {
        reload()
    }

    /** Restarts the reading. Also the remediation behind the error state's action. */
    fun reload() {
        collection?.cancel()
        mutableState.update { it.copy(isLoading = true, failure = null) }
        collection =
            viewModelScope.launch {
                combine(
                    drops.inbox(),
                    albums.albums(),
                    sharing.uploadLinks(),
                ) { inbox, albumList, links -> Reading(inbox, albumList, links) }
                    .catch { throwable ->
                        val failure = DropsFailure.of(throwable)
                        mutableState.update { it.copy(isLoading = false, failure = failure) }
                    }.collect { reading -> mutableState.update { it.loaded(reading) } }
            }
    }

    /** Opens the album picker for one deposit. */
    fun beginAdoption(dropId: DropId) {
        mutableState.update { it.copy(choosingAlbumFor = dropId, failure = null) }
    }

    /** Closes the album picker without adopting anything. */
    fun cancelAdoption() {
        mutableState.update { it.copy(choosingAlbumFor = null) }
    }

    /**
     * Adopts a deposit into [albumId], minting an asset.
     *
     * This is the only path from the inbox into the library and it runs on a trusted client
     * by construction. The adopter — never the guest — is recorded as the asset's creator.
     */
    fun adopt(
        dropId: DropId,
        albumId: AlbumId,
    ) {
        if (mutableState.value.busyDropId != null) return
        mutableState.update { it.copy(busyDropId = dropId, choosingAlbumFor = null, failure = null) }
        viewModelScope.launch {
            runCatching { drops.adopt(dropId, albumId) }
                .onSuccess { _: AssetId -> mutableState.update { it.copy(busyDropId = null) } }
                .onFailure { throwable ->
                    mutableState.update { it.copy(busyDropId = null, failure = DropsFailure.of(throwable)) }
                }
        }
    }

    /** Discards a deposit, releasing its bytes. Nothing is kept, and there is no undo. */
    fun discard(dropId: DropId) {
        if (mutableState.value.busyDropId != null) return
        mutableState.update { it.copy(busyDropId = dropId, choosingAlbumFor = null, failure = null) }
        viewModelScope.launch {
            runCatching { drops.discard(dropId) }
                .onSuccess { mutableState.update { it.copy(busyDropId = null) } }
                .onFailure { throwable ->
                    mutableState.update { it.copy(busyDropId = null, failure = DropsFailure.of(throwable)) }
                }
        }
    }

    private fun DropInboxUiState.loaded(reading: Reading): DropInboxUiState {
        val opaqueIds = reading.links.associate { it.id to it.opaqueId }
        val rows =
            reading.inbox
                .sortedByDescending { it.receivedAt }
                .map { drop -> DropRow(drop, opaqueIds[drop.viaLink]) }
        return copy(
            isLoading = false,
            failure = null,
            drops = rows,
            // Only containers: a view album holds no key and owns no assets, so it cannot be
            // an adoption destination.
            albums =
                reading.albums
                    .filter { it.kind.knownOrNull == AlbumKind.Container }
                    .map { AlbumOption(it.id, it.name) },
            choosingAlbumFor = choosingAlbumFor?.takeIf { id -> rows.any { it.drop.id == id } },
        )
    }

    /** One consistent reading of the inbox and everything it is named against. */
    private data class Reading(
        val inbox: List<PendingDrop>,
        val albums: List<Album>,
        val links: List<UploadLink>,
    )
}
