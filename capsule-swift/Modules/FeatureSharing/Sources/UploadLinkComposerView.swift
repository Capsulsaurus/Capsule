import CapsuleDomain
import CapsuleFoundation
import CapsuleMock
import CapsuleUI
import SwiftUI

// MARK: - UploadLinkComposerView

/// Provision a guest upload link and its caps (*Web Upload*).
public struct UploadLinkComposerView: View {
    @State private var model: UploadLinkComposerViewModel

    public init(model: UploadLinkComposerViewModel) {
        _model = State(wrappedValue: model)
    }

    public var body: some View {
        SharingStateView(
            phase: model.phase,
            empty: .init(
                title: "app.drops.compose.no_albums.title",
                message: "app.drops.compose.no_albums.description",
                symbol: "rectangle.stack.badge.plus"
            ),
            retry: { Task { await model.load() } },
            content: {
                form
            }
        )
        .navigationTitle("app.drops.compose.title")
        .task { await model.load() }
        .onDisappear { model.reset() }
        .safeAreaInset(edge: .top) { OfflineNotice(connection: model.connection) }
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("app.drops.compose.create") {
                    Task { await model.createLink() }
                }
                .disabled(!model.canSubmit)
            }
        }
    }

    @ViewBuilder
    private var form: some View {
        if let url = model.uploadURL {
            issuedForm(url)
        } else {
            Form {
                destinationSection
                LinkCapsSection(draft: $model.draft, issues: model.issues)
                passphraseSection
                scopeSection
            }
            .formStyle(.grouped)
            .frame(maxWidth: 640)
            .frame(maxWidth: .infinity)
        }
    }

    private var destinationSection: some View {
        Section {
            Picker("app.drops.compose.destination", selection: $model.destination) {
                ForEach(model.albums) { album in
                    albumLabel(album).tag(AlbumID?.some(album.id))
                }
            }
        } header: {
            Text("app.drops.compose.destination.header")
        } footer: {
            Text("app.drops.compose.destination.footer")
        }
    }

    @ViewBuilder
    private func albumLabel(_ album: ContainerAlbum) -> some View {
        if let name = album.name {
            Text(name)
        } else {
            Text("app.drops.compose.default_album")
        }
    }

    private var passphraseSection: some View {
        Section {
            Toggle("app.drops.compose.passphrase.toggle", isOn: $model.passphraseEnabled)
            if model.passphraseEnabled {
                SecureField("app.drops.compose.passphrase.field", text: $model.passphrase)
                    .textContentType(.password)
            }
        } header: {
            Text("app.drops.compose.passphrase.header")
        } footer: {
            // Named for what it is: an abuse gate on the owner's quota, not
            // confidentiality. Calling it "encryption" here would be false.
            Text("app.drops.compose.passphrase.footer")
        }
    }

    private var scopeSection: some View {
        Section("app.drops.compose.scope.header") {
            ScopeNote(message: "app.drops.compose.scope.write_only")
            ScopeNote(message: "app.drops.compose.scope.review_required")
            ScopeNote(message: "app.drops.compose.scope.quota")
        }
    }

    /// The one place the upload key is rendered.
    private func issuedForm(_ url: URL) -> some View {
        Form {
            Section {
                Text(verbatim: url.absoluteString)
                    .font(.footnote.monospaced())
                    .textSelection(.enabled)
                    .lineLimit(3)
                    .accessibilityHidden(true)
                SwiftUI.ShareLink(item: url) {
                    Label("app.drops.compose.issued.share", systemImage: "square.and.arrow.up")
                }
                .accessibilityLabel("app.drops.compose.issued.share")
                Button("app.drops.compose.issued.done") { model.reset() }
            } header: {
                Text("app.drops.compose.issued.header")
            } footer: {
                Text("app.drops.compose.issued.footer")
            }
        }
        .formStyle(.grouped)
        .frame(maxWidth: 640)
        .frame(maxWidth: .infinity)
    }
}

// MARK: - Previews

#Preview("Upload link — light") {
    let environment = MockEnvironment(scenario: .healthy)
    return NavigationStack {
        UploadLinkComposerView(model: UploadLinkComposerViewModel(
            drops: environment.drops,
            albums: environment.albums,
            homeServer: "capsule.example",
            connectivity: SharingConnectivity(sync: environment.sync),
            now: { MockClock.reference.now.date }
        ))
    }
    .preferredColorScheme(.light)
}

#Preview("Upload link — dark") {
    let environment = MockEnvironment(scenario: .healthy)
    return NavigationStack {
        UploadLinkComposerView(model: UploadLinkComposerViewModel(
            drops: environment.drops,
            albums: environment.albums,
            homeServer: "capsule.example",
            connectivity: SharingConnectivity(sync: environment.sync),
            now: { MockClock.reference.now.date }
        ))
    }
    .preferredColorScheme(.dark)
}
