package com.justin13888.capsule.feature.sync

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.SettingsRepository
import com.justin13888.capsule.core.data.SyncRepository
import com.justin13888.capsule.core.data.TimelineRepository
import com.justin13888.capsule.core.model.StaleSyncNotification
import com.justin13888.capsule.core.model.SyncScope
import com.justin13888.capsule.core.model.UploadPolicy
import kotlinx.collections.immutable.toImmutableList
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.time.Clock
import java.time.Duration

/**
 * The sync centre's state machine.
 *
 * There is no pause or resume here and there never will be: every background window is
 * preemptible by construction, so there is no "must finish" state to suspend. The one control
 * that *does* exist is [forceSyncNow], and it is the only path that ignores the metered and
 * Wi-Fi rules — because the user asked for it, having been shown what it costs.
 */
class SyncCentreViewModel(
    private val sync: SyncRepository,
    private val settings: SettingsRepository,
    private val timeline: TimelineRepository,
    private val peers: PeerDiscovery,
    private val quarantine: QuarantineSource,
    private val clock: Clock,
) : ViewModel() {
    private val mutableState = MutableStateFlow(SyncCentreUiState())

    /** Everything the sync centre draws. */
    val state: StateFlow<SyncCentreUiState> = mutableState.asStateFlow()

    private var collection: Job? = null

    init {
        reload()
    }

    /** Restarts the reads behind the screen. Also the error state's remediation. */
    fun reload() {
        collection?.cancel()
        mutableState.update { it.copy(isLoading = true, failure = null, hasLocalFailure = false) }
        collection =
            viewModelScope.launch {
                combine(
                    sync.connectionClass(),
                    sync.lastCompletedSync(),
                    sync.syncScope(),
                    sync.uploadPolicy(),
                    settings.settings(),
                ) { connection, last, scope, policy, library ->
                    SyncCentreUiState(
                        isLoading = false,
                        connection = connection,
                        lastCompletedSync = last,
                        now = clock.instant(),
                        scope = scope,
                        uploadPolicy = policy,
                        staleNotification = library.staleSyncNotification,
                    )
                }.catch { throwable -> mutableState.update { it.failed(throwable) } }
                    .collect { snapshot -> mutableState.update { current -> current.merged(snapshot) } }
            }
        viewModelScope.launch { collectPeers() }
        viewModelScope.launch { refreshCounts() }
    }

    /**
     * Runs a reconciliation now, with explicit consent to ignore the connection rules.
     *
     * Only ever user-initiated. Background work never bypasses those rules, because a user
     * cannot consent to a data charge they were never shown.
     */
    fun forceSyncNow() {
        if (mutableState.value.isSyncing) return
        mutableState.update { it.copy(isSyncing = true, failure = null, hasLocalFailure = false) }
        viewModelScope.launch {
            runCatching { sync.forceSyncNow() }
                .onFailure { throwable -> mutableState.update { it.failed(throwable) } }
            mutableState.update { it.copy(isSyncing = false) }
            refreshCounts()
        }
    }

    /** Changes how much of each new asset this library fetches. Lowering it evicts nothing. */
    fun setScope(scope: SyncScope) {
        viewModelScope.launch {
            runCatching { sync.setSyncScope(scope) }
                .onFailure { throwable -> mutableState.update { it.failed(throwable) } }
        }
    }

    /** Changes whether upload sessions open eagerly or in tier order. */
    fun setUploadPolicy(policy: UploadPolicy) {
        viewModelScope.launch {
            runCatching { sync.setUploadPolicy(policy) }
                .onFailure { throwable -> mutableState.update { it.failed(throwable) } }
        }
    }

    /**
     * Suppresses the staleness warning for [window], or turns it off entirely with `null`.
     *
     * Snoozing and disabling are kept distinct all the way down because they mean different
     * things: a user who silenced a notification has not asked to stop backing up their
     * photos, and neither choice touches auto-sync.
     */
    fun snoozeStaleWarning(window: Duration?) {
        val mode =
            if (window == null) {
                StaleSyncNotification.Disabled
            } else {
                StaleSyncNotification.Snoozed(clock.instant().plus(window))
            }
        setStaleNotification(mode)
    }

    /** Puts the staleness warning back. */
    fun enableStaleWarning() {
        setStaleNotification(StaleSyncNotification.Enabled)
    }

    private fun setStaleNotification(mode: StaleSyncNotification) {
        viewModelScope.launch {
            runCatching { settings.setStaleSyncNotification(mode) }
                .onFailure { throwable -> mutableState.update { it.failed(throwable) } }
        }
    }

    private suspend fun collectPeers() {
        peers
            .peers()
            .catch { throwable -> mutableState.update { it.failed(throwable) } }
            .collect { found -> mutableState.update { it.copy(peers = found.toImmutableList()) } }
    }

    /**
     * Recounts the two queues the centre links to.
     *
     * A one-shot read rather than a flow: both counts are derived from a paged timeline read,
     * and re-deriving them on every revision would walk the window on each keystroke elsewhere
     * in the app for a number that only has to be right when this screen is looked at.
     */
    private suspend fun refreshCounts() {
        runCatching {
            val assets = timeline.timelineWindow(offset = 0, limit = COUNT_WINDOW)
            val connection = mutableState.value.connection
            val pending = TransferQueues.uploads(assets, connection).sumOf(UploadTierGroup::pending)
            pending to quarantine.items().size
        }.onSuccess { (pending, held) ->
            mutableState.update { it.copy(pendingUploads = pending, quarantined = held) }
        }.onFailure { throwable -> mutableState.update { it.failed(throwable) } }
    }

    /**
     * Keeps what a separate read already filled in.
     *
     * The failure is carried across on purpose: a refused force-sync bumps the same revision
     * the combine is watching, so a snapshot that reset it would erase the very message the
     * refusal exists to deliver, one frame after it appeared.
     */
    private fun SyncCentreUiState.merged(snapshot: SyncCentreUiState): SyncCentreUiState =
        snapshot.copy(
            pendingUploads = pendingUploads,
            quarantined = quarantined,
            peers = peers,
            isSyncing = isSyncing,
            failure = failure,
            hasLocalFailure = hasLocalFailure,
        )

    private fun SyncCentreUiState.failed(throwable: Throwable): SyncCentreUiState =
        copy(
            failure = (throwable as? CapsuleError)?.code,
            hasLocalFailure = throwable !is CapsuleError || throwable.code == null,
        )

    private companion object {
        /** How far into the library the queue counts look. */
        const val COUNT_WINDOW = 2_000
    }
}
