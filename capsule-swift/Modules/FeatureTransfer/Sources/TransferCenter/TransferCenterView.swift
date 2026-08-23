import CapsuleDomain
import CapsuleFoundation
import CapsuleMock
import CapsulePorts
import CapsuleUI
import SwiftUI

// MARK: - TransferCenterView

/// The transfer hub: uploads, downloads, and settled activity, under a header
/// ring that renders the staged-upload ladder as three concentric arcs.
///
/// Route entry point. Ports required: ``UploadPort``, ``SyncPort``,
/// ``LibraryPort`` (capture dates and LQIP colours for the rows — the transfer
/// ports carry no display metadata, by design).
///
/// Adaptive layout: the per-asset detail opens in an `.inspector`, which is a
/// trailing column on iPad and Mac and a sheet on iPhone, so the same code is
/// right in a narrow stack and a 1400-point window.
public struct TransferCenterView: View {
    @State private var model: TransferCenterModel
    @State private var inspectedAsset: AssetID?
    private let uploads: any UploadPort
    private let sync: any SyncPort
    private let storage: any StoragePort
    private let clock: TransferClock

    public init(
        uploads: any UploadPort,
        sync: any SyncPort,
        library: any LibraryPort,
        storage: any StoragePort,
        clock: TransferClock = .system
    ) {
        _model = State(wrappedValue: TransferCenterModel(
            uploads: uploads,
            sync: sync,
            library: library,
            clock: clock
        ))
        self.uploads = uploads
        self.sync = sync
        self.storage = storage
        self.clock = clock
    }

    public var body: some View {
        content
            .navigationTitle("ios.transfer.title")
            .toolbar { toolbar }
            .safeAreaInset(edge: .bottom) {
                ConnectionFooter(
                    connection: model.connection,
                    policy: model.policy,
                    aggregateBytesPerSecond: model.aggregateBytesPerSecond
                )
            }
            .task { await model.load() }
            .inspector(isPresented: .constant(inspectedAsset != nil)) { inspector }
    }

    // MARK: Chrome

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        ToolbarItem(placement: .primaryAction) {
            Button("ios.transfer.action.force_now") {
                Task { await model.forceUploads() }
            }
            .disabled(!model.phase.permitsNetworkActions || model.rows.isEmpty)
        }
    }

    @ViewBuilder
    private var inspector: some View {
        if let inspectedAsset {
            UploadDetailView(
                assetID: inspectedAsset,
                uploads: uploads,
                sync: sync,
                storage: storage,
                clock: clock
            )
            .inspectorColumnWidth(min: 320, ideal: 380, max: 520)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("ios.common.done") { self.inspectedAsset = nil }
                }
            }
        }
    }

    // MARK: Content

    @ViewBuilder
    private var content: some View {
        List {
            Section {
                TierRingView(progress: model.tierProgress, isGated: model.isGated)
                    .padding(.vertical, CapsuleTheme.Spacing.small)
            }
            Section {
                Picker(selection: $model.segment) {
                    ForEach(TransferSegment.allCases) { segment in
                        Text(LocalizedStringKey(segment.titleKey)).tag(segment)
                    }
                } label: {
                    Text("ios.transfer.segment.label")
                }
                .pickerStyle(.segmented)
                .labelsHidden()
            }
            segmentSection
        }
        .listStyle(.inset)
        .refreshable { await model.reload() }
        .overlay { placeholder }
    }

    @ViewBuilder
    private var placeholder: some View {
        if !model.phase.hasContent {
            PhasePlaceholderView(
                phase: model.phase,
                emptyTitle: "ios.transfer.empty.title",
                emptyDescription: "ios.transfer.empty.description",
                emptySymbol: "checkmark.circle",
                retry: { await model.reload() }
            )
            .background(.background)
        }
    }

    @ViewBuilder
    private var segmentSection: some View {
        switch model.segment {
        case .uploads:
            uploadsSection
        case .downloads:
            DownloadsSection(status: model.status)
        case .activity:
            activitySection
        }
    }

    @ViewBuilder
    private var uploadsSection: some View {
        if model.rows.isEmpty {
            Section("ios.transfer.segment.uploads") {
                Text("ios.transfer.uploads.empty")
                    .foregroundStyle(.secondary)
            }
        } else {
            Section("ios.transfer.segment.uploads") {
                ForEach(model.rows) { row in
                    Button { inspectedAsset = row.assetID } label: {
                        TransferRowView(row: row)
                    }
                    .buttonStyle(.plain)
                    .swipeActions(edge: .trailing) { cancelAction(for: row) }
                }
            }
        }
    }

    @ViewBuilder
    private func cancelAction(for row: TransferRow) -> some View {
        if let cancellable = row.sessions.first(where: { $0.state.isCancellable }) {
            Button("ios.transfer.action.cancel", role: .destructive) {
                Task { await model.cancel(cancellable.id) }
            }
        }
    }

    @ViewBuilder
    private var activitySection: some View {
        if model.settledRows.isEmpty {
            Section("ios.transfer.segment.activity") {
                Text("ios.transfer.activity.empty")
                    .foregroundStyle(.secondary)
            }
        } else {
            Section("ios.transfer.segment.activity") {
                ForEach(model.settledRows) { row in TransferRowView(row: row) }
            }
        }
    }
}

// MARK: - DownloadsSection

/// What this device is waiting to receive.
///
/// The sync feed reports a **pending count**, not a per-asset download queue
/// (*Download and Synchronization — Discovering What Changed*: a client holds
/// one opaque cursor and never polls assets individually), so this section
/// reports the count and the fetch policy that governs it rather than inventing
/// rows the port cannot supply.
struct DownloadsSection: View {
    let status: SyncStatus

    var body: some View {
        Section("ios.transfer.segment.downloads") {
            LabeledContent("ios.transfer.downloads.pending") {
                Text(verbatim: TransferFormat.count(status.pendingDownloadCount))
            }
            Text("ios.transfer.downloads.ladder")
                .font(.footnote)
                .foregroundStyle(.secondary)
            if status.pendingDownloadCount == 0 {
                Label("ios.transfer.downloads.none", systemImage: "checkmark.circle")
                    .foregroundStyle(.secondary)
            }
        }
    }
}

// MARK: - Previews

#Preview("Staged uploads, metered") {
    let environment = MockEnvironment(scenario: .awaitingOriginals)
    return TransferCenterView(
        uploads: environment.uploads,
        sync: environment.sync,
        library: environment.library,
        storage: environment.storage,
        clock: .fixed(environment.configuration.clock.now)
    )
}

#Preview("Offline") {
    let environment = MockEnvironment(scenario: .offline)
    return TransferCenterView(
        uploads: environment.uploads,
        sync: environment.sync,
        library: environment.library,
        storage: environment.storage,
        clock: .fixed(environment.configuration.clock.now)
    )
    .preferredColorScheme(.dark)
}

#Preview("Nothing in flight") {
    let environment = MockEnvironment(scenario: .emptyLibrary)
    return TransferCenterView(
        uploads: environment.uploads,
        sync: environment.sync,
        library: environment.library,
        storage: environment.storage,
        clock: .fixed(environment.configuration.clock.now)
    )
}
