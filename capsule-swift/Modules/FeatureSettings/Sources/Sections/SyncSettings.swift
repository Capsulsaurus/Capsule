import CapsuleDomain
import CapsuleMock
import CapsuleNavigation
import CapsulePorts
import Observation
import SwiftUI

// MARK: - SyncSettingsModel

/// Drives the Sync screen: what is fetched eagerly, under which network
/// conditions, and what to do when the library has fallen behind.
///
/// The two-week staleness rule is a **product surface, not a bug report**: a
/// mobile OS may grant no background window for days, so "after two weeks
/// without a completed sync while changes remain un-synced … the user is
/// notified that the library is behind and offered a one-tap force sync now,
/// which proceeds regardless of the metered/Wi-Fi criteria with their explicit
/// consent". Both halves matter — a library with nothing pending is not stale,
/// however long it has been.
@MainActor
@Observable
public final class SyncSettingsModel {
    public private(set) var phase: SettingsPhase = .loading
    public private(set) var status: SyncStatus?
    public private(set) var scope: SyncScope = .metadataAndThumbnails
    public private(set) var uploadPolicy: UploadPolicy = .full
    public private(set) var autoSyncEnabled = true
    public private(set) var isWorking = false

    private let sync: any SyncPort
    private let uploads: any UploadPort
    private let settings: any SettingsPort
    private let connectivity: SettingsConnectivity
    private let clock: SettingsClock

    public init(
        sync: any SyncPort,
        uploads: any UploadPort,
        settings: any SettingsPort,
        connectivity: SettingsConnectivity,
        clock: SettingsClock = .system
    ) {
        self.sync = sync
        self.uploads = uploads
        self.settings = settings
        self.connectivity = connectivity
        self.clock = clock
    }

    public func load() async {
        phase = .loading
        do {
            status = try await sync.status()
            scope = try await sync.syncScope()
            uploadPolicy = try await uploads.uploadPolicy()
            autoSyncEnabled = try await settings.settings().autoSyncEnabled
            phase = .ready
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }

    /// The staleness threshold, in days, as the doc fixes it.
    public var stalenessThresholdDays: Int { SyncStatus.stalenessThresholdDays }

    /// Whether the library is behind by the threshold **and** has pending work.
    public var isStale: Bool {
        status?.isStale(at: clock.now()) == true
    }

    /// Whether a large reconciliation would run right now without a force.
    public var canRunLargeReconciliation: Bool {
        status?.canRunLargeReconciliation == true
    }

    public var scopeOptions: [SyncScope] { SyncScope.knownCases }
    public var policyOptions: [UploadPolicy] { UploadPolicy.knownCases }

    public func setScope(_ newScope: SyncScope) async {
        await perform {
            try await self.sync.setSyncScope(newScope)
            self.scope = try await self.sync.syncScope()
        }
    }

    public func setUploadPolicy(_ policy: UploadPolicy) async {
        await perform {
            try await self.uploads.setUploadPolicy(policy)
            self.uploadPolicy = try await self.uploads.uploadPolicy()
        }
    }

    public func setAutoSyncEnabled(_ enabled: Bool) async {
        await perform {
            var current = try await self.settings.settings()
            current.autoSyncEnabled = enabled
            try await self.settings.update(current)
            self.autoSyncEnabled = enabled
        }
    }

    /// Reconcile now, subject to the connection criteria.
    public func synchronize() async {
        await perform {
            try await self.sync.synchronize()
            self.status = try await self.sync.status()
        }
    }

    /// Reconcile **regardless** of the metered and Wi-Fi criteria.
    ///
    /// Never automatic: it spends the user's data on their explicit say-so, so
    /// it may only ever be something they chose, which is why the caller
    /// confirms first.
    public func forceSynchronize() async {
        await perform {
            try await self.sync.forceSynchronize()
            self.status = try await self.sync.status()
        }
    }

    /// Suppress the staleness **warning** only. Auto sync is unaffected — a
    /// user who dismissed a notice has not asked to stop syncing.
    public func snoozeStalenessWarning(days: Int) async {
        let until = CapsuleTimestamp(epochSeconds: clock.now().epochSeconds + Int64(days) * 86400)
        await perform {
            try await self.sync.snoozeStalenessNotification(until: until)
            self.status = try await self.sync.status()
        }
    }

    private func perform(_ work: @escaping () async throws -> Void) async {
        isWorking = true
        defer { isWorking = false }
        do {
            try await work()
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }
}

// MARK: - Presentation

public extension SyncScope {
    var titleKey: String {
        switch self {
        case .metadataOnly: "app.settings.sync.scope.metadata_only"
        case .metadataAndThumbnails: "app.settings.sync.scope.metadata_thumbnails"
        case .metadataThumbnailsAndOriginals: "app.settings.sync.scope.metadata_thumbnails_originals"
        case .unknown: "app.settings.sync.scope.unknown"
        }
    }
}

public extension UploadPolicy {
    var titleKey: String {
        switch self {
        case .full: "app.settings.sync.policy.full"
        case .staged: "app.settings.sync.policy.staged"
        case .unknown: "app.settings.sync.policy.unknown"
        }
    }
}

// MARK: - SyncSettingsView

/// Sync — scope, cadence, and the staleness escape hatch.
public struct SyncSettingsView: View {
    @State private var model: SyncSettingsModel
    @State private var isForcePresented = false

    public init(model: SyncSettingsModel) {
        _model = State(initialValue: model)
    }

    public init(environment: SettingsEnvironment) {
        self.init(
            model: SyncSettingsModel(
                sync: environment.sync,
                uploads: environment.uploads,
                settings: environment.settings,
                connectivity: environment.connectivity
            )
        )
    }

    public var body: some View {
        SettingsScreen(
            titleKey: SettingsSection.sync.titleKey,
            phase: model.phase,
            retry: { await model.load() },
            content: {
                if model.isStale {
                    stalenessSection
                }
                statusSection
                scopeSection
                policySection
            }
        )
        .task { await model.load() }
        .settingsDestructiveConfirmation(
            titleKey: "app.settings.sync.force.confirm.title",
            messageKey: "app.settings.sync.force.confirm.message",
            confirmKey: "app.settings.sync.force.confirm.action",
            isPresented: $isForcePresented
        ) {
            await model.forceSynchronize()
        }
    }

    private var stalenessSection: some View {
        Section {
            SettingsValueRow(
                labelKey: "app.settings.sync.stale.threshold",
                value: SettingsFormat.days(model.stalenessThresholdDays)
            )
            Button("app.settings.sync.force.action") { isForcePresented = true }
            Button("app.settings.sync.stale.snooze") {
                Task { await model.snoozeStalenessWarning(days: 7) }
            }
        } header: {
            Text("app.settings.sync.stale.header")
        } footer: {
            Text("app.settings.sync.stale.footer")
        }
    }

    private var statusSection: some View {
        Section {
            SettingsStatusRow(
                labelKey: "app.settings.sync.connection",
                statusKey: ConnectionClassPresentation.titleKey(
                    model.status?.connectionClass ?? .unknown("")
                ),
                tone: (model.status?.connectionClass ?? .unknown("")).tone
            )
            SettingsValueRow(
                labelKey: "app.settings.sync.pending_uploads",
                value: SettingsFormat.count(model.status?.pendingUploadCount ?? 0)
            )
            SettingsValueRow(
                labelKey: "app.settings.sync.pending_downloads",
                value: SettingsFormat.count(model.status?.pendingDownloadCount ?? 0)
            )
            SettingsValueRow(
                labelKey: "app.settings.sync.last_completed",
                value: SettingsFormat.timestamp(model.status?.lastCompletedSyncAt)
            )
            SettingsStatusRow(
                labelKey: "app.settings.sync.bulk",
                statusKey: model.canRunLargeReconciliation
                    ? "app.settings.sync.bulk.allowed"
                    : "app.settings.sync.bulk.deferred",
                tone: model.canRunLargeReconciliation ? .positive : .caution
            )
            Button("app.settings.sync.now") { Task { await model.synchronize() } }
                .disabled(model.isWorking)
        } header: {
            Text("app.settings.sync.status.header")
        } footer: {
            Text("app.settings.sync.status.footer")
        }
    }

    private var scopeSection: some View {
        Section {
            Picker("app.settings.sync.scope.label", selection: scopeBinding) {
                ForEach(model.scopeOptions, id: \.rawValue) { option in
                    Text(LocalizedStringKey(option.titleKey)).tag(option)
                }
            }
            .pickerStyle(.inline)
            Toggle("app.settings.sync.auto.toggle", isOn: autoSyncBinding)
        } header: {
            Text("app.settings.sync.scope.header")
        } footer: {
            Text("app.settings.sync.scope.footer")
        }
    }

    private var policySection: some View {
        Section {
            Picker("app.settings.sync.policy.label", selection: policyBinding) {
                ForEach(model.policyOptions, id: \.rawValue) { option in
                    Text(LocalizedStringKey(option.titleKey)).tag(option)
                }
            }
            .pickerStyle(.inline)
        } header: {
            Text("app.settings.sync.policy.header")
        } footer: {
            Text("app.settings.sync.policy.footer")
        }
    }

    private var scopeBinding: Binding<SyncScope> {
        Binding(
            get: { model.scope },
            set: { newValue in Task { await model.setScope(newValue) } }
        )
    }

    private var policyBinding: Binding<UploadPolicy> {
        Binding(
            get: { model.uploadPolicy },
            set: { newValue in Task { await model.setUploadPolicy(newValue) } }
        )
    }

    private var autoSyncBinding: Binding<Bool> {
        Binding(
            get: { model.autoSyncEnabled },
            set: { newValue in Task { await model.setAutoSyncEnabled(newValue) } }
        )
    }
}

// MARK: - Preview

#Preview("Sync") {
    NavigationStack {
        SyncSettingsView(environment: .preview())
    }
}

#Preview("Sync — Offline") {
    NavigationStack {
        SyncSettingsView(environment: .preview(.offline))
    }
}
