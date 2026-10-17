import CapsuleDomain
import CapsuleMock
import CapsuleUI
import SwiftUI

// MARK: - ImportHistoryView

/// **Step 5 — what happened before.**
///
/// Each row expands to the run's outcome rather than linking away to it: the
/// question a user brings here — "did those four hundred photos actually land?"
/// — is answered by four numbers, and a push transition to show four numbers is
/// a push transition too many.
public struct ImportHistoryView: View {
    @State private var model: ImportHistoryModel
    private let onRerun: (@MainActor (ImportPlan) -> Void)?

    public init(model: ImportHistoryModel, onRerun: (@MainActor (ImportPlan) -> Void)? = nil) {
        _model = State(initialValue: model)
        self.onRerun = onRerun
    }

    public init(environment: ImportEnvironment, onRerun: (@MainActor (ImportPlan) -> Void)? = nil) {
        self.init(model: ImportHistoryModel(environment: environment), onRerun: onRerun)
    }

    public var body: some View {
        ImportScreen(
            titleKey: "app.import.history.title",
            phase: model.phase,
            emptyTitleKey: "app.import.history.empty.title",
            emptyDescriptionKey: "app.import.history.empty.description",
            emptySymbol: "clock.arrow.circlepath",
            retry: { await model.load() },
            content: { historyList }
        )
        .task { await model.load() }
    }

    private var historyList: some View {
        List {
            ForEach(model.sessions) { session in
                sessionRows(session)
            }
        }
    }

    @ViewBuilder
    private func sessionRows(_ session: ImportSessionRecord) -> some View {
        Section {
            ImportHistoryHeaderRow(
                session: session,
                isExpanded: model.isExpanded(session.id),
                toggle: { model.toggle(session.id) }
            )
            if model.isExpanded(session.id) {
                ImportHistoryOutcomeRows(session: session, albumName: model.albumName(session.destinationAlbumID))
                actionRows(session)
            }
        }
    }

    @ViewBuilder
    private func actionRows(_ session: ImportSessionRecord) -> some View {
        Button("app.import.history.rerun") {
            Task {
                if let plan = await model.rerun(session.id) { onRerun?(plan) }
            }
        }
        Button("app.import.history.dismiss", role: .destructive) {
            Task { await model.dismiss(session.id) }
        }
    }
}

// MARK: - ImportHistoryHeaderRow

/// The always-visible half of a history row.
private struct ImportHistoryHeaderRow: View {
    let session: ImportSessionRecord
    let isExpanded: Bool
    let toggle: () -> Void

    var body: some View {
        Button(action: toggle) {
            HStack(alignment: .firstTextBaseline, spacing: CapsuleTheme.Spacing.small) {
                text
                Spacer(minLength: CapsuleTheme.Spacing.small)
                Image(systemName: isExpanded ? "chevron.down" : "chevron.forward")
                    .font(.footnote)
                    .foregroundStyle(.tertiary)
                    .accessibilityHidden(true)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(isExpanded ? [.isButton, .isSelected] : .isButton)
    }

    private var text: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
            Label(LocalizedStringKey(session.scope.sourceKind.importTitleKey), systemImage: session.scope.sourceKind.importSymbol)
                .font(.body)
            Text(verbatim: ImportFormat.timestamp(session.startedAt))
                .font(.caption)
                .foregroundStyle(.secondary)
            ImportStatusLabel(titleKey: session.outcome.titleKey, tone: session.outcome.tone)
                .font(.caption)
        }
    }
}

// MARK: - ImportHistoryOutcomeRows

/// The expanded half: what the run actually did.
///
/// Deferred derivatives are reported separately from failures rather than folded
/// into them. An asset imported without a thumbnail is signed, encrypted, and
/// verifiable — only the preview is missing — and counting it as a loss would
/// make a HEIC-only library look like it dropped photos.
private struct ImportHistoryOutcomeRows: View {
    let session: ImportSessionRecord
    let albumName: String?

    var body: some View {
        Group {
            destinationRow
            ImportValueRow(labelKey: "app.import.history.imported", value: ImportFormat.count(session.summary.importedCount))
            ImportValueRow(labelKey: "app.import.history.skipped", value: ImportFormat.count(session.summary.skippedCount))
            deferredRow
            failedRow
            ImportValueRow(labelKey: "app.import.history.mode", value: String(localized: String.LocalizationValue(session.mode.titleKey)))
            ImportValueRow(labelKey: "app.import.history.elapsed", value: ImportFormat.elapsed(from: session.startedAt, to: session.finishedAt))
        }
    }

    private var destinationRow: some View {
        ImportDestinationRow(albumName: albumName, rule: session.destinationRule)
    }

    @ViewBuilder
    private var deferredRow: some View {
        if session.summary.deferredDerivativeCount > 0 {
            ImportValueRow(
                labelKey: "app.import.history.deferred",
                value: ImportFormat.count(session.summary.deferredDerivativeCount)
            )
        }
    }

    @ViewBuilder
    private var failedRow: some View {
        if !session.retryableLocators.isEmpty {
            ImportValueRow(
                labelKey: "app.import.history.failed",
                value: ImportFormat.count(session.retryableLocators.count)
            )
        }
    }
}

// MARK: - Previews

#Preview("History — populated") {
    NavigationStack {
        ImportHistoryView(environment: .preview(.healthy))
    }
}

#Preview("History — nothing imported yet") {
    NavigationStack {
        ImportHistoryView(environment: .preview(.emptyLibrary))
    }
}

#Preview("History — offline") {
    NavigationStack {
        ImportHistoryView(environment: .preview(.offline))
    }
}
