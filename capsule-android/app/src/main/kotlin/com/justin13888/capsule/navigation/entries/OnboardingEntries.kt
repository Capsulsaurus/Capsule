package com.justin13888.capsule.navigation.entries

import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.Placeholder
import com.justin13888.capsule.navigation.destination

/**
 * First run, from nothing to a library.
 *
 * Every step is immersive: the navigation suite would offer a way out of a flow that has not
 * yet produced an account for those tabs to show anything about.
 *
 * Filled in by `:capsule-android:feature:onboarding`.
 */
fun EntryProviderScope<NavKey>.onboardingEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.OnboardingWelcome>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.OnboardingServer>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.OnboardingSignIn>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.OnboardingPassphrase>(scope) { scope.Placeholder(it) }
    destination<CapsuleDestination.OnboardingComplete>(scope) { scope.Placeholder(it) }
}
