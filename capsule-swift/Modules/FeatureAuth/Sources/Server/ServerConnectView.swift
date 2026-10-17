import CapsuleUI
import SwiftUI

// MARK: - ServerConnectView

/// Domain entry, `.well-known/capsule/server-info` discovery, and the pinning
/// decision.
///
/// The screen's job after a successful lookup is to make the server's **signing
/// key** comparable by a human: it is chunked in exactly the format every other
/// comparable code in the app uses, selectable so it can be pasted next to
/// whatever the administrator published, and accompanied by the sentence that
/// explains what pinning it means.
///
/// Entry point: ``init(discovery:clientProtocolVersion:)``, needing
/// ``ServerDiscoveryPort``.
public struct ServerConnectView: View {
    @State private var model: ServerConnectViewModel

    public init(discovery: any ServerDiscoveryPort, clientProtocolVersion: Int = 1) {
        _model = State(wrappedValue: ServerConnectViewModel(
            discovery: discovery,
            clientProtocolVersion: clientProtocolVersion
        ))
    }

    public var body: some View {
        CeremonyContainer {
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xLarge) {
                CeremonyHeader(
                    titleKey: "app.auth.server.title",
                    subtitleKey: "app.auth.server.subtitle",
                    symbolName: "server.rack"
                )
                domainField
                if case let .failed(error) = model.state {
                    AuthErrorBanner(error: error) {
                        Task { await model.discover() }
                    }
                }
                if model.state.isLoading {
                    AuthLoadingView(labelKey: "app.auth.server.discovering")
                }
                if let server = model.server {
                    ServerIdentityCard(
                        server: server,
                        signingKeyDisplay: model.signingKeyDisplay,
                        isCompatible: model.isProtocolCompatible
                    )
                    pinControls
                } else if model.state == .empty {
                    ContentUnavailableView(
                        "app.auth.server.empty.title",
                        systemImage: "network.slash",
                        description: Text("app.auth.server.empty.description")
                    )
                }
            }
        }
        .task { await model.loadPinnedServer() }
    }

    private var domainField: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            Text("app.auth.server.domain.label")
                .font(.headline)
            TextField("app.auth.server.domain.prompt", text: $model.domainInput)
                .textFieldStyle(.roundedBorder)
                .textContentType(.URL)
                .autocorrectionDisabled()
                .accessibilityLabel("app.auth.server.domain.label")
                .onSubmit { Task { await model.discover() } }
            Text("app.auth.server.domain.footer")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Button("app.auth.server.discover") {
                Task { await model.discover() }
            }
            .capsuleGlassButtonStyle(prominent: true)
            .disabled(!model.canDiscover || model.state.isLoading)
            .accessibilityLabel("app.auth.server.discover")
        }
    }

    @ViewBuilder
    private var pinControls: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            if model.isPinned {
                StatusChip(
                    titleKey: "app.auth.server.pinned",
                    symbolName: "checkmark.seal.fill",
                    tint: .green
                )
            }
            Button("app.auth.server.pin") {
                Task { await model.pin() }
            }
            .capsuleGlassButtonStyle(prominent: true)
            .disabled(model.isPinned || !model.isProtocolCompatible)
            .accessibilityLabel("app.auth.server.pin")

            Button("app.auth.server.change") { model.reset() }
                .buttonStyle(.borderless)
                .accessibilityLabel("app.auth.server.change")
        }
    }
}

// MARK: - Previews

#Preview("Server connect") {
    ServerConnectView(discovery: AuthPreviewEnvironment.neverSignedIn.discovery)
}

#Preview("Server connect — dark") {
    ServerConnectView(discovery: AuthPreviewEnvironment.neverSignedIn.discovery)
        .preferredColorScheme(.dark)
}

#Preview("Server connect — offline") {
    ServerConnectView(discovery: AuthPreviewEnvironment.offline.discovery)
}
