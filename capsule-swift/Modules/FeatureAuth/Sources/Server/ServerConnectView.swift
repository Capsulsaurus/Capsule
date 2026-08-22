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
                    titleKey: "ios.auth.server.title",
                    subtitleKey: "ios.auth.server.subtitle",
                    symbolName: "server.rack"
                )
                domainField
                if case let .failed(error) = model.state {
                    AuthErrorBanner(error: error) {
                        Task { await model.discover() }
                    }
                }
                if model.state.isLoading {
                    AuthLoadingView(labelKey: "ios.auth.server.discovering")
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
                        "ios.auth.server.empty.title",
                        systemImage: "network.slash",
                        description: Text("ios.auth.server.empty.description")
                    )
                }
            }
        }
        .task { await model.loadPinnedServer() }
    }

    private var domainField: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            Text("ios.auth.server.domain.label")
                .font(.headline)
            TextField("ios.auth.server.domain.prompt", text: $model.domainInput)
                .textFieldStyle(.roundedBorder)
                .textContentType(.URL)
                .autocorrectionDisabled()
                .accessibilityLabel("ios.auth.server.domain.label")
                .onSubmit { Task { await model.discover() } }
            Text("ios.auth.server.domain.footer")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Button("ios.auth.server.discover") {
                Task { await model.discover() }
            }
            .capsuleGlassButtonStyle(prominent: true)
            .disabled(!model.canDiscover || model.state.isLoading)
            .accessibilityLabel("ios.auth.server.discover")
        }
    }

    @ViewBuilder
    private var pinControls: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            if model.isPinned {
                StatusChip(
                    titleKey: "ios.auth.server.pinned",
                    symbolName: "checkmark.seal.fill",
                    tint: .green
                )
            }
            Button("ios.auth.server.pin") {
                Task { await model.pin() }
            }
            .capsuleGlassButtonStyle(prominent: true)
            .disabled(model.isPinned || !model.isProtocolCompatible)
            .accessibilityLabel("ios.auth.server.pin")

            Button("ios.auth.server.change") { model.reset() }
                .buttonStyle(.borderless)
                .accessibilityLabel("ios.auth.server.change")
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
