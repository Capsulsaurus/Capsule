import CapsuleUI
import SwiftUI

// MARK: - PasskeyEnrollView

/// Enrolling a passkey as a sign-in factor.
///
/// The claim this screen makes is deliberately narrow: a passkey replaces the
/// **password** in the login ceremony. It is not a second copy of the master key
/// and it cannot recover an account — that is the recovery secret's job.
/// Conflating the two would leave a user believing a lost phone is survivable
/// when it is not, so the scope note is part of the screen rather than a help
/// article.
///
/// A device with no authenticator is told so. Offering a button that fails when
/// tapped teaches the user that the app is broken; saying "this device has
/// nowhere to keep a passkey" tells them what is actually true.
///
/// Entry point: ``init(secondFactor:defaultDisplayName:)``, needing
/// ``SecondFactorPort``.
public struct PasskeyEnrollView: View {
    @State private var model: PasskeyEnrollViewModel

    public init(secondFactor: any SecondFactorPort, defaultDisplayName: String = "") {
        _model = State(wrappedValue: PasskeyEnrollViewModel(
            secondFactor: secondFactor,
            defaultDisplayName: defaultDisplayName
        ))
    }

    public var body: some View {
        CeremonyContainer {
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.large) {
                CeremonyHeader(
                    titleKey: "app.passkey.title",
                    subtitleKey: "app.passkey.subtitle",
                    symbolName: "person.badge.key.fill"
                )
                content
                scopeNote
            }
        }
        .task { await model.load() }
    }

    @ViewBuilder
    private var content: some View {
        switch model.state {
        case .idle:
            AuthLoadingView(labelKey: "app.passkey.loading")
        case .loading:
            AuthLoadingView(labelKey: model.isEnrolling ? "app.passkey.enrolling" : "app.passkey.loading")
        case let .failed(error):
            AuthErrorBanner(error: error) { Task { await model.enroll() } }
        case .empty:
            ContentUnavailableView(
                "app.passkey.unavailable.title",
                systemImage: "key.slash",
                description: Text("app.passkey.unavailable.description")
            )
        case .ready:
            enrollment
        }
    }

    @ViewBuilder
    private var enrollment: some View {
        if let registration = model.registration {
            registered(registration)
        } else {
            form
        }
    }

    private var form: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
            LabeledField(
                labelKey: "app.passkey.name.label",
                footerKey: "app.passkey.name.footer"
            ) {
                TextField("app.passkey.name.label", text: $model.displayNameInput)
                    .autocorrectionDisabled()
                    .accessibilityLabel("app.passkey.name.label")
            }
            // The ceremony itself belongs to the OS: the system sheet collects
            // whatever it collects, and this returns only once the credential is
            // registered server-side, so a half-finished ceremony can never look
            // like an enrolled factor.
            Button("app.passkey.enroll") { Task { await model.enroll() } }
                .capsuleGlassButtonStyle(prominent: true)
                .disabled(!model.canEnroll)
                .accessibilityLabel("app.passkey.enroll")
        }
    }

    private func registered(_ registration: PasskeyRegistration) -> some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
            AuthSectionHeader(
                titleKey: "app.passkey.done.title",
                descriptionKey: "app.passkey.done.description",
                symbolName: "checkmark.seal.fill"
            )
            AuthLabeledValue(
                labelKey: "app.passkey.done.authenticator",
                value: registration.authenticatorLabel
            )
            AuthLabeledDate(labelKey: "app.passkey.done.created", date: registration.createdAt.date)
        }
        .authCard()
    }

    private var scopeNote: some View {
        Text("app.passkey.scope_note")
            .font(.footnote)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
    }
}

// MARK: - Previews

#Preview("Passkey enrolment") {
    PasskeyEnrollView(secondFactor: AuthPreviewEnvironment.healthy.secondFactor)
}

#Preview("Passkey enrolment — dark") {
    PasskeyEnrollView(secondFactor: AuthPreviewEnvironment.healthy.secondFactor)
        .preferredColorScheme(.dark)
}

#Preview("Passkey enrolment — no authenticator") {
    let world = AuthPreviewEnvironment.healthy
    return PasskeyEnrollView(
        secondFactor: PreviewSecondFactor(environment: world.environment, passkeysAvailable: false)
    )
}
