import CapsuleMock
import CapsulePorts
import CapsuleUI
import SwiftUI

// MARK: - WelcomeView

/// The first-run fork.
///
/// **The local path is a first-class choice, not a skip link.** *Local Gallery —
/// FR2* is a product commitment: a user who never connects a server still gets
/// import, organise, search, and export, in full. So the two options are the
/// same size, the same weight, and the same shape of card; neither is a
/// secondary button under the other, and the local one is listed first because
/// it is the one that asks nothing of the user.
///
/// The entry point the route table calls is ``init(auth:)``; the port it needs
/// is ``AuthPort``, used only to notice a session that already exists.
public struct WelcomeView: View {
    @State private var model: WelcomeViewModel

    public init(auth: any AuthPort) {
        _model = State(wrappedValue: WelcomeViewModel(auth: auth))
    }

    public var body: some View {
        CeremonyContainer {
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xLarge) {
                CeremonyHeader(
                    titleKey: "ios.auth.welcome.title",
                    subtitleKey: "ios.auth.welcome.subtitle",
                    symbolName: "photo.stack"
                )
                switch model.state {
                case .idle, .loading:
                    AuthLoadingView()
                case let .failed(error):
                    AuthErrorBanner(error: error) {
                        Task { await model.load() }
                    }
                case .ready, .empty:
                    choices
                }
            }
        }
        .task { await model.load() }
    }

    @ViewBuilder
    private var choices: some View {
        if let account = model.existingAccount {
            existingSession(account.handle)
        }
        VStack(spacing: CapsuleTheme.Spacing.large) {
            choiceCard(
                titleKey: "ios.auth.welcome.local.title",
                descriptionKey: "ios.auth.welcome.local.description",
                actionKey: "ios.auth.welcome.local.action",
                symbolName: "iphone.gen3",
                choice: .useWithoutAccount
            )
            choiceCard(
                titleKey: "ios.auth.welcome.server.title",
                descriptionKey: "ios.auth.welcome.server.description",
                actionKey: "ios.auth.welcome.server.action",
                symbolName: "server.rack",
                choice: .connectServer
            )
        }
        Text("ios.auth.welcome.later_note")
            .font(.footnote)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
    }

    private func existingSession(_ handle: String) -> some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            Text("ios.auth.welcome.signed_in_as")
                .font(.footnote)
                .foregroundStyle(.secondary)
            Text(verbatim: handle)
                .font(.headline)
            if model.sessionHasExpired {
                Text("ios.auth.welcome.session_expired")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(CapsuleTheme.Spacing.medium)
        .background(.quaternary, in: RoundedRectangle(cornerRadius: CapsuleTheme.Radius.medium))
        .accessibilityElement(children: .combine)
    }

    /// One of the two paths. Identical construction for both, so neither can
    /// drift into looking like the "real" one.
    private func choiceCard(
        titleKey: LocalizedStringKey,
        descriptionKey: LocalizedStringKey,
        actionKey: LocalizedStringKey,
        symbolName: String,
        choice: WelcomeChoice
    ) -> some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            Label {
                Text(titleKey).font(.title3.weight(.semibold))
            } icon: {
                Image(systemName: symbolName).font(.title3)
            }
            Text(descriptionKey)
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Button(actionKey) { model.choose(choice) }
                .capsuleGlassButtonStyle(prominent: model.choice == choice)
                .accessibilityLabel(actionKey)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(CapsuleTheme.Spacing.large)
        .background(.quaternary, in: RoundedRectangle(cornerRadius: CapsuleTheme.Radius.large))
    }
}

// MARK: - Previews

#Preview("Welcome — never signed in") {
    WelcomeView(auth: AuthPreviewEnvironment.neverSignedIn.auth)
}

#Preview("Welcome — never signed in, dark") {
    WelcomeView(auth: AuthPreviewEnvironment.neverSignedIn.auth)
        .preferredColorScheme(.dark)
}

#Preview("Welcome — session already on this device") {
    WelcomeView(auth: AuthPreviewEnvironment.healthy.auth)
}
