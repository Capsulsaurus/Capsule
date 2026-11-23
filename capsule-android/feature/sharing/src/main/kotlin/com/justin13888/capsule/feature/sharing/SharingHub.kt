package com.justin13888.capsule.feature.sharing

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.AlbumRepository
import com.justin13888.capsule.core.data.DropRepository
import com.justin13888.capsule.core.data.SharingRepository
import com.justin13888.capsule.core.model.Album
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.ShareLink
import com.justin13888.capsule.core.model.ShareScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/**
 * One share link as the hub and the list render it.
 *
 * The album name is resolved here rather than in the composition so that a link to an album
 * that has since been deleted reads as a link to an album — with no name — instead of
 * disappearing from a list the user is trying to revoke things from.
 */
@Immutable
data class ShareLinkRow(
    /** The link itself. */
    val link: ShareLink,
    /** The album's name when the scope names one and it is still known, else `null`. */
    val albumName: String? = null,
) {
    /** Whether the link points at a whole album rather than a single item. */
    val isAlbumScope: Boolean get() = link.scope is ShareScope.WholeAlbum
}

/**
 * Everything the sharing hub and the share-link list draw, as one value.
 *
 * Share links and upload links are counted separately and never merged into one list. They
 * look symmetrical and carry opposite risk — a share link exposes what is already here, an
 * upload link accepts bytes from someone who can see nothing — and a single list would invite
 * a user to reason about the two the same way.
 */
@Immutable
data class SharingUiState(
    /** Whether the first reading is still in flight. */
    val isLoading: Boolean = true,
    /** Why the last reading failed, or `null`. */
    val failure: SharingFailure? = null,
    /** Every live share link this user has issued. */
    val shareLinks: List<ShareLinkRow> = emptyList(),
    /** How many upload links are provisioned, for the hub's second row. */
    val uploadLinkCount: Int = 0,
    /** How many guest deposits are waiting for review. */
    val pendingDropCount: Int = 0,
) {
    /** Nothing has been shared and nothing has been provisioned — a genuinely empty hub. */
    val isEmpty: Boolean
        get() = !isLoading && shareLinks.isEmpty() && uploadLinkCount == 0 && pendingDropCount == 0

    /** Whether the failure still leaves usable content on screen. */
    val isDegraded: Boolean get() = failure != null && shareLinks.isNotEmpty()
}

/**
 * The sharing hub's state machine.
 *
 * The four sources are combined rather than collected separately so the hub can never render
 * a link count from one reading beside a drop count from another; a hub that disagreed with
 * itself would be read as two bugs.
 *
 * The collection runs in a job this view model owns rather than in a `stateIn` pipeline,
 * because retry has to be able to *restart* it. A failure terminates the flow, and a pipeline
 * that could not be restarted would leave an error state whose retry button did nothing.
 */
class SharingViewModel(
    private val sharing: SharingRepository,
    private val drops: DropRepository,
    private val albums: AlbumRepository,
) : ViewModel() {
    private val mutableState = MutableStateFlow(SharingUiState())

    /** Everything the hub and the list draw. */
    val state: StateFlow<SharingUiState> = mutableState.asStateFlow()

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
                    sharing.shareLinks(),
                    sharing.uploadLinks(),
                    drops.inbox(),
                    albums.albums(),
                ) { shares, uploads, inbox, albumList ->
                    Reading(shares, uploads.size, inbox.size, albumList)
                }.catch { throwable -> mutableState.update { it.failed(throwable) } }
                    .collect { reading -> mutableState.update { it.loaded(reading) } }
            }
    }

    private fun SharingUiState.loaded(reading: Reading): SharingUiState {
        val names = reading.albums.associateBy(Album::id)
        return copy(
            isLoading = false,
            failure = null,
            shareLinks = reading.shares.map { link -> ShareLinkRow(link, link.albumName(names)) },
            uploadLinkCount = reading.uploadLinkCount,
            pendingDropCount = reading.pendingDropCount,
        )
    }

    private fun SharingUiState.failed(throwable: Throwable): SharingUiState =
        copy(isLoading = false, failure = SharingFailure.of(throwable))

    /** One consistent reading of every source the hub shows. */
    private data class Reading(
        val shares: List<ShareLink>,
        val uploadLinkCount: Int,
        val pendingDropCount: Int,
        val albums: List<Album>,
    )
}

/** The album's name when this link is album-scoped and that album is still known. */
internal fun ShareLink.albumName(albums: Map<AlbumId, Album>): String? {
    val scope = scope as? ShareScope.WholeAlbum ?: return null
    return albums[scope.albumId]?.name?.takeIf(String::isNotBlank)
}
