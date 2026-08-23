import CapsuleDomain
import CapsuleMock
import CapsulePorts
import CapsuleUI
import SwiftUI

// MARK: - SyncStatusView

/// Where the library stands with its server: last sync, cursor position,
/// pending work in both directions, force-sync-now, and the two-week staleness
/// prompt.
///
/// Route entry point. Ports required: ``SyncPort``.
public struct SyncStatusView: View {
    @State private var model: SyncStatusModel

    public init(sync: any SyncPort, clock: TransferClock = .system) {
        _model = State(wrappedValue: SyncStatusModel(sync: sync, clock: clock))
    }

    public var body: some View {
        content
            .navigationTitle("app.sync.title")
            .task { await model.load() }
    }

    @ViewBuilder
    private var content: some View {
        if model.phase.hasContent {
            List {
                // The prompt is a banner in the flow, never a sheet and never a
                // gate: it must not block anything.
                if model.isStale { StalenessPrompt(model: model) }
                if let refusal = model.lastRefusal { refusalRow(refusal) }
                statusSection
                pendingSection
                actionsSection
                if model.isSnoozed { snoozeSection }
            }
            .listStyle(.inset)
            .refreshable { await model.reload() }
        } else {
            PhasePlaceholderView(
                phase: model.phase,
                emptyTitle: "app.sync.empty.title",
                emptyDescription: "app.sync.empty.description",
                emptySymbol: "arrow.triangle.2.circlepath",
                retry: { await model.reload() }
            )
        }
    }

    // MARK: Sections

    private var statusSection: some View {
        Section {
            LabeledContent("app.sync.last_completed") {
                if let cursor = model.cursorPosition {
                    Text(verbatim: TransferFormat.relative(cursor, now: model.now))
                } else {
                    Text("app.sync.last_completed.never")
                }
            }
            LabeledContent("app.sync.cursor") {
                if let cursor = model.cursorPosition {
                    Text(verbatim: TransferFormat.captureDate(cursor))
                        .font(.footnote.monospacedDigit())
                } else {
                    Text("app.sync.cursor.unset")
                }
            }
            BadgeChip(model.connection.badge)
            Text(model.connection.policyKey)
                .font(.footnote)
                .foregroundStyle(.secondary)
        } header: {
            Text("app.sync.status.section")
        } footer: {
            Text("app.sync.cursor.footer")
        }
    }

    private var pendingSection: some View {
        Section {
            LabeledContent("app.sync.pending.uploads") {
                Text(verbatim: TransferFormat.count(model.status.pendingUploadCount))
            }
            LabeledContent("app.sync.pending.downloads") {
                Text(verbatim: TransferFormat.count(model.status.pendingDownloadCount))
            }
            if !model.hasPendingWork {
                Label("app.sync.pending.none", systemImage: "checkmark.circle")
                    .foregroundStyle(.secondary)
            }
        } header: {
            Text("app.sync.pending.section")
        }
    }

    private var actionsSection: some View {
        Section {
            Button("app.sync.action.sync_now") {
                Task { await model.synchronizeNow() }
            }
            .disabled(model.isSyncing || !model.phase.permitsNetworkActions)
            Button("app.sync.action.force_now") {
                Task { await model.forceSynchronizeNow() }
            }
            .disabled(model.isSyncing || !model.phase.permitsNetworkActions)
            if model.isSyncing {
                ProgressView()
                    .accessibilityLabel("app.sync.state.running")
            }
        } footer: {
            Text(model.canRunLargeReconciliation
                ? "app.sync.action.footer.unmetered"
                : "app.sync.action.footer.deferred")
        }
    }

    private var snoozeSection: some View {
        Section {
            Label("app.sync.snooze.active", systemImage: "bell.slash")
                .foregroundStyle(.secondary)
        } footer: {
            Text("app.sync.snooze.footer")
        }
    }

    private func refusalRow(_ error: CapsuleError) -> some View {
        Section {
            Label(LocalizedStringKey(error.localizationKey), systemImage: "exclamationmark.triangle")
                .foregroundStyle(.orange)
            Text(error.recoveryAction.explanationKey)
                .font(.footnote)
                .foregroundStyle(.secondary)
        }
    }
}

// MARK: - StalenessPrompt

/// The two-week prompt.
///
/// Snoozeable, dismissible, and **non-blocking by construction** — it is a
/// section in a list, not a sheet or an alert. It offers the one-tap force sync
/// that proceeds regardless of the metered criteria on the user's explicit
/// consent, which is the whole affordance the design doc specifies.
struct StalenessPrompt: View {
    let model: SyncStatusModel

    var body: some View {
        Section {
            Label("app.sync.stale.title", systemImage: "exclamationmark.arrow.triangle.2.circlepath")
                .font(.headline)
                .foregroundStyle(.orange)
            Text("app.sync.stale.description")
                .font(.footnote)
            ViewThatFits(in: .horizontal) {
                HStack(spacing: CapsuleTheme.Spacing.small) { actions }
                VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) { actions }
            }
        } footer: {
            Text("app.sync.stale.footer")
        }
    }

    @ViewBuilder
    private var actions: some View {
        Button("app.sync.action.force_now") {
            Task { await model.forceSynchronizeNow() }
        }
        .buttonStyle(.borderedProminent)
        Button("app.sync.action.snooze") {
            Task { await model.snoozeStalenessPrompt() }
        }
        .buttonStyle(.bordered)
    }
}

// MARK: - Previews

#Preview("Stale for three weeks") {
    let environment = MockEnvironment(scenario: .awaitingOriginals)
    return NavigationStack {
        SyncStatusView(sync: environment.sync, clock: .fixed(environment.configuration.clock.now))
    }
}

#Preview("Offline") {
    let environment = MockEnvironment(scenario: .offline)
    return NavigationStack {
        SyncStatusView(sync: environment.sync, clock: .fixed(environment.configuration.clock.now))
    }
    .preferredColorScheme(.dark)
}

#Preview("Healthy") {
    let environment = MockEnvironment(scenario: .healthy)
    return NavigationStack {
        SyncStatusView(sync: environment.sync, clock: .fixed(environment.configuration.clock.now))
    }
}
