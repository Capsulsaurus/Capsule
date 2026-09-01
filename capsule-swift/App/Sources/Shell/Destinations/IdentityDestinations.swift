import CapsuleNavigation
import FeatureAuth
import FeatureTransfer
import SwiftUI

extension RouteDestination {
    /// The enrolled-device directory and the session ledger.
    var devicesDestination: some View {
        DevicesAndSessionsView(devices: environment.devices)
    }

    /// One step of first-run setup.
    ///
    /// Each step is a screen that also has to work *after* onboarding — the
    /// recovery phrase is re-shown from Settings, the enrollment ceremony reruns
    /// on a new device — which is why the steps are routes rather than pages of a
    /// private flow, and why nothing here holds the flow's position.
    @ViewBuilder
    func onboardingDestination(_ step: OnboardingStep) -> some View {
        switch step {
        case .welcome:
            WelcomeView(auth: environment.auth)
        case .server:
            ServerConnectView(discovery: environment.serverDiscovery)
        case .signIn:
            signInDestination
        case .recovery:
            RecoveryPassphraseView(recovery: environment.recovery)
        case .enrollment:
            EnrollmentCeremonyView(enrollment: environment.firstDeviceEnrollment)
        case .backupScope:
            SyncScopeSettingsView(
                sync: environment.sync,
                uploads: environment.uploads,
                settings: environment.settings
            )
        // Photo access is a system prompt with no screen of its own yet, and the
        // hand-off into the library has none either.
        case .photoAccess, .finish:
            RouteScaffold(titleKey: step.titleKey, systemImage: "person.badge.key")
        }
    }

    /// Sign-in, once a server is pinned.
    ///
    /// Which credentials are even offered is the *server's* answer, not the
    /// client's, so the pinned server has to be read before the chooser can be
    /// built. With nothing pinned there is nothing to sign in to, and the screen
    /// says so rather than offering a chooser with no methods on it.
    @ViewBuilder
    private var signInDestination: some View {
        let discovery = environment.serverDiscovery
        ResolvedDestination(
            titleKey: OnboardingStep.signIn.titleKey,
            systemImage: "person.badge.key",
            resolve: {
                // `try?` on a throwing call that already returns an optional
                // nests one, and here a lookup that failed and a lookup that
                // found nothing mean the same thing.
                let pinned = try? await discovery.pinnedServer()
                return pinned.flatMap { $0 }
            },
            content: { server in
                AuthPathChooserView(server: server)
            }
        )
    }
}
