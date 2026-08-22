package com.justin13888.capsule.navigation.entries

import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.feature.recovery.BackupExportRoute
import com.justin13888.capsule.feature.recovery.BackupRestoreRoute
import com.justin13888.capsule.feature.recovery.RecoveryRewrapRoute
import com.justin13888.capsule.feature.recovery.RecoveryStatusRoute
import com.justin13888.capsule.feature.recovery.RecoveryVerifyRoute
import com.justin13888.capsule.feature.recovery.ShamirRoute
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.destination

/**
 * Everything that decides whether a lost device is an inconvenience or a lost library.
 *
 * Six destinations for what could be a wizard, because each is somewhere a user needs to be
 * able to arrive at directly — from a reminder, from a support article, from settings — and a
 * wizard step with no key of its own cannot be linked to.
 *
 * Filled in by `:capsule-android:feature:recovery`.
 *
 * Every hop below pushes rather than replacing, so the back gesture always returns to where
 * the user came from. That matters most on the two paths that fork: a verification that keeps
 * missing offers the re-wrap, and a re-wrap that invalidated guardian shares offers to
 * re-issue them. Both are suggestions, and a suggestion the user can back out of is a
 * suggestion rather than a trap.
 */
fun EntryProviderScope<NavKey>.recoveryEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.RecoveryStatus>(scope) {
        RecoveryStatusRoute(
            onVerify = { scope.navigator.goTo(CapsuleDestination.RecoveryVerify) },
            onRewrap = { scope.navigator.goTo(CapsuleDestination.RecoveryRewrap) },
            onShamir = { scope.navigator.goTo(CapsuleDestination.Shamir) },
            onExport = { scope.navigator.goTo(CapsuleDestination.BackupExport) },
            onRestore = { scope.navigator.goTo(CapsuleDestination.BackupRestore) },
        )
    }
    destination<CapsuleDestination.RecoveryVerify>(scope) {
        RecoveryVerifyRoute(onRewrap = { scope.navigator.goTo(CapsuleDestination.RecoveryRewrap) })
    }
    destination<CapsuleDestination.RecoveryRewrap>(scope) {
        RecoveryRewrapRoute(onReissueShares = { scope.navigator.goTo(CapsuleDestination.Shamir) })
    }
    destination<CapsuleDestination.Shamir>(scope) { ShamirRoute() }
    destination<CapsuleDestination.BackupExport>(scope) { BackupExportRoute() }
    destination<CapsuleDestination.BackupRestore>(scope) { BackupRestoreRoute() }
}
