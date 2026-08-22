package com.justin13888.capsule.navigation.entries

import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.feature.moderation.AppealRoute
import com.justin13888.capsule.feature.moderation.BlocksRoute
import com.justin13888.capsule.feature.moderation.ReportRoute
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.destination

/**
 * Reporting, blocking, and contesting a decision.
 *
 * Filled in by `:capsule-android:feature:moderation`.
 *
 * A finished report pops rather than pushing a confirmation destination: the user came here
 * from whatever they were looking at, and the thing they want after sending is that, not a
 * receipt they have to dismiss.
 */
fun EntryProviderScope<NavKey>.moderationEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.ModerationReport>(scope) { key ->
        ReportRoute(subjectId = key.subjectId, onDone = { scope.navigator.goBack() })
    }
    destination<CapsuleDestination.ModerationBlocks>(scope) { BlocksRoute() }
    destination<CapsuleDestination.ModerationAppeal>(scope) { key -> AppealRoute(caseId = key.caseId) }
}
