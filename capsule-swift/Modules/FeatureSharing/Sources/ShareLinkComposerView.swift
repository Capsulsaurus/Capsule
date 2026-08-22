import CapsuleDomain
import CapsuleFoundation
import CapsuleMock
import CapsuleUI
import SwiftUI

// MARK: - ShareLinkComposerView

/// Create a view-only share link for one asset or one album (*Share Links*).
///
/// The screen is a `Form`, which gives the grouped-inset look on iPhone and the
/// settings-style layout on Mac without a size-class branch, and it stays inside
/// a readable measure at window width rather than stretching.
public struct ShareLinkComposerView: View {
    @State private var model: ShareLinkComposerViewModel

    public init(model: ShareLinkComposerViewModel) {
        _model = State(wrappedValue: model)
    }

    public var body: some View {
        Form {
            if let url = model.shareURL {
                issuedSection(url)
            } else {
                failureNotice
                optionsSection
                privacySection
                scopeSection
            }
        }
        .formStyle(.grouped)
        .frame(maxWidth: 640)
        .frame(maxWidth: .infinity)
        .navigationTitle("ios.share.composer.title")
        .task { await model.load() }
        .onDisappear { model.reset() }
        .safeAreaInset(edge: .top) { OfflineNotice(connection: model.connection) }
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("ios.share.composer.create") {
                    Task { await model.createLink() }
                }
                .disabled(!model.canSubmit)
            }
        }
    }

    // MARK: Sections

    /// A failed or offline create is reported **in place**, above the form, so
    /// the options the user filled in survive the failure and a retry does not
    /// mean re-entering a passphrase.
    @ViewBuilder
    private var failureNotice: some View {
        switch model.phase {
        case let .failed(code):
            Label {
                Text(LocalizedStringKey(code.rawValue))
            } icon: {
                Image(systemName: "exclamationmark.triangle")
            }
            .font(.footnote)
            .accessibilityElement(children: .combine)
        case .offline:
            Label("ios.share.state.offline.description", systemImage: "wifi.slash")
                .font(.footnote)
                .accessibilityElement(children: .combine)
        case .loading, .ready, .empty:
            EmptyView()
        }
    }

    private var optionsSection: some View {
        Section {
            Toggle("ios.share.composer.expiry.toggle", isOn: $model.expiryEnabled)
            if model.expiryEnabled {
                DatePicker(
                    "ios.share.composer.expiry.date",
                    selection: $model.expiryDate,
                    displayedComponents: [.date, .hourAndMinute]
                )
            }
            Toggle("ios.share.composer.passphrase.toggle", isOn: $model.passphraseEnabled)
            if model.passphraseEnabled {
                SecureField("ios.share.composer.passphrase.field", text: $model.passphrase)
                    .textContentType(.password)
            }
        } header: {
            Text("ios.share.composer.options")
        } footer: {
            Text("ios.share.composer.passphrase.footer")
        }
    }

    /// The strip, stated as fact. There is no control here because there is no
    /// per-share opt-out to expose.
    private var privacySection: some View {
        Section {
            PrivacyStripView(policy: model.privacyPolicy, setRetention: nil)
        } header: {
            Text("ios.share.privacy.header")
        } footer: {
            Text("ios.share.privacy.footer")
        }
    }

    /// What v1 links are and are not. Stated rather than implied, so nobody
    /// goes looking for a write switch or a per-recipient view log.
    private var scopeSection: some View {
        Section("ios.share.scope.header") {
            if model.isAlbumWide {
                Label("ios.share.scope.album_wide", systemImage: "exclamationmark.shield")
                    .accessibilityElement(children: .combine)
            }
            ScopeNote(message: "ios.share.scope.read_only")
            ScopeNote(message: "ios.share.scope.no_analytics")
            ScopeNote(message: "ios.share.scope.revocation_delay")
        }
    }

    /// The one place a fragment secret is rendered: the user is copying the
    /// link, which is the entire point of the screen.
    private func issuedSection(_ url: URL) -> some View {
        Section {
            // Hidden from VoiceOver on purpose — the secret must not be read
            // out as a label. The share control below is the accessible path to
            // the same URL.
            Text(verbatim: url.absoluteString)
                .font(.footnote.monospaced())
                .textSelection(.enabled)
                .lineLimit(3)
                .accessibilityHidden(true)
            SwiftUI.ShareLink(item: url) {
                Label("ios.share.issued.share", systemImage: "square.and.arrow.up")
            }
            .accessibilityLabel("ios.share.issued.share")
            Button("ios.share.issued.done") { model.reset() }
        } header: {
            Text("ios.share.issued.header")
        } footer: {
            Text("ios.share.issued.footer")
        }
    }
}

// MARK: - Previews

#Preview("Share composer — light") {
    let environment = MockEnvironment(scenario: .healthy)
    return NavigationStack {
        ShareLinkComposerView(model: ShareLinkComposerViewModel(
            scope: .album(MockIdentifiers.albumID(seed: environment.configuration.seed, ordinal: 1)),
            share: MockEnvironment(scenario: .offline).sharing,
            homeServer: "capsule.example",
            connectivity: SharingConnectivity(sync: MockEnvironment(scenario: .offline).sync)
        ))
    }
    .preferredColorScheme(.light)
}

#Preview("Share composer — dark, offline") {
    NavigationStack {
        ShareLinkComposerView(model: ShareLinkComposerViewModel(
            scope: .asset(AssetID.managed(uuid: "preview-asset")),
            share: MockEnvironment(scenario: .offline).sharing,
            homeServer: "capsule.example",
            connectivity: SharingConnectivity(sync: MockEnvironment(scenario: .offline).sync)
        ))
    }
    .preferredColorScheme(.dark)
}
