import CapsuleDomain
import CapsuleMock
import CapsuleNavigation
import SwiftUI

// MARK: - MaintenanceSettingsView

/// Maintenance — scrub, self-validation, repair, deduplicate, rebuild index.
public struct MaintenanceSettingsView: View {
    @State private var model: MaintenanceSettingsModel
    @State private var kindPendingConfirmation: MaintenanceTaskKind?

    public init(model: MaintenanceSettingsModel) {
        _model = State(initialValue: model)
    }

    public init(environment: SettingsEnvironment) {
        self.init(
            model: MaintenanceSettingsModel(
                maintenance: environment.maintenance,
                connectivity: environment.connectivity
            )
        )
    }

    public var body: some View {
        SettingsScreen(
            titleKey: SettingsSection.maintenance.titleKey,
            phase: model.phase,
            emptyTitleKey: "app.settings.maintenance.empty.title",
            emptyDescriptionKey: "app.settings.maintenance.empty.description",
            retry: { await model.load() },
            content: {
                scrubSection
                deduplicationSection
                jobsSection
            }
        )
        .task { await model.load() }
        .settingsDestructiveConfirmation(
            titleKey: "app.settings.maintenance.confirm.title",
            messageKey: "app.settings.maintenance.confirm.message",
            confirmKey: "app.settings.maintenance.confirm.action",
            isPresented: confirmationPresented
        ) {
            if let kind = kindPendingConfirmation {
                await model.run(kind, userInitiated: true)
            }
            kindPendingConfirmation = nil
        }
    }

    private var confirmationPresented: Binding<Bool> {
        Binding(
            get: { kindPendingConfirmation != nil },
            set: { presented in if !presented { kindPendingConfirmation = nil } }
        )
    }

    // MARK: Scrub

    /// The scrub has no port of its own: it is housekeeping the client runs for
    /// itself, at most weekly, over `.tmp` files older than ten minutes. Stated
    /// rather than omitted, so a user reading this screen has the whole list of
    /// what touches their library.
    private var scrubSection: some View {
        Section {
            SettingsNoteRow(textKey: "app.settings.maintenance.scrub.body")
        } header: {
            Text("app.settings.maintenance.scrub.header")
        } footer: {
            Text("app.settings.maintenance.scrub.footer")
        }
    }

    // MARK: Deduplication

    private var deduplicationSection: some View {
        Section {
            if let count = model.pendingDuplicateSetCount {
                SettingsValueRow(
                    labelKey: "app.settings.maintenance.dedupe.found",
                    value: SettingsFormat.count(count)
                )
            }
            Button("app.settings.maintenance.dedupe.run") {
                kindPendingConfirmation = .intraLibraryDeduplication
            }
            .disabled(model.task(.intraLibraryDeduplication)?.state.isRunning == true)
        } header: {
            Text("app.settings.maintenance.dedupe.header")
        } footer: {
            Text("app.settings.maintenance.dedupe.footer")
        }
    }

    // MARK: Jobs

    private var jobsSection: some View {
        Section {
            ForEach(model.tasks) { task in
                jobRows(task)
            }
        } header: {
            Text("app.settings.maintenance.jobs.header")
        } footer: {
            Text("app.settings.maintenance.jobs.footer")
        }
    }

    @ViewBuilder
    private func jobRows(_ task: MaintenanceTask) -> some View {
        SettingsStatusRow(
            labelKey: task.kind.titleKey,
            statusKey: task.state.statusKey,
            tone: task.state.tone
        )
        SettingsNoteRow(textKey: task.kind.detailKey)
        SettingsValueRow(
            labelKey: "app.settings.maintenance.last_run",
            value: SettingsFormat.timestamp(task.lastRunAt)
        )
        jobStateDetail(task)
        jobAction(task)
    }

    @ViewBuilder
    private func jobStateDetail(_ task: MaintenanceTask) -> some View {
        switch task.state {
        case let .running(fraction):
            ProgressView(value: fraction)
                .accessibilityLabel(Text(task.kind.titleKey))
        case let .completed(_, findingCount):
            SettingsValueRow(
                labelKey: "app.settings.maintenance.findings",
                value: SettingsFormat.count(findingCount)
            )
        case let .failed(_, code):
            SettingsStatusRow(
                labelKey: "app.settings.maintenance.failure",
                statusKey: code.rawValue,
                tone: .critical
            )
        case .idle, .waitingForConditions:
            EmptyView()
        }
    }

    /// A destructive job is confirmed; an ordinary one runs on the tap.
    ///
    /// Confirming is not a substitute for the verify-before-destroy gate the
    /// port applies — asking explicitly does not waive it, and the footer says
    /// so — it is only what stops the tap being the whole decision.
    @ViewBuilder
    private func jobAction(_ task: MaintenanceTask) -> some View {
        if task.state.isRunning {
            Button("app.settings.maintenance.cancel", role: .cancel) {
                Task { await model.cancel(task.kind) }
            }
        } else if task.kind.isDestructive || task.kind.requiresIdleAndPower {
            Button("app.settings.maintenance.run") {
                kindPendingConfirmation = task.kind
            }
        } else {
            Button("app.settings.maintenance.run") {
                Task { await model.run(task.kind, userInitiated: true) }
            }
        }
    }
}

// MARK: - Preview

#Preview("Maintenance") {
    NavigationStack {
        MaintenanceSettingsView(environment: .preview())
    }
}

#Preview("Maintenance — Dark") {
    NavigationStack {
        MaintenanceSettingsView(environment: .preview())
    }
    .preferredColorScheme(.dark)
}
