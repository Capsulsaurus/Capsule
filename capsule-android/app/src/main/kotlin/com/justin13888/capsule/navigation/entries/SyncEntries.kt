package com.justin13888.capsule.navigation.entries

import androidx.compose.runtime.getValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.feature.sync.QuarantineActions
import com.justin13888.capsule.feature.sync.QuarantineScreen
import com.justin13888.capsule.feature.sync.QuarantineViewModel
import com.justin13888.capsule.feature.sync.SyncCentreActions
import com.justin13888.capsule.feature.sync.SyncCentreScreen
import com.justin13888.capsule.feature.sync.SyncCentreViewModel
import com.justin13888.capsule.feature.sync.TransferQueueActions
import com.justin13888.capsule.feature.sync.TransferQueueScreen
import com.justin13888.capsule.feature.sync.TransferQueueViewModel
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.destination
import org.koin.androidx.compose.koinViewModel

/**
 * Sync state, the upload queue behind it, and whatever it refused to accept.
 *
 * The upload and download queues share the `UploadQueue` destination rather than each having
 * one: they answer the same question — what is moving, and what is holding it up — against the
 * same connection class, and two keys would let a user read one half and conclude the other
 * was broken. The quarantine surface stays its own destination because it is the one place
 * that asks the user to decide something.
 */
fun EntryProviderScope<NavKey>.syncEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.SyncCentre>(scope) {
        val viewModel = koinViewModel<SyncCentreViewModel>()
        val state by viewModel.state.collectAsStateWithLifecycle()
        SyncCentreScreen(
            state = state,
            actions =
                SyncCentreActions(
                    onForceSync = viewModel::forceSyncNow,
                    onSnoozeStale = viewModel::snoozeStaleWarning,
                    onEnableStale = viewModel::enableStaleWarning,
                    onOpenQueues = { scope.navigator.goTo(CapsuleDestination.UploadQueue) },
                    onOpenQuarantine = { scope.navigator.goTo(CapsuleDestination.Quarantine) },
                    onRetry = viewModel::reload,
                    onBack = { scope.navigator.goBack() },
                ),
        )
    }

    destination<CapsuleDestination.UploadQueue>(scope) {
        val viewModel = koinViewModel<TransferQueueViewModel>()
        val state by viewModel.state.collectAsStateWithLifecycle()
        TransferQueueScreen(
            state = state,
            actions =
                TransferQueueActions(
                    onFetchNow = viewModel::fetchNow,
                    onRetry = viewModel::reload,
                    onBack = { scope.navigator.goBack() },
                ),
        )
    }

    destination<CapsuleDestination.Quarantine>(scope) {
        val viewModel = koinViewModel<QuarantineViewModel>()
        val state by viewModel.state.collectAsStateWithLifecycle()
        QuarantineScreen(
            state = state,
            actions =
                QuarantineActions(
                    onResolve = viewModel::resolve,
                    onRetry = viewModel::reload,
                    onBack = { scope.navigator.goBack() },
                ),
        )
    }
}
