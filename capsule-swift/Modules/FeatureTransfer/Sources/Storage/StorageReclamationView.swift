import CapsuleDomain
import CapsuleMock
import CapsulePorts
import CapsuleUI
import SwiftUI

// MARK: - StorageReclamationView

/// **Local disk** — deliberately a different screen from ``QuotaStatusView``,
/// and cross-linked to it.
///
/// Quota is what the server charges; this is what the device holds and whether
/// it is safe to stop holding it. "Free up space" **previews what would be
/// evicted**, in the documented `original → preview → thumbnail` order, and
/// never touches pinned representations or device-owned originals that are not
/// yet confirmed durable.
///
/// Route entry point. Ports required: ``StoragePort``, ``SettingsPort`` (the
/// cache budget), ``SyncPort`` (connection class).
public struct StorageReclamationView: View {
    @State private var model: StorageReclamationModel
    @State private var isConfirming = false
    private let onOpenQuota: (@MainActor () -> Void)?

    public init(
        storage: any StoragePort,
        settings: any SettingsPort,
        sync: any SyncPort,
        onOpenQuota: (@MainActor () -> Void)? = nil
    ) {
        _model = State(wrappedValue: StorageReclamationModel(
            storage: storage,
            settings: settings,
            sync: sync
        ))
        self.onOpenQuota = onOpenQuota
    }

    public var body: some View {
        content
            .navigationTitle("ios.storage.title")
            .task { await model.load() }
            .confirmationDialog(
                "ios.storage.free_up.confirm.title",
                isPresented: $isConfirming,
                titleVisibility: .visible
            ) {
                Button("ios.storage.free_up.confirm.action", role: .destructive) {
                    Task { await model.confirmEviction() }
                }
                Button("ios.common.cancel", role: .cancel) { model.discardPlan() }
            } message: {
                Text("ios.storage.free_up.confirm.message")
            }
    }

    @ViewBuilder
    private var content: some View {
        if model.phase.hasContent {
            List {
                budgetSection
                consumersSection
                exemptSection
                freeUpSection
                quotaLink
            }
            .listStyle(.inset)
        } else {
            PhasePlaceholderView(
                phase: model.phase,
                emptyTitle: "ios.storage.empty.title",
                emptyDescription: "ios.storage.empty.description",
                emptySymbol: "internaldrive",
                retry: { await model.reload() }
            )
        }
    }

    // MARK: Sections

    private var budgetSection: some View {
        Section {
            CacheBudgetControl(
                budgetBytes: model.cacheBudgetBytes,
                reclaimableBytes: model.breakdown.reclaimableBytes,
                overBudgetBytes: model.overBudgetBytes,
                hasExplicitBudget: model.hasExplicitBudget
            ) { bytes in
                Task { await model.setCacheBudget(bytes) }
            }
        } header: {
            Text("ios.storage.budget.section")
        } footer: {
            Text("ios.storage.budget.footer")
        }
    }

    private var consumersSection: some View {
        Section {
            ForEach(model.consumers) { consumer in StorageConsumerRow(consumer: consumer) }
            if let available = model.breakdown.availableDiskBytes {
                LabeledContent("ios.storage.available") {
                    Text(verbatim: TransferFormat.bytes(available))
                }
            }
        } header: {
            Text("ios.storage.consumers.section")
        } footer: {
            // Honest about the granularity this screen can offer.
            Text("ios.storage.consumers.footer")
        }
    }

    @ViewBuilder
    private var exemptSection: some View {
        if model.exemptBytes > 0 {
            Section {
                LabeledContent("ios.storage.exempt.unreleased") {
                    Text(verbatim: TransferFormat.bytes(model.exemptBytes))
                }
            } header: {
                Text("ios.storage.exempt.section")
            } footer: {
                Text("ios.storage.exempt.footer")
            }
        }
    }

    private var freeUpSection: some View {
        Section {
            Button("ios.storage.free_up.preview") {
                model.previewEvictionToBudget()
            }
            .disabled(model.isBusy || model.breakdown.reclaimableBytes == 0)
            if let plan = model.pendingPlan {
                EvictionPreviewList(plan: plan)
                planActions(plan)
            }
            if let reclaimed = model.lastReclaimedBytes {
                Label("ios.storage.free_up.done", systemImage: "checkmark.circle")
                    .foregroundStyle(.green)
                LabeledContent("ios.storage.free_up.reclaimed") {
                    Text(verbatim: TransferFormat.bytes(reclaimed))
                }
            }
        } header: {
            Text("ios.storage.free_up.section")
        } footer: {
            Text("ios.storage.free_up.footer")
        }
    }

    @ViewBuilder
    private func planActions(_ plan: EvictionPlan) -> some View {
        if plan.isEmpty {
            Label("ios.storage.plan.nothing", systemImage: "checkmark.circle")
                .foregroundStyle(.secondary)
        } else {
            Button("ios.storage.free_up.apply", role: .destructive) { isConfirming = true }
                .disabled(model.isBusy)
            Button("ios.common.cancel") { model.discardPlan() }
        }
    }

    private var quotaLink: some View {
        Section {
            Button {
                onOpenQuota?()
            } label: {
                Label("ios.storage.link.quota", systemImage: "externaldrive.badge.icloud")
            }
            .disabled(onOpenQuota == nil)
        } footer: {
            Text("ios.storage.link.quota.footer")
        }
    }
}

// MARK: - Previews

#Preview("Staged uploads hold originals") {
    let environment = MockEnvironment(scenario: .awaitingOriginals)
    return NavigationStack {
        StorageReclamationView(
            storage: environment.storage,
            settings: environment.settings,
            sync: environment.sync,
            onOpenQuota: {}
        )
    }
}

#Preview("Empty library") {
    let environment = MockEnvironment(scenario: .emptyLibrary)
    return NavigationStack {
        StorageReclamationView(
            storage: environment.storage,
            settings: environment.settings,
            sync: environment.sync
        )
    }
    .preferredColorScheme(.dark)
}
