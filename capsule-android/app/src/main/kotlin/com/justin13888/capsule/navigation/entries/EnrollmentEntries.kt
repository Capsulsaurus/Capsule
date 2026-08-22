package com.justin13888.capsule.navigation.entries

import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.feature.enrollment.EnrollmentApproveRoute
import com.justin13888.capsule.feature.enrollment.EnrollmentCodeRoute
import com.justin13888.capsule.feature.enrollment.EnrollmentCompleteRoute
import com.justin13888.capsule.feature.enrollment.EnrollmentScanRoute
import com.justin13888.capsule.feature.enrollment.EnrollmentStartRoute
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.destination

/**
 * Adding a device to an account that already exists.
 *
 * Both sides of the exchange live here — the device being added and the device approving it —
 * because they are the same protocol seen from two ends, and a user runs both within a minute
 * of each other.
 *
 * The two sides converge on the same destination. Whether this device showed the code or read
 * one, the next screen is the safety-code comparison, because that comparison is the whole of
 * the channel's integrity and both ends have to perform it. Routing them to two different
 * screens would be two chances to get the one check that matters wrong.
 */
fun EntryProviderScope<NavKey>.enrollmentEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.EnrollmentStart>(scope) {
        EnrollmentStartRoute(
            onShowCode = { scope.navigator.goTo(CapsuleDestination.EnrollmentCode) },
            onScanCode = { scope.navigator.goTo(CapsuleDestination.EnrollmentScan) },
        )
    }
    destination<CapsuleDestination.EnrollmentScan>(scope) {
        EnrollmentScanRoute(
            onCodeAccepted = { scope.navigator.goTo(CapsuleDestination.EnrollmentApprove) },
        )
    }
    destination<CapsuleDestination.EnrollmentCode>(scope) {
        EnrollmentCodeRoute(
            onCompareSafetyCode = { scope.navigator.goTo(CapsuleDestination.EnrollmentApprove) },
        )
    }
    destination<CapsuleDestination.EnrollmentApprove>(scope) {
        EnrollmentApproveRoute(
            onSettled = { scope.navigator.goTo(CapsuleDestination.EnrollmentComplete) },
        )
    }
    destination<CapsuleDestination.EnrollmentComplete>(scope) {
        EnrollmentCompleteRoute(onDone = { scope.navigator.goBack() })
    }
}
