import CapsuleDomain
import CapsuleFoundation
import CapsuleMock
import CapsulePorts
import CapsuleUI
import SwiftUI

// MARK: - UploadDetailView

/// One asset's transfer: per-tier progress, session state, the authoritative
/// resumption point, the adaptive chunk plan, and any failure with its
/// documented recovery as the button label.
///
/// Route entry point. Ports required: ``UploadPort``, ``SyncPort`` (connection
/// class, which is what gates each tier), ``StoragePort`` (the custody link).
public struct UploadDetailView: View {
    @State private var model: UploadDetailModel
    private let uploads: any UploadPort
    private let storage: any StoragePort
    private let clock: TransferClock

    public init(
        assetID: AssetID,
        uploads: any UploadPort,
        sync: any SyncPort,
        storage: any StoragePort,
        clock: TransferClock = .system
    ) {
        _model = State(wrappedValue: UploadDetailModel(
            assetID: assetID,
            uploads: uploads,
            sync: sync,
            clock: clock
        ))
        self.uploads = uploads
        self.storage = storage
        self.clock = clock
    }

    public var body: some View {
        content
            .navigationTitle("ios.transfer.detail.title")
            .task { await model.load() }
    }

    @ViewBuilder
    private var content: some View {
        if model.phase.hasContent {
            List {
                if !model.phase.permitsNetworkActions { offlineNotice }
                ladderSection
                if model.requiresProtocolUpgrade { upgradeSection }
                if !model.failures.isEmpty { failureSection }
                ForEach(model.sessions) { session in sessionSection(session) }
                custodySection
            }
            .listStyle(.inset)
        } else {
            PhasePlaceholderView(
                phase: model.phase,
                emptyTitle: "ios.transfer.detail.empty.title",
                emptyDescription: "ios.transfer.detail.empty.description",
                emptySymbol: "checkmark.circle",
                retry: { await model.reload() }
            )
        }
    }

    // MARK: Sections

    private var offlineNotice: some View {
        Section {
            Label("ios.transfer.state.offline.inline", systemImage: "wifi.slash")
                .foregroundStyle(.secondary)
        }
    }

    private var ladderSection: some View {
        Section("ios.transfer.detail.ladder") {
            ForEach(model.tierProgress) { progress in
                TierLegendRow(progress: progress, isGated: model.isGated(progress.tier))
            }
        }
    }

    /// The `426` hard stop gets a link, not a button: there is no in-app
    /// remedy and no downgrade to offer.
    private var upgradeSection: some View {
        Section {
            NavigationLink {
                ProtocolUpgradeRequiredView()
            } label: {
                Label("ios.transfer.upgrade.link", systemImage: "exclamationmark.octagon.fill")
                    .foregroundStyle(.red)
            }
        }
    }

    private var failureSection: some View {
        Section("ios.transfer.detail.failures") {
            ForEach(model.failures) { failure in
                UploadFailureRow(failure: failure) { await model.recover(failure) }
            }
        }
    }

    private func sessionSection(_ session: UploadSession) -> some View {
        Section {
            SessionStateTrack(state: session.state)
            ProgressView(value: session.fractionComplete)
                .accessibilityLabel("ios.transfer.detail.progress")
                .accessibilityValue(Text(verbatim: TransferFormat.percent(session.fractionComplete)))
            LabeledContent("ios.transfer.detail.resume_at") {
                Text(verbatim: TransferFormat.bytes(model.resumptionPoint(for: session)))
            }
            LabeledContent("ios.transfer.detail.declared_size") {
                Text(verbatim: TransferFormat.bytes(session.declaredSize))
            }
            LabeledContent("ios.transfer.detail.throughput") {
                throughput(for: session)
            }
            AdaptiveChunkDisclosure(plan: model.chunkPlan(for: session))
            cancelButton(for: session)
        } header: {
            Label(LocalizedStringKey(session.tier.badge.titleKey), systemImage: session.tier.badge.systemImage)
        } footer: {
            Text(session.tier.explanationKey)
        }
    }

    @ViewBuilder
    private func throughput(for session: UploadSession) -> some View {
        if let rate = model.rate(for: session.id) {
            Text(verbatim: TransferFormat.rate(bytesPerSecond: rate))
        } else {
            Text("ios.transfer.row.throughput_measuring")
        }
    }

    /// Cancellation disappears once finalization has begun — it is not
    /// interruptible, and a button that would always be refused is a lie.
    @ViewBuilder
    private func cancelButton(for session: UploadSession) -> some View {
        if session.state.isCancellable {
            Button("ios.transfer.action.cancel", role: .destructive) {
                Task { await model.cancel(session.id) }
            }
            .disabled(!model.phase.permitsNetworkActions)
        }
    }

    private var custodySection: some View {
        Section {
            NavigationLink {
                CustodyReceiptView(
                    assetID: model.assetID,
                    uploads: uploads,
                    storage: storage,
                    clock: clock
                )
            } label: {
                Label("ios.custody.link", systemImage: "signature")
            }
        } footer: {
            Text("ios.custody.link.footer")
        }
    }
}

// MARK: - Previews

#Preview("Staged, original still local") {
    let environment = MockEnvironment(scenario: .awaitingOriginals)
    let assetID = AssetID.managed(uuid: "preview")
    return NavigationStack {
        UploadDetailView(
            assetID: assetID,
            uploads: environment.uploads,
            sync: environment.sync,
            storage: environment.storage,
            clock: .fixed(environment.configuration.clock.now)
        )
    }
}

#Preview("Offline") {
    let environment = MockEnvironment(scenario: .offline)
    return NavigationStack {
        UploadDetailView(
            assetID: .managed(uuid: "preview"),
            uploads: environment.uploads,
            sync: environment.sync,
            storage: environment.storage,
            clock: .fixed(environment.configuration.clock.now)
        )
    }
    .preferredColorScheme(.dark)
}
