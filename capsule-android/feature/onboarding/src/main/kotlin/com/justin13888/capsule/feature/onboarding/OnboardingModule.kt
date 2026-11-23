package com.justin13888.capsule.feature.onboarding

import org.koin.android.ext.koin.androidContext
import org.koin.core.module.Module
import org.koin.core.module.dsl.viewModelOf
import org.koin.dsl.module

/**
 * Everything `feature:onboarding` contributes to the application graph.
 *
 * Declared here rather than in the app's DI file so that adding a binding is a change inside
 * this module. The app assembles the graph; it does not need to know what this module needs
 * to build a screen.
 *
 * [OnboardingSession] is the one singleton: the app's navigation gives each destination its
 * own `ViewModelStore`, so the answers gathered on one screen would be invisible to the next
 * if they lived in a view model.
 */
val onboardingFeatureModule: Module =
    module {
        single { OnboardingSession() }
        single<ServerProbe> { FixtureServerProbe() }
        single<AuthGateway> { FixtureAuthGateway() }
        single<MediaAccessProbe> { AndroidMediaAccessProbe(androidContext()) }

        viewModelOf(::WelcomeViewModel)
        viewModelOf(::ServerConnectViewModel)
        viewModelOf(::SignInViewModel)
        viewModelOf(::OnboardingCompleteViewModel)
    }
