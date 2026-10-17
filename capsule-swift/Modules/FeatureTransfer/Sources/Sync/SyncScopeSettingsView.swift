import CapsuleDomain
import CapsuleMock
import CapsulePorts
import CapsuleUI
import SwiftUI

// MARK: - SyncScopeSettingsView

/// What this device fetches eagerly, how it orders its uploads, and the
/// criteria auto sync waits for.
///
/// Route entry point. Ports required: ``SyncPort`` (scope, connection),
/// ``UploadPort`` (policy), ``SettingsPort`` (auto sync and the staleness
/// warning).
public struct SyncScopeSettingsView: View {
    @State private var model: SyncScopeSettingsModel

    public init(sync: any SyncPort, uploads: any UploadPort, settings: any SettingsPort) {
        _model = State(wrappedValue: SyncScopeSettingsModel(
            sync: sync,
            uploads: uploads,
            settings: settings
        ))
    }

    public var body: some View {
        content
            .navigationTitle("app.sync.scope.title")
            .task { await model.load() }
    }

    @ViewBuilder
    private var content: some View {
        if model.phase.hasContent {
            Form {
                if let refusal = model.lastRefusal { refusalRow(refusal) }
                scopeSection
                policySection
                autoSyncSection
                criteriaSection
            }
            .formStyle(.grouped)
        } else {
            PhasePlaceholderView(
                phase: model.phase,
                emptyTitle: "app.sync.scope.empty.title",
                emptyDescription: "app.sync.scope.empty.description",
                emptySymbol: "slider.horizontal.3",
                retry: { await model.reload() }
            )
        }
    }

    // MARK: Sections

    private var scopeSection: some View {
        Section {
            ForEach(model.selectableScopes, id: \.rawValue) { scope in
                Button {
                    Task { await model.setScope(scope) }
                } label: {
                    ScopeChoiceRow(scope: scope, isSelected: scope == model.scope)
                }
                .buttonStyle(.plain)
            }
        } header: {
            Text("app.sync.scope.section")
        } footer: {
            Text("app.sync.scope.footer")
        }
    }

    private var policySection: some View {
        Section {
            ForEach(model.selectablePolicies, id: \.rawValue) { policy in
                Button {
                    Task { await model.setPolicy(policy) }
                } label: {
                    PolicyChoiceRow(policy: policy, isSelected: policy == model.policy)
                }
                .buttonStyle(.plain)
            }
        } header: {
            Text("app.sync.policy.section")
        } footer: {
            Text("app.sync.policy.footer")
        }
    }

    private var autoSyncSection: some View {
        Section {
            Toggle("app.sync.auto.enabled", isOn: Binding(
                get: { model.settings.autoSyncEnabled },
                set: { value in Task { await model.setAutoSyncEnabled(value) } }
            ))
            Toggle("app.sync.auto.staleness_warning", isOn: Binding(
                get: { model.settings.stalenessNotificationEnabled },
                set: { value in Task { await model.setStalenessNotificationEnabled(value) } }
            ))
        } header: {
            Text("app.sync.auto.section")
        } footer: {
            Text("app.sync.auto.footer")
        }
    }

    /// The criteria are **reported, not configured**: they are the platform's
    /// rules, and two of them are not observable from this layer at all.
    private var criteriaSection: some View {
        Section {
            ForEach(model.criteria) { criterion in
                CriterionRow(criterion: criterion, standing: model.standing(of: criterion))
            }
        } header: {
            Text("app.sync.criteria.section")
        } footer: {
            Text("app.sync.criteria.footer")
        }
    }

    private func refusalRow(_ error: CapsuleError) -> some View {
        Section {
            Label(LocalizedStringKey(error.localizationKey), systemImage: "exclamationmark.triangle")
                .foregroundStyle(.orange)
        }
    }
}

// MARK: - ScopeChoiceRow

/// One scope, with a checkmark rather than a colour to mark the selection.
struct ScopeChoiceRow: View {
    let scope: SyncScope
    let isSelected: Bool

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: CapsuleTheme.Spacing.small) {
            Image(systemName: isSelected ? "checkmark.circle.fill" : "circle")
                .foregroundStyle(isSelected ? scope.badge.tint : Color.secondary)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
                Label(LocalizedStringKey(scope.badge.titleKey), systemImage: scope.badge.systemImage)
                Text(scope.explanationKey)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(isSelected ? [.isSelected] : [])
    }
}

// MARK: - PolicyChoiceRow

/// `full` or `staged`. Ordering, not a different kind of upload.
struct PolicyChoiceRow: View {
    let policy: UploadPolicy
    let isSelected: Bool

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: CapsuleTheme.Spacing.small) {
            Image(systemName: isSelected ? "checkmark.circle.fill" : "circle")
                .foregroundStyle(isSelected ? Color.accentColor : Color.secondary)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
                Text(titleKey)
                Text(explanationKey)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(isSelected ? [.isSelected] : [])
    }

    private var titleKey: LocalizedStringKey {
        policy == .staged ? "app.sync.policy.staged" : "app.sync.policy.full"
    }

    private var explanationKey: LocalizedStringKey {
        policy == .staged ? "app.sync.policy.staged.description" : "app.sync.policy.full.description"
    }
}

// MARK: - CriterionRow

/// One auto-sync criterion and whether it currently holds.
struct CriterionRow: View {
    let criterion: AutoSyncCriterion
    let standing: AutoSyncCriterion.Standing

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: CapsuleTheme.Spacing.small) {
            Image(systemName: symbol)
                .foregroundStyle(tint)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
                Text(LocalizedStringKey(criterion.titleKey))
                Text(LocalizedStringKey(criterion.explanationKey))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: CapsuleTheme.Spacing.small)
            Text(standingKey)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .combine)
    }

    private var standingKey: LocalizedStringKey {
        switch standing {
        case .satisfied: "app.sync.criterion.satisfied"
        case .notSatisfied: "app.sync.criterion.not_satisfied"
        case .unknown: "app.sync.criterion.unknown"
        }
    }

    private var symbol: String {
        switch standing {
        case .satisfied: "checkmark.circle.fill"
        case .notSatisfied: "pause.circle.fill"
        case .unknown: "questionmark.circle"
        }
    }

    private var tint: Color {
        switch standing {
        case .satisfied: .green
        case .notSatisfied: .orange
        case .unknown: .secondary
        }
    }
}

// MARK: - Previews

#Preview("Metered, staged policy") {
    let environment = MockEnvironment(scenario: .awaitingOriginals)
    return NavigationStack {
        SyncScopeSettingsView(
            sync: environment.sync,
            uploads: environment.uploads,
            settings: environment.settings
        )
    }
}

#Preview("Offline") {
    let environment = MockEnvironment(scenario: .offline)
    return NavigationStack {
        SyncScopeSettingsView(
            sync: environment.sync,
            uploads: environment.uploads,
            settings: environment.settings
        )
    }
    .preferredColorScheme(.dark)
}
