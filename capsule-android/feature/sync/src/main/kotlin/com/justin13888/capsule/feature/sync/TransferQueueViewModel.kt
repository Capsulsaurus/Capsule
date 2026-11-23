package com.justin13888.capsule.feature.sync

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.SyncRepository
import com.justin13888.capsule.core.data.TimelineRepository
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.ConnectionClass
import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.SyncScope
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.persistentListOf
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/**
 * Everything the two transfer queues draw.
 *
 * Uploads and fetches share one screen and one state because they share one decision: the
 * connection class gates both, and a user looking at "why has nothing moved" needs to see the
 * two halves against the same answer rather than on two screens that could disagree.
 */
data class TransferQueueUiState(
    /** The first read is still in flight. */
    val isLoading: Boolean = true,
    /** The device's current path quality — what gates every rung below. */
    val connection: ConnectionClass = ConnectionClass.Unmetered,
    /** The staged ladder, all three rungs, empty ones included. */
    val uploads: ImmutableList<UploadTierGroup> = persistentListOf(),
    /** Assets holding less than the library's scope calls for. */
    val fetches: ImmutableList<FetchQueueEntry> = persistentListOf(),
    /** How much of each new asset this library fetches. */
    val scope: SyncScope = SyncScope.MetadataAndThumbnails,
    /** The coded failure to localize, or `null`. */
    val failure: ErrorCode? = null,
    /** Set when a failure carries no catalogue code — a purely local refusal. */
    val hasLocalFailure: Boolean = false,
) {
    /** Whether nothing at all is owed to the server. */
    val hasNoUploads: Boolean get() = uploads.all(UploadTierGroup::isClear)

    /** Whether nothing at all is owed to this device. */
    val hasNoFetches: Boolean get() = fetches.isEmpty()

    /** Whether any row has degraded to a lower rung than its scope asks for. */
    val hasDegraded: Boolean get() = fetches.any(FetchQueueEntry::isDegraded)

    /** Whether an error state should replace the content. */
    val hasFailed: Boolean get() = isLoading && (failure != null || hasLocalFailure)
}

/**
 * The upload and download queues' state machine.
 *
 * Both queues are **derived** from a window of the library rather than accumulated: the tier
 * queue is re-derivable from server truth by design, so holding one would be holding a second
 * source of truth that can only ever drift out of agreement with the feed.
 */
class TransferQueueViewModel(
    private val sync: SyncRepository,
    private val timeline: TimelineRepository,
) : ViewModel() {
    private val mutableState = MutableStateFlow(TransferQueueUiState())

    /** Everything the queue screen draws. */
    val state: StateFlow<TransferQueueUiState> = mutableState.asStateFlow()

    private var collection: Job? = null

    init {
        reload()
    }

    /** Re-derives both queues. Also the error state's remediation. */
    fun reload() {
        collection?.cancel()
        mutableState.update { it.copy(isLoading = true, failure = null, hasLocalFailure = false) }
        collection =
            viewModelScope.launch {
                combine(sync.connectionClass(), sync.syncScope(), timeline.revision()) { connection, scope, _ ->
                    connection to scope
                }.catch { throwable -> mutableState.update { it.failed(throwable) } }
                    .collect { (connection, scope) -> rebuild(connection, scope) }
            }
    }

    /**
     * Fetches one asset's missing rung now.
     *
     * The call answers with what is actually in hand rather than throwing, so a permanent
     * failure lands as a degraded row and the asset stays listed. Only that is why a fetch
     * button is safe to offer at all.
     */
    fun fetchNow(id: AssetId) {
        val wanted = mutableState.value.scope.tier
        viewModelScope.launch {
            runCatching { sync.ensureRepresentation(id, wanted) }
                .onFailure { throwable -> mutableState.update { it.failed(throwable) } }
            rebuild(mutableState.value.connection, mutableState.value.scope)
        }
    }

    private suspend fun rebuild(
        connection: ConnectionClass,
        scope: SyncScope,
    ) {
        runCatching { timeline.timelineWindow(offset = 0, limit = QUEUE_WINDOW) }
            .onSuccess { assets ->
                mutableState.update {
                    it.copy(
                        isLoading = false,
                        connection = connection,
                        scope = scope,
                        uploads = TransferQueues.uploads(assets, connection),
                        fetches = TransferQueues.fetches(assets, scope),
                    )
                }
            }.onFailure { throwable -> mutableState.update { it.failed(throwable) } }
    }

    private fun TransferQueueUiState.failed(throwable: Throwable): TransferQueueUiState =
        copy(
            failure = (throwable as? CapsuleError)?.code,
            hasLocalFailure = throwable !is CapsuleError || throwable.code == null,
        )

    private companion object {
        /**
         * How far into the library the queues look.
         *
         * Bounded rather than exhaustive: a queue is a list of what is moving now, and a
         * million-asset walk to draw it would make the screen unusable on the devices most
         * likely to have a backlog.
         */
        const val QUEUE_WINDOW = 2_000
    }
}
