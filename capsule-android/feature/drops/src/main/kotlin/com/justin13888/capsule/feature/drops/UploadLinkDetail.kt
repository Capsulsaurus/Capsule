package com.justin13888.capsule.feature.drops

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.SharingRepository
import com.justin13888.capsule.core.model.UploadLink
import com.justin13888.capsule.core.model.UploadLinkId
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.time.Clock
import java.time.Instant

/**
 * How much of one cap has been spent.
 *
 * [limit] is nullable because an absent cap means unbounded, and unbounded is not the same
 * as zero: a bar drawn at 0% for a link with no ceiling would tell the user they have room
 * they were never promised. The screen renders a count instead when there is no limit.
 */
@Immutable
data class CapUsage(
    /** How much has been used. */
    val used: Long,
    /** The ceiling, or `null` when the cap was left unbounded. */
    val limit: Long?,
) {
    /** How full the cap is, 0f..1f. Zero when there is no ceiling to be full of. */
    val fraction: Float
        get() {
            val ceiling = limit ?: return 0f
            if (ceiling <= 0) return 1f
            return (used.toFloat() / ceiling.toFloat()).coerceIn(0f, 1f)
        }

    /** Whether this cap alone would now refuse a deposit. */
    val isExhausted: Boolean get() = limit != null && used >= limit
}

/**
 * Everything one upload link's detail draws, as one value.
 *
 * There is deliberately no shareable URL on this screen. An upload link's URL carries the
 * Drop Key's public half in its fragment, the server never receives it, and [UploadLink]
 * carries no field for it — so a "copy link" here could only produce a URL a guest's browser
 * has nothing to encrypt to. Showing the caps and the usage is the honest half of the screen.
 */
@Immutable
data class UploadLinkDetailUiState(
    /** Whether the first reading is still in flight. */
    val isLoading: Boolean = true,
    /** Why the screen cannot show the link, or `null`. */
    val failure: DropsFailure? = null,
    /** The link itself. */
    val link: UploadLink? = null,
    /** Whether it would accept a deposit at the moment of the reading. */
    val acceptsDeposits: Boolean = false,
    /** Whether its expiry has passed. */
    val hasExpired: Boolean = false,
    /** Whether a revocation is in flight. */
    val isRevoking: Boolean = false,
    /** Whether this screen revoked the link. */
    val isRevoked: Boolean = false,
) {
    /** Cumulative bytes against the cumulative ceiling. */
    val bytes: CapUsage? get() = link?.let { CapUsage(it.bytesReceived, it.caps.maxTotalBytes) }

    /** Deposited files against the file-count ceiling. */
    val files: CapUsage? get() = link?.let { CapUsage(it.filesReceived.toLong(), it.caps.maxFileCount?.toLong()) }

    /** The link is gone and this screen is not the reason. */
    val isMissing: Boolean get() = !isLoading && failure == null && link == null && !isRevoked

    /** Whether revocation is still available. */
    val canRevoke: Boolean get() = link != null && !isRevoked && !isRevoking
}

/**
 * One upload link's state machine.
 *
 * Read out of the provisioned-links flow rather than fetched by id, so a revocation made on
 * another device removes it here without this screen polling for it.
 */
class UploadLinkDetailViewModel(
    private val sharing: SharingRepository,
    private val linkId: UploadLinkId,
    private val clock: Clock = Clock.systemUTC(),
) : ViewModel() {
    private val mutableState = MutableStateFlow(UploadLinkDetailUiState())

    /** Everything the detail screen draws. */
    val state: StateFlow<UploadLinkDetailUiState> = mutableState.asStateFlow()

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
                sharing
                    .uploadLinks()
                    .catch { throwable ->
                        val failure = DropsFailure.of(throwable)
                        mutableState.update { it.copy(isLoading = false, failure = failure) }
                    }.collect { links ->
                        val link = links.firstOrNull { it.id == linkId }
                        mutableState.update { it.loaded(link, clock.instant()) }
                    }
            }
    }

    /**
     * Revokes the link.
     *
     * Deposits already in the inbox are untouched: revoking the door does not empty the room
     * behind it, and the screen says so before this runs.
     */
    fun revoke() {
        if (!mutableState.value.canRevoke) return
        mutableState.update { it.copy(isRevoking = true, failure = null) }
        viewModelScope.launch {
            runCatching { sharing.revokeUploadLink(linkId) }
                .onSuccess {
                    mutableState.update {
                        it.copy(isRevoking = false, isRevoked = true, acceptsDeposits = false)
                    }
                }.onFailure { throwable ->
                    mutableState.update { it.copy(isRevoking = false, failure = DropsFailure.of(throwable)) }
                }
        }
    }

    private fun UploadLinkDetailUiState.loaded(
        link: UploadLink?,
        now: Instant,
    ): UploadLinkDetailUiState =
        copy(
            isLoading = false,
            failure = null,
            link = link,
            acceptsDeposits = link?.acceptsDepositsAt(now) == true,
            hasExpired = link?.caps?.expiresAt?.let { !now.isBefore(it) } == true,
        )
}
