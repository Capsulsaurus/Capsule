package com.justin13888.capsule.feature.enrollment

import com.justin13888.capsule.core.mock.EnrollmentGateway
import com.justin13888.capsule.core.mock.MockRepositories
import org.koin.core.module.Module
import org.koin.core.module.dsl.viewModel
import org.koin.core.module.dsl.viewModelOf
import org.koin.dsl.module

/**
 * Everything `feature:enrollment` contributes to the application graph.
 *
 * Declared here rather than in the app's DI file, so that adding a binding is a change inside
 * this module.
 *
 * The gateway is pulled off the one [MockRepositories] singleton rather than constructed here.
 * The whole set is only correct together — they share a backend — so a second instance would
 * quietly enroll devices into a directory nothing else reads.
 *
 * [EnrollmentSession] is a singleton because the app gives each destination its own
 * `ViewModelStore`: a code issued on one screen would otherwise be invisible to the screen
 * that compares safety codes.
 */
val enrollmentFeatureModule: Module =
    module {
        single { EnrollmentSession() }
        single<EnrollmentGateway> { get<MockRepositories>().enrollment }
        single<SecureElement> { MockSecureElement(get<MockRepositories>().backend) }
        single<FirstDeviceCeremony> { MockFirstDeviceCeremony(get(), get()) }
        single<EnrollmentCodeIssuer> { MockEnrollmentCodeIssuer(get<MockRepositories>().backend) }
        single<EnrollmentCodeReader> { MockEnrollmentCodeReader(get<MockRepositories>().backend) }
        // Bound rather than hard-coded so the app can point it at the real biometric prompt
        // without this module changing: the gate is a security property of the ceremony.
        single<EnrollmentAuthorization> { AlwaysConfirmingAuthorization() }

        // Explicit rather than `viewModelOf`, because the constructor takes a source of
        // randomness with a default that a test replaces to make the challenge deterministic.
        viewModel { FirstDeviceViewModel(get()) }
        viewModelOf(::EnrollmentStartViewModel)
        viewModelOf(::EnrollmentCodeViewModel)
        viewModelOf(::EnrollmentScanViewModel)
        viewModelOf(::SafetyCodeViewModel)
        viewModelOf(::EnrollmentCompleteViewModel)
    }
