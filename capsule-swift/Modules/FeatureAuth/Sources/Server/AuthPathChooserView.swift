import CapsuleUI
import SwiftUI

// MARK: - AuthPathChooserView

/// The fork between the server's own credential ceremony and an external
/// identity provider.
///
/// Both are first-class: *Authentication — Choosing an Auth Path* recommends
/// local auth for personal and household servers and OIDC for deployments that
/// already run an IdP, and a deployment may enable either or both. The screen
/// shows what *this* server offers and nothing else.
///
/// The footer is the part worth keeping: whichever path the user picks, the
/// credential authenticates the **session**, and the master key never derives
/// from — and is never visible to — whoever checks it. A user choosing SSO
/// should not come away believing their employer can read their photographs.
///
/// Entry point: ``init(server:)``, needing only a discovered ``ServerInfo``.
public struct AuthPathChooserView: View {
    @State private var model: AuthPathChooserViewModel

    public init(server: ServerInfo) {
        _model = State(wrappedValue: AuthPathChooserViewModel(server: server))
    }

    public var body: some View {
        CeremonyContainer {
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xLarge) {
                CeremonyHeader(
                    titleKey: "app.auth.path.title",
                    subtitleKey: "app.auth.path.subtitle",
                    symbolName: "person.badge.key"
                )
                if model.methods.isEmpty {
                    ContentUnavailableView(
                        "app.auth.path.unsupported.title",
                        systemImage: "questionmark.circle",
                        description: Text("app.auth.path.unsupported.description")
                    )
                } else {
                    ForEach(model.methods, id: \.rawValue) { method in
                        methodCard(method)
                    }
                    Text("app.auth.path.footer")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }

    private func methodCard(_ method: ServerAuthMethod) -> some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            Label {
                Text(titleKey(for: method)).font(.title3.weight(.semibold))
            } icon: {
                Image(systemName: symbolName(for: method)).font(.title3)
            }
            Text(descriptionKey(for: method))
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Button(actionKey(for: method)) { model.select(method) }
                .capsuleGlassButtonStyle(prominent: model.selection == method)
                .accessibilityLabel(actionKey(for: method))
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(CapsuleTheme.Spacing.large)
        .background(.quaternary, in: RoundedRectangle(cornerRadius: CapsuleTheme.Radius.large))
    }

    private func titleKey(for method: ServerAuthMethod) -> LocalizedStringKey {
        method == .oidc ? "app.auth.path.oidc.title" : "app.auth.path.local.title"
    }

    private func descriptionKey(for method: ServerAuthMethod) -> LocalizedStringKey {
        method == .oidc ? "app.auth.path.oidc.description" : "app.auth.path.local.description"
    }

    private func actionKey(for method: ServerAuthMethod) -> LocalizedStringKey {
        method == .oidc ? "app.auth.path.oidc.action" : "app.auth.path.local.action"
    }

    private func symbolName(for method: ServerAuthMethod) -> String {
        method == .oidc ? "person.crop.circle.badge.checkmark" : "key.horizontal.fill"
    }
}

// MARK: - Previews

#Preview("Auth path chooser") {
    AuthPathChooserView(server: AuthPreviewEnvironment.neverSignedIn.server)
}

#Preview("Auth path chooser — dark") {
    AuthPathChooserView(server: AuthPreviewEnvironment.neverSignedIn.server)
        .preferredColorScheme(.dark)
}
