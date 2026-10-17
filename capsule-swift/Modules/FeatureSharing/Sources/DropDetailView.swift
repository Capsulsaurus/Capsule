import CapsuleDomain
import CapsuleFoundation
import CapsuleMock
import CapsuleUI
import SwiftUI

// MARK: - DropDetailView

/// Adopt a drop into an album, or discard it (*Web Upload*).
public struct DropDetailView: View {
    @State private var model: DropDetailViewModel
    private let onFinish: () -> Void

    public init(model: DropDetailViewModel, onFinish: @escaping () -> Void = {}) {
        _model = State(wrappedValue: model)
        self.onFinish = onFinish
    }

    public var body: some View {
        Form {
            previewSection
            claimsSection
            if model.outcome == nil {
                destinationSection
                actionsSection
            } else {
                outcomeSection
            }
        }
        .formStyle(.grouped)
        .frame(maxWidth: 640)
        .frame(maxWidth: .infinity)
        .navigationTitle("app.drops.detail.title")
        .task { await model.load() }
        .safeAreaInset(edge: .top) { OfflineNotice(connection: model.connection) }
        .confirmationDialog(
            "app.drops.detail.discard.confirm_title",
            isPresented: $model.isConfirmingDiscard,
            titleVisibility: .visible
        ) {
            Button("app.drops.detail.discard.confirm", role: .destructive) {
                Task {
                    await model.discard()
                    onFinish()
                }
            }
            Button("app.common.cancel", role: .cancel) { model.cancelDiscard() }
        } message: {
            Text("app.drops.detail.discard.confirm_message")
        }
    }

    // MARK: Sections

    /// The preview, and what it means when there is not one.
    ///
    /// The bytes are external-origin, so they are decoded only in the sandboxed
    /// decoder. A file the sandbox will not open is a normal outcome — and the
    /// screen keeps working, because discarding something you could not look at
    /// is a legitimate decision.
    private var previewSection: some View {
        Section {
            switch model.preview {
            case .pending, .decoding:
                ProgressView()
                    .frame(maxWidth: .infinity)
                    .accessibilityLabel("app.drops.detail.preview.decoding")
            case .unavailable:
                ContentUnavailableView(
                    "app.drops.detail.preview.unavailable.title",
                    systemImage: "eye.slash",
                    description: Text("app.drops.detail.preview.unavailable.description")
                )
            }
        } header: {
            Text("app.drops.detail.preview.header")
        } footer: {
            Text("app.drops.detail.preview.footer")
        }
    }

    /// Everything the guest asserted, under one heading that says so.
    ///
    /// The descriptor is not an asset manifest: no signatures, no album, no
    /// provenance link. Grouping the fields is what stops a size or a content
    /// type reading as verified simply because it is rendered in a neat row.
    private var claimsSection: some View {
        Section {
            UnverifiedClaimView(claim: model.claimedFilename)
            LabeledContent {
                Text(verbatim: model.drop.descriptor.contentType.rawValue)
                    .monospaced()
            } label: {
                Text("app.drops.detail.claim.content_type")
            }
            LabeledContent {
                Text(Int64(model.drop.descriptor.plaintextSize), format: .byteCount(style: .file))
            } label: {
                Text("app.drops.detail.claim.size")
            }
            LabeledContent {
                Text(model.drop.receivedAt.date, format: .dateTime.year().month().day().hour().minute())
            } label: {
                Text("app.drops.detail.received")
            }
        } header: {
            Text("app.drops.detail.claims.header")
        } footer: {
            Text("app.drops.detail.claims.footer")
        }
    }

    private var destinationSection: some View {
        Section {
            Picker("app.drops.detail.destination", selection: $model.destination) {
                ForEach(model.albums) { album in
                    albumLabel(album).tag(AlbumID?.some(album.id))
                }
            }
        } header: {
            Text("app.drops.detail.destination.header")
        } footer: {
            Text("app.drops.detail.destination.footer")
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

    private var actionsSection: some View {
        Section {
            Button("app.drops.detail.adopt") {
                Task {
                    await model.adopt()
                    onFinish()
                }
            }
            .disabled(!model.canAdopt)
            Button("app.drops.detail.discard", role: .destructive) {
                model.requestDiscard()
            }
            .disabled(model.isWorking)
        }
    }

    @ViewBuilder
    private var outcomeSection: some View {
        Section {
            switch model.outcome {
            case .adopted:
                Label("app.drops.detail.outcome.adopted", systemImage: "checkmark.circle")
                    .accessibilityElement(children: .combine)
            case .discarded:
                Label("app.drops.detail.outcome.discarded", systemImage: "trash")
                    .accessibilityElement(children: .combine)
            case nil:
                EmptyView()
            }
        }
    }
}

// MARK: - Previews

#Preview("Drop detail — light") {
    let environment = MockEnvironment(scenario: .healthy)
    return NavigationStack {
        DropDetailView(model: DropDetailViewModel(
            drop: PreviewDrops.claimedChrome(seed: environment.configuration.seed),
            drops: environment.drops,
            albums: environment.albums,
            connectivity: SharingConnectivity(sync: environment.sync)
        ))
    }
    .preferredColorScheme(.light)
}

#Preview("Drop detail — dark") {
    let environment = MockEnvironment(scenario: .healthy)
    return NavigationStack {
        DropDetailView(model: DropDetailViewModel(
            drop: PreviewDrops.claimedChrome(seed: environment.configuration.seed),
            drops: environment.drops,
            albums: environment.albums,
            connectivity: SharingConnectivity(sync: environment.sync)
        ))
    }
    .preferredColorScheme(.dark)
}

// MARK: - PreviewDrops

/// A drop whose asserted filename imitates app chrome, so both previews render
/// the case the "unverified" marker exists for.
private enum PreviewDrops {
    static func claimedChrome(seed: UInt64) -> PendingDrop {
        PendingDrop(
            id: MockIdentifiers.dropID(seed: seed, ordinal: 2),
            receivedAt: MockClock.reference.offset(days: -3),
            viaLink: MockIdentifiers.shareID(seed: seed, ordinal: 3),
            descriptor: DropDescriptor(
                contentType: .png,
                plaintextSize: 4600000,
                chunkSize: 1 << 20,
                ciphertextHash: MockIdentifiers.blobHash(seed: seed, ordinal: 9002),
                suggestedFilename: "Settings \u{2014} Capsule.png"
            )
        )
    }
}
