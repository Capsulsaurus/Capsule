import CapsuleDomain
import CapsuleMock
import CapsuleNavigation
import SwiftUI

// MARK: - StorageSettingsView

/// Storage — the server's charge, this device's occupancy, and reclamation.
public struct StorageSettingsView: View {
    @State private var model: StorageSettingsModel
    @State private var isEvictPresented = false

    public init(model: StorageSettingsModel) {
        _model = State(initialValue: model)
    }

    public init(environment: SettingsEnvironment) {
        self.init(
            model: StorageSettingsModel(
                storage: environment.storage,
                quota: environment.quota,
                settings: environment.settings,
                connectivity: environment.connectivity
            )
        )
    }

    public var body: some View {
        SettingsScreen(
            titleKey: SettingsSection.storage.titleKey,
            phase: model.phase,
            retry: { await model.load() },
            content: {
                quotaSection
                occupancySection
                budgetSection
                reclaimSection
            }
        )
        .task { await model.load() }
        .settingsDestructiveConfirmation(
            titleKey: "app.settings.storage.evict.confirm.title",
            messageKey: "app.settings.storage.evict.confirm.message",
            confirmKey: "app.settings.storage.evict.confirm.action",
            isPresented: $isEvictPresented
        ) {
            await model.evictCache(targetBytes: model.reclaimableBytes)
        }
    }

    // MARK: Quota

    private var quotaSection: some View {
        Section {
            SettingsStatusRow(
                labelKey: "app.settings.storage.quota.label",
                statusKey: (model.quota?.state ?? .unknown("")).titleKey,
                tone: (model.quota?.state ?? .unknown("")).tone
            )
            SettingsValueRow(
                labelKey: "app.settings.storage.quota.used",
                value: SettingsFormat.bytes(model.quota?.used)
            )
            SettingsValueRow(
                labelKey: "app.settings.storage.quota.hard_limit",
                value: SettingsFormat.bytes(model.quota?.hardLimit)
            )
            if let fraction = model.quota?.fractionUsed {
                SettingsValueRow(
                    labelKey: "app.settings.storage.quota.fraction",
                    value: SettingsFormat.percent(fraction)
                )
                ProgressView(value: min(1, fraction))
                    .accessibilityLabel(Text("app.settings.storage.quota.fraction"))
            }
        } header: {
            Text("app.settings.storage.quota.header")
        } footer: {
            Text("app.settings.storage.quota.footer")
        }
    }

    // MARK: Occupancy

    private var occupancySection: some View {
        Section {
            ForEach(model.tiers, id: \.rawValue) { tier in
                SettingsValueRow(
                    labelKey: tier.titleKey,
                    value: SettingsFormat.bytes(model.bytes(for: tier))
                )
            }
            SettingsValueRow(
                labelKey: "app.settings.storage.local.trash",
                value: SettingsFormat.bytes(model.breakdown?.trashBytes)
            )
            SettingsValueRow(
                labelKey: "app.settings.storage.local.unreleased",
                value: SettingsFormat.bytes(model.unreleasedOriginalBytes)
            )
            SettingsValueRow(
                labelKey: "app.settings.storage.local.total",
                value: SettingsFormat.bytes(model.breakdown?.totalBytes)
            )
            SettingsValueRow(
                labelKey: "app.settings.storage.local.available",
                value: SettingsFormat.bytes(model.breakdown?.availableDiskBytes)
            )
        } header: {
            Text("app.settings.storage.local.header")
        } footer: {
            Text("app.settings.storage.local.footer")
        }
    }

    // MARK: Budget

    private var budgetSection: some View {
        Section {
            Picker("app.settings.storage.budget.label", selection: budgetBinding) {
                Text("app.settings.storage.budget.unset").tag(UInt64?.none)
                ForEach(StorageSettingsModel.budgetOptions, id: \.self) { option in
                    Text(verbatim: SettingsFormat.bytes(option)).tag(UInt64?.some(option))
                }
            }
            .pickerStyle(.menu)
        } header: {
            Text("app.settings.storage.budget.header")
        } footer: {
            Text("app.settings.storage.budget.footer")
        }
    }

    private var budgetBinding: Binding<UInt64?> {
        Binding(
            get: { model.cacheBudgetBytes },
            set: { newValue in Task { await model.setCacheBudget(newValue) } }
        )
    }

    // MARK: Reclaim

    private var reclaimSection: some View {
        Section {
            SettingsValueRow(
                labelKey: "app.settings.storage.reclaim.available",
                value: SettingsFormat.bytes(model.reclaimableBytes)
            )
            if let reclaimed = model.lastReclaimedBytes {
                SettingsValueRow(
                    labelKey: "app.settings.storage.reclaim.last",
                    value: SettingsFormat.bytes(reclaimed)
                )
            }
            Button("app.settings.storage.evict.action", role: .destructive) {
                isEvictPresented = true
            }
            .disabled(model.isWorking || model.reclaimableBytes == 0)
        } header: {
            Text("app.settings.storage.reclaim.header")
        } footer: {
            Text("app.settings.storage.reclaim.footer")
        }
    }
}

// MARK: - Preview

#Preview("Storage") {
    NavigationStack {
        StorageSettingsView(environment: .preview())
    }
}

#Preview("Storage — Grace Expired") {
    NavigationStack {
        StorageSettingsView(environment: .preview(.quotaGraceExpired))
    }
}
