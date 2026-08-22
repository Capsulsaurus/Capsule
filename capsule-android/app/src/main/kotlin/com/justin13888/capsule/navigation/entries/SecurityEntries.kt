package com.justin13888.capsule.navigation.entries

import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.feature.security.CredentialsViewModel
import com.justin13888.capsule.feature.security.PasskeysScreen
import com.justin13888.capsule.feature.security.SecurityHubScreen
import com.justin13888.capsule.feature.security.SessionsActions
import com.justin13888.capsule.feature.security.SessionsScreen
import com.justin13888.capsule.feature.security.SessionsViewModel
import com.justin13888.capsule.feature.security.TotpActions
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.destination
import org.koin.androidx.compose.koinViewModel

/**
 * The security hub and the two lists it fronts.
 *
 * The hub reads both view models because it summarises both authorities — where the account is
 * signed in, and what proves it is you — and the authenticator factor lives there rather than
 * behind a fourth destination, since the routing table has no key for it and a factor the user
 * has to hunt for is a factor they do not turn on.
 */
fun EntryProviderScope<NavKey>.securityEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.SecurityHub>(scope) {
        val sessionsViewModel = koinViewModel<SessionsViewModel>()
        val credentialsViewModel = koinViewModel<CredentialsViewModel>()
        val sessions by sessionsViewModel.state.collectAsStateWithLifecycle()
        val credentials by credentialsViewModel.state.collectAsStateWithLifecycle()
        val totp =
            remember(credentialsViewModel) {
                TotpActions(
                    onBegin = credentialsViewModel::beginTotpEnrollment,
                    onCodeChange = credentialsViewModel::onCodeChange,
                    onConfirm = credentialsViewModel::confirmTotp,
                    onCancel = credentialsViewModel::cancelTotpEnrollment,
                    onDisable = credentialsViewModel::disableTotp,
                )
            }
        SecurityHubScreen(
            sessions = sessions,
            credentials = credentials,
            totp = totp,
            onOpenSessions = { scope.navigator.goTo(CapsuleDestination.SessionsAndDevices) },
            onOpenPasskeys = { scope.navigator.goTo(CapsuleDestination.Passkeys) },
        )
    }
    destination<CapsuleDestination.SessionsAndDevices>(scope) {
        val viewModel = koinViewModel<SessionsViewModel>()
        val state by viewModel.state.collectAsStateWithLifecycle()
        val actions =
            remember(viewModel) {
                SessionsActions(
                    onRevokeSession = viewModel::revokeSession,
                    onRevokeDevice = viewModel::revokeDevice,
                    onAskRevokeAll = viewModel::askRevokeAll,
                    onConfirmRevokeAll = viewModel::confirmRevokeAll,
                    onDismissRevokeAll = viewModel::dismissRevokeAll,
                    onReportCohort = viewModel::openSupportReport,
                    onDismissReport = viewModel::dismissSupportReport,
                    onRetry = viewModel::reload,
                )
            }
        SessionsScreen(state = state, actions = actions)
    }
    destination<CapsuleDestination.Passkeys>(scope) {
        val viewModel = koinViewModel<CredentialsViewModel>()
        val state by viewModel.state.collectAsStateWithLifecycle()
        PasskeysScreen(
            state = state,
            onAddPasskey = viewModel::addPasskey,
            onDeletePasskey = viewModel::deletePasskey,
            onRetry = viewModel::reload,
        )
    }
}
