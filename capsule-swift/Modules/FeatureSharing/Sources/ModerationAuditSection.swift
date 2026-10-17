import CapsuleUI
import SwiftUI

// MARK: - ModerationAuditSection

/// The user's moderation audit log, and the appeal against each entry
/// (*Moderation — No Silent Operations*).
///
/// This section is the reason a takedown is not simply a photograph that stopped
/// loading. Every moderation action that affects user data produces a record the
/// user can see — what, when, and where policy permits, why — so nobody is left
/// to guess at a missing asset. The bytes are not deleted either, and the entry
/// says so.
struct ModerationAuditSection: View {
    let model: ModerationViewModel

    var body: some View {
        Section {
            if model.auditEntries.isEmpty {
                Text("app.moderation.audit.none")
                    .foregroundStyle(.secondary)
            } else {
                ForEach(model.auditEntries) { entry in
                    AuditRowView(
                        entry: entry,
                        appeal: model.appeal(for: entry),
                        canAppeal: model.canAppeal(entry),
                        submitAppeal: { Task { await model.appealEntry(entry) } }
                    )
                }
            }
        } header: {
            Text("app.moderation.section.audit")
        } footer: {
            Text("app.moderation.section.audit_footer")
        }
    }
}

// MARK: - AuditRowView

/// One moderation action, with its appeal state.
struct AuditRowView: View {
    let entry: ModerationAuditEntry
    let appeal: ModerationAppeal?
    let canAppeal: Bool
    let submitAppeal: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            Label(actionTitle, systemImage: symbol)
                .font(.subheadline)
            Text(verbatim: entry.subjectDescription)
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
            LabeledContent {
                Text(entry.occurredAt.date, format: .dateTime.year().month().day().hour().minute())
            } label: {
                Text("app.moderation.audit.when")
            }
            .font(.caption)
            .foregroundStyle(.secondary)
            reasonRow
            appealRow
        }
        .padding(.vertical, CapsuleTheme.Spacing.xxSmall)
    }

    /// An undisclosed reason is stated as undisclosed. Policy does not always
    /// permit saying why, and an empty row would read as a bug.
    @ViewBuilder
    private var reasonRow: some View {
        if let reason = entry.reason {
            Text(verbatim: reason)
                .font(.caption)
        } else {
            Text("app.moderation.audit.reason_withheld")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    @ViewBuilder
    private var appealRow: some View {
        if let appeal {
            StatusBadge(title: appealTitle(appeal.state), symbol: appealSymbol(appeal.state), tint: appealTint(appeal.state))
        } else if canAppeal {
            Button("app.moderation.audit.appeal", action: submitAppeal)
                .buttonStyle(.borderless)
                .font(.caption)
        }
    }

    private var actionTitle: LocalizedStringKey {
        switch entry.action {
        case .takedown: "app.moderation.action.takedown"
        case .legalHold: "app.moderation.action.legal_hold"
        case .accountSuspension: "app.moderation.action.suspension"
        case .reinstatement: "app.moderation.action.reinstatement"
        }
    }

    private var symbol: String {
        switch entry.action {
        case .takedown: "eye.slash"
        case .legalHold: "lock.doc"
        case .accountSuspension: "person.crop.circle.badge.exclamationmark"
        case .reinstatement: "arrow.uturn.backward.circle"
        }
    }

    private func appealTitle(_ state: ModerationAppeal.State) -> LocalizedStringKey {
        switch state {
        case .submitted: "app.moderation.appeal.submitted"
        case .underReview: "app.moderation.appeal.under_review"
        case .granted: "app.moderation.appeal.granted"
        case .declined: "app.moderation.appeal.declined"
        }
    }

    private func appealSymbol(_ state: ModerationAppeal.State) -> String {
        switch state {
        case .submitted: "paperplane"
        case .underReview: "hourglass"
        case .granted: "checkmark.seal"
        case .declined: "xmark.seal"
        }
    }

    private func appealTint(_ state: ModerationAppeal.State) -> Color {
        switch state {
        case .submitted, .underReview: .secondary
        case .granted: .green
        case .declined: .red
        }
    }
}
