package com.justin13888.capsule.feature.sharing

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.AlbumRepository
import com.justin13888.capsule.core.data.SharingRepository
import com.justin13888.capsule.core.model.Album
import com.justin13888.capsule.core.model.ShareLinkId
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/**
 * Everything one share link's detail screen draws, as one value.
 *
 * [url] is nullable and that is the interesting case, not an oversight. The fragment secret
 * exists exactly once, on the device that minted the link, so a link read back from the
 * server has no shareable URL here at all — a degraded state the screen has to explain rather
 * than paper over with a copy button that yields a dead link.
 */
@Immutable
data class ShareLinkDetailUiState(
    /** Whether the first reading is still in flight. */
    val isLoading: Boolean = true,
    /** Why the screen cannot show the link, or `null`. */
    val failure: SharingFailure? = null,
    /** The link and its resolved album name. */
    val row: ShareLinkRow? = null,
    /** The complete URL including its fragment, or `null` when this device cannot build one. */
    val url: String? = null,
    /** Whether a revocation is in flight. */
    val isRevoking: Boolean = false,
    /** Whether this screen revoked the link. */
    val isRevoked: Boolean = false,
) {
    /**
     * The link is not in the issued list and this screen did not revoke it.
     *
     * Distinct from [isRevoked] on purpose: revoking here is a thing the user just did and
     * gets a confirmation, while a link that vanished was revoked or expired somewhere else
     * and needs an explanation instead.
     */
    val isMissing: Boolean get() = !isLoading && failure == null && row == null && !isRevoked

    /** Whether there is a complete URL to hand out. */
    val canShare: Boolean get() = url != null && !isRevoked

    /** Whether revocation is still available. */
    val canRevoke: Boolean get() = row != null && !isRevoked && !isRevoking
}

/**
 * One share link's state machine.
 *
 * The link is read out of the issued-links flow rather than fetched by id, so a revocation
 * made on another device removes it here without this screen having to poll — and the
 * "no longer live" state is reached by the same path whether the cause was revocation,
 * expiry, or a link that never existed. That indistinguishability is the design, not a
 * shortcut: a visitor cannot tell those three apart either.
 */
class ShareLinkDetailViewModel(
    private val sharing: SharingRepository,
    private val albums: AlbumRepository,
    private val linkId: ShareLinkId,
    private val origin: LinkOrigin,
) : ViewModel() {
    private val mutableState = MutableStateFlow(ShareLinkDetailUiState())

    /** Everything the detail screen draws. */
    val state: StateFlow<ShareLinkDetailUiState> = mutableState.asStateFlow()

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
                combine(sharing.shareLinks(), albums.albums()) { links, albumList ->
                    val link = links.firstOrNull { it.id == linkId }
                    link?.let { ShareLinkRow(it, it.albumName(albumList.associateBy(Album::id))) }
                }.catch { throwable ->
                    mutableState.update { it.copy(isLoading = false, failure = SharingFailure.of(throwable)) }
                }.collect { row ->
                    mutableState.update {
                        it.copy(
                            isLoading = false,
                            failure = null,
                            row = row,
                            url = row?.link?.shareUrl(origin),
                        )
                    }
                }
            }
    }

    /**
     * Revokes the link.
     *
     * There is no undo and the screen says so before this runs: to everyone holding the URL
     * the result is indistinguishable from a link that never existed, and nothing on the
     * server can put the secret back.
     */
    fun revoke() {
        if (!mutableState.value.canRevoke) return
        mutableState.update { it.copy(isRevoking = true, failure = null) }
        viewModelScope.launch {
            runCatching { sharing.revokeShareLink(linkId) }
                .onSuccess {
                    mutableState.update { it.copy(isRevoking = false, isRevoked = true, url = null) }
                }.onFailure { throwable ->
                    mutableState.update { it.copy(isRevoking = false, failure = SharingFailure.of(throwable)) }
                }
        }
    }
}
