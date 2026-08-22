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
            .navigationTitle("ios.sync.title")
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
                emptyTitle: "ios.sync.empty.title",
                emptyDescription: "ios.sync.empty.description",
                emptySymbol: "arrow.triangle.2.circlepath",
                retry: { await model.reload() }
            )
        }
    }

    // MARK: Sections

    private var statusSection: some View {
        Section {
            LabeledContent("ios.sync.last_completed") {
                if let cursor = model.cursorPosition {
                    Text(verbatim: TransferFormat.relative(cursor, now: model.now))
                } else {
                    Text("ios.sync.last_completed.never")
                }
            }
            LabeledContent("ios.sync.cursor") {
                if let cursor = model.cursorPosition {
                    Text(verbatim: TransferFormat.captureDate(cursor))
                        .font(.footnote.monospacedDigit())
                } else {
                    Text("ios.sync.cursor.unset")
                }
            }
            BadgeChip(model.connection.badge)
            Text(model.connection.policyKey)
                .font(.footnote)
                .foregroundStyle(.secondary)
        } header: {
            Text("ios.sync.status.section")
        } footer: {
            Text("ios.sync.cursor.footer")
        }
    }

    private var pendingSection: some View {
        Section {
            LabeledContent("ios.sync.pending.uploads") {
                Text(verbatim: TransferFormat.count(model.status.pendingUploadCount))
            }
            LabeledContent("ios.sync.pending.downloads") {
                Text(verbatim: TransferFormat.count(model.status.pendingDownloadCount))
            }
            if !model.hasPendingWork {
                Label("ios.sync.pending.none", systemImage: "checkmark.circle")
                    .foregroundStyle(.secondary)
            }
        } header: {
            Text("ios.sync.pending.section")
        }
    }

    private var actionsSection: some View {
        Section {
            Button("ios.sync.action.sync_now") {
                Task { await model.synchronizeNow() }
            }
            .disabled(model.isSyncing || !model.phase.permitsNetworkActions)
            Button("ios.sync.action.force_now") {
                Task { await model.forceSynchronizeNow() }
            }
            .disabled(model.isSyncing || !model.phase.permitsNetworkActions)
            if model.isSyncing {
                ProgressView()
                    .accessibilityLabel("ios.sync.state.running")
            }
        } footer: {
            Text(model.canRunLargeReconciliation
                ? "ios.sync.action.footer.unmetered"
                : "ios.sync.action.footer.deferred")
        }
    }

    private var snoozeSection: some View {
        Section {
            Label("ios.sync.snooze.active", systemImage: "bell.slash")
                .foregroundStyle(.secondary)
        } footer: {
            Text("ios.sync.snooze.footer")
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
            Label("ios.sync.stale.title", systemImage: "exclamationmark.arrow.triangle.2.circlepath")
                .font(.headline)
                .foregroundStyle(.orange)
            Text("ios.sync.stale.description")
                .font(.footnote)
            ViewThatFits(in: .horizontal) {
                HStack(spacing: CapsuleTheme.Spacing.small) { actions }
                VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) { actions }
            }
        } footer: {
            Text("ios.sync.stale.footer")
        }
    }

    @ViewBuilder
    private var actions: some View {
        Button("ios.sync.action.force_now") {
            Task { await model.forceSynchronizeNow() }
        }
        .buttonStyle(.borderedProminent)
        Button("ios.sync.action.snooze") {
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
