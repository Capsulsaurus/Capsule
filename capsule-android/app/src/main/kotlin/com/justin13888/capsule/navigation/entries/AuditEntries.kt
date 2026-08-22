package com.justin13888.capsule.navigation.entries

import androidx.compose.runtime.getValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.feature.audit.AuditLogScreen
import com.justin13888.capsule.feature.audit.AuditRecordScreen
import com.justin13888.capsule.feature.audit.AuditViewModel
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.destination
import com.justin13888.capsule.shell.panes.CapsuleListDetailPane
import org.koin.androidx.compose.koinViewModel

/**
 * The account's security-relevant event log.
 *
 * One destination, rendered as a listing beside the record behind the selected row. The pane
 * scaffold owns the width decision and its own back behaviour, so the log is a single screen
 * on a phone and two panes on a tablet without the back stack knowing either happened.
 *
 * The view model is resolved once, here, and both panes render from the same state. Resolving
 * it separately per pane would work — they share a store owner — but it would leave the
 * detail free to read a selection the list had already moved past, and a two-pane layout is
 * exactly where that disagreement is visible.
 */
fun EntryProviderScope<NavKey>.auditEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.AuditLog>(scope) {
        val viewModel = koinViewModel<AuditViewModel>()
        val state by viewModel.state.collectAsStateWithLifecycle()
        CapsuleListDetailPane(
            listPane = { openDetail ->
                AuditLogScreen(
                    state = state,
                    onSelectEntry = { entryId ->
                        viewModel.select(entryId)
                        openDetail(entryId)
                    },
                    onToggleCategory = viewModel::toggleCategory,
                    onToggleThisDevice = viewModel::toggleThisDevice,
                    onClearFilters = viewModel::clearFilters,
                    onRetry = viewModel::reload,
                )
            },
            detailPane = { contentKey ->
                AuditRecordScreen(entry = state.entry(contentKey), devices = state.devices)
            },
        )
    }
}
