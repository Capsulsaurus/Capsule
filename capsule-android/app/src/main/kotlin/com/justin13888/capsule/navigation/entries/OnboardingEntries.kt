package com.justin13888.capsule.navigation.entries

import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavKey
import com.justin13888.capsule.feature.enrollment.FirstDeviceRoute
import com.justin13888.capsule.feature.onboarding.OnboardingCompleteRoute
import com.justin13888.capsule.feature.onboarding.OnboardingServerRoute
import com.justin13888.capsule.feature.onboarding.OnboardingSignInRoute
import com.justin13888.capsule.feature.onboarding.OnboardingWelcomeRoute
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.destination

/**
 * First run, from nothing to a library.
 *
 * Every step is immersive: the navigation suite would offer a way out of a flow that has not
 * yet produced an account for those tabs to show anything about.
 *
 * Two things about the routing are product decisions rather than plumbing.
 *
 * **The local-only path is a first-class exit, not a skip.** Both the welcome screen and the
 * server screen can reach the completion screen directly, because a user who chooses to keep
 * their library on this device — or who cannot reach the server they named — has finished
 * onboarding rather than abandoned it. The Local Gallery contract (FR2) says never signing in
 * is a valid, complete mode, and a routing table that only reached the end through sign-in
 * would say the opposite.
 *
 * **The passphrase destination is filled by `feature:enrollment`.** Creating the account keys
 * and revealing the passphrase they are escrowed under is first-device *enrollment*; it lands
 * here because that is where the onboarding flow reaches it, and it is owned there because it
 * is the same ceremony the enrollment module runs.
 */
fun EntryProviderScope<NavKey>.onboardingEntries(scope: CapsuleEntryScope) {
    destination<CapsuleDestination.OnboardingWelcome>(scope) {
        OnboardingWelcomeRoute(
            onServerChosen = { scope.navigator.goTo(CapsuleDestination.OnboardingServer) },
            onLocalOnlyChosen = { scope.navigator.goTo(CapsuleDestination.OnboardingComplete) },
        )
    }
    destination<CapsuleDestination.OnboardingServer>(scope) {
        OnboardingServerRoute(
            onServerAccepted = { scope.navigator.goTo(CapsuleDestination.OnboardingSignIn) },
            onLocalOnlyChosen = { scope.navigator.goTo(CapsuleDestination.OnboardingComplete) },
        )
    }
    destination<CapsuleDestination.OnboardingSignIn>(scope) {
        OnboardingSignInRoute(
            onSignedIn = { scope.navigator.goTo(CapsuleDestination.OnboardingPassphrase) },
        )
    }
    destination<CapsuleDestination.OnboardingPassphrase>(scope) {
        FirstDeviceRoute(
            onFinished = { scope.navigator.goTo(CapsuleDestination.OnboardingComplete) },
        )
    }
    destination<CapsuleDestination.OnboardingComplete>(scope) {
        // `switchTo` rather than `goTo`: onboarding is over, and leaving it on the back stack
        // would let a back press drop the user into a ceremony that has already happened.
        OnboardingCompleteRoute(onFinish = { scope.navigator.switchTo(CapsuleDestination.start) })
    }
}
