package com.justin13888.capsule.navigation.entries

import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.feature.storage.QuotaScreen
import com.justin13888.capsule.feature.storage.StorageActions
import com.justin13888.capsule.feature.storage.StorageScreen
import com.justin13888.capsule.feature.storage.StorageViewModel
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.destination
import org.koin.androidx.compose.koinViewModel

/**
 * What the library occupies and what the account is allowed.
 *
 * Two destinations over one view model type. Each entry resolves its own instance — they are
 * separate back-stack entries with separate stores — and that is fine here because neither
 * screen's state is a decision the other has to see: both read the same repositories, so both
 * settle on the same numbers.
 *
 * The remediations are wired to destinations that genuinely act on the state. The
 * grace-expired state points at the trash rather than at a paywall, because deletes and
 * restores are still admitted there by design and emptying trash is literally how a user gets
 * back under quota. A suspension points at About, which is where a deployment states how to
 * reach its operator — the one state the user cannot resolve alone.
 */
fun EntryProviderScope<NavKey>.storageEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.Storage>(scope) {
        val viewModel = koinViewModel<StorageViewModel>()
        val state by viewModel.state.collectAsStateWithLifecycle()
        val actions =
            remember(viewModel, scope) {
                StorageActions(
                    onBudgetChange = viewModel::setBudget,
                    onReclaim = viewModel::reclaim,
                    onPinnedChange = viewModel::setPinned,
                    onRelease = viewModel::releaseOriginal,
                    onSyncScopeChange = viewModel::setSyncScope,
                    onOpenQuota = { scope.navigator.goTo(CapsuleDestination.Quota) },
                    onRetry = viewModel::reload,
                )
            }
        StorageScreen(state = state, actions = actions)
    }
    destination<CapsuleDestination.Quota>(scope) {
        val viewModel = koinViewModel<StorageViewModel>()
        val state by viewModel.state.collectAsStateWithLifecycle()
        QuotaScreen(
            state = state,
            onReviewLargest = { scope.navigator.goTo(CapsuleDestination.Storage) },
            onOpenTrash = { scope.navigator.goTo(CapsuleDestination.Trash) },
            onContactSupport = { scope.navigator.goTo(CapsuleDestination.SettingsAbout) },
            onRetry = viewModel::reload,
        )
    }
}
