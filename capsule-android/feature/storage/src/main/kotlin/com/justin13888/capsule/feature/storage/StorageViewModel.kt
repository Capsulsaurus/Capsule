package com.justin13888.capsule.feature.storage

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.StorageRepository
import com.justin13888.capsule.core.data.SyncRepository
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.SyncScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.time.Clock

/**
 * The storage state machine.
 *
 * One immutable [StorageUiState] out and nothing else, so the quota screen and the local
 * screen render from the same value.
 *
 * Pinning and release progress are held here rather than read back from [StorageRepository],
 * because the contract has no read side for either: it can pin and it can answer a durability
 * question, but it cannot say which assets are pinned. Keeping the record here is honest
 * about that — the alternative would be a screen that renders a pin state it invented and
 * that a restart silently disagrees with.
 */
class StorageViewModel(
    private val source: StorageSource,
    private val storage: StorageRepository,
    private val sync: SyncRepository,
    private val clock: Clock = Clock.systemUTC(),
) : ViewModel() {
    private val mutableState = MutableStateFlow(StorageUiState())

    /** Everything the storage destinations draw. */
    val state: StateFlow<StorageUiState> = mutableState.asStateFlow()

    private val pinned = mutableSetOf<AssetId>()
    private val releases = mutableMapOf<AssetId, ReleaseState>()

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
                source
                    .snapshots()
                    .catch { throwable -> mutableState.update { it.failed(throwable) } }
                    .collect { snapshot -> mutableState.update { it.loaded(snapshot) } }
            }
    }

    /** Chooses the ceiling the automatic sweep keeps the reclaimable cache under. */
    fun setBudget(budget: CacheBudget) {
        mutableState.update { it.copy(budget = budget, reclaim = null) }
    }

    /**
     * Runs an eviction down to the chosen budget.
     *
     * Reports what it freed even when that is nothing: "already inside the budget" is a real
     * answer, and a button that appears to do nothing is one the user presses again.
     */
    fun reclaim() {
        if (mutableState.value.isReclaiming) return
        mutableState.update { it.copy(reclaim = ReclaimOutcome.Running) }
        viewModelScope.launch {
            runCatching { storage.evictCache(mutableState.value.budget.bytes) }
                .onSuccess { freed -> mutableState.update { it.copy(reclaim = ReclaimOutcome.Freed(freed)) } }
                .onFailure { throwable -> mutableState.update { it.copy(reclaim = null).failed(throwable) } }
        }
    }

    /** Pins or unpins one asset so cache pressure cannot evict it. */
    fun setPinned(
        id: AssetId,
        isPinned: Boolean,
    ) {
        viewModelScope.launch {
            runCatching { storage.setPinned(id, isPinned) }
                .onSuccess {
                    if (isPinned) pinned += id else pinned -= id
                    mutableState.update { it.withDecisions() }
                }.onFailure { throwable -> mutableState.update { it.failed(throwable) } }
        }
    }

    /**
     * Releases one device-owned original, but only once the server confirms it holds it.
     *
     * The verdict is asked for first and acted on second, and a `false` is rendered as a
     * state rather than swallowed: this is the last check before bytes that may exist nowhere
     * else stop existing here, so the failure to confirm has to be visible. A thrown check is
     * treated exactly like a `false` — an unanswered question is not a yes.
     */
    fun releaseOriginal(id: AssetId) {
        if (releases[id] == ReleaseState.Checking) return
        releases[id] = ReleaseState.Checking
        mutableState.update { it.withDecisions() }
        viewModelScope.launch {
            val durable = runCatching { storage.verifyDurable(id) }
            val verdict = durable.getOrDefault(false)
            if (verdict) {
                runCatching { storage.setPinned(id, false) }
                pinned -= id
                releases[id] = ReleaseState.Released
            } else {
                releases[id] = ReleaseState.Unconfirmed
            }
            mutableState.update { current ->
                val withVerdict = current.withDecisions()
                durable.fold({ withVerdict }, { throwable -> withVerdict.failedIfCoded(throwable) })
            }
        }
    }

    /** Changes how much of each new asset this device fetches. Lowering it evicts nothing. */
    fun setSyncScope(scope: SyncScope) {
        viewModelScope.launch {
            runCatching { sync.setSyncScope(scope) }
                .onFailure { throwable -> mutableState.update { it.failed(throwable) } }
        }
    }

    private fun StorageUiState.loaded(snapshot: StorageSnapshot): StorageUiState =
        copy(
            isLoading = false,
            failure = null,
            standing = QuotaStanding.of(snapshot.quota, clock.instant()),
            cacheBytes = snapshot.cacheBytes,
            syncScope = snapshot.syncScope,
            consumers = snapshot.consumers.map(::toConsumer),
        )

    /** Re-projects the rows with the current pin and release decisions folded back in. */
    private fun StorageUiState.withDecisions(): StorageUiState =
        copy(
            consumers =
                consumers.map { row ->
                    row.copy(
                        isPinned = row.id in pinned,
                        release = releases[row.id] ?: ReleaseState.Idle,
                    )
                },
        )

    private fun toConsumer(facts: ConsumerFacts): StorageConsumer =
        StorageConsumer(
            id = facts.id,
            capturedAt = facts.asset.captureTimestamp,
            assetType = facts.asset.knownType(),
            bytes = facts.bytes,
            held = facts.asset.heldRepresentations,
            isPinned = facts.id in pinned,
            release = releases[facts.id] ?: ReleaseState.Idle,
        )

    private fun StorageUiState.failed(throwable: Throwable): StorageUiState =
        copy(isLoading = false, failure = StorageFailure.of(throwable))

    /**
     * Surfaces a failure only when it carries something the user can act on.
     *
     * A release that could not reach the server has already been rendered as the unconfirmed
     * state, which says the useful thing; adding a second banner over it would report one
     * event twice.
     */
    private fun StorageUiState.failedIfCoded(throwable: Throwable): StorageUiState {
        val coded = StorageFailure.of(throwable)
        return if (coded.code == null) this else copy(failure = coded)
    }
}
