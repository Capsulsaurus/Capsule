import CapsuleUI
import SwiftUI

// MARK: - EnrollmentStageRailRow

/// One named step of the enrollment rail.
///
/// Every row carries the same four things whatever it is doing: a symbol, the
/// step's name, one plain-language sentence, and a live status that is text as
/// well as colour. That is what a named rail buys over a percentage — a user who
/// looks up mid-ceremony can tell *which* thing is happening and, if it stops,
/// which thing stopped.
///
/// A `deferred` row also renders its reason, because "finished, but the upload
/// will land later" is a different promise from "finished" and the difference is
/// the whole reason the status exists.
struct EnrollmentStageRailRow: View {
    let row: EnrollmentStageRow

    var body: some View {
        HStack(alignment: .top, spacing: CapsuleTheme.Spacing.medium) {
            Image(systemName: row.stage.symbolName)
                .font(.title3)
                .frame(width: 28)
                .foregroundStyle(.tint)
                .accessibilityHidden(true)
            detail
        }
        .authInnerCard()
        .accessibilityElement(children: .combine)
    }

    private var detail: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            Text(LocalizedStringKey(row.stage.titleKey))
                .font(.headline)
                .fixedSize(horizontal: false, vertical: true)
            Text(LocalizedStringKey(row.stage.explanationKey))
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            status
            if case let .deferred(reasonKey) = row.status {
                Text(LocalizedStringKey(reasonKey))
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder
    private var status: some View {
        if row.status == .running {
            HStack(spacing: CapsuleTheme.Spacing.xSmall) {
                ProgressView().controlSize(.small)
                Text("app.enrollment.status.running")
                    .font(.caption.weight(.medium))
                    .foregroundStyle(.secondary)
            }
            .accessibilityElement(children: .combine)
        } else {
            StatusChip(titleKey: statusTitleKey, symbolName: statusSymbol, tint: statusTint)
        }
    }

    private var statusTitleKey: LocalizedStringKey {
        switch row.status {
        case .pending: "app.enrollment.status.pending"
        case .running: "app.enrollment.status.running"
        case .done: "app.enrollment.status.done"
        case .deferred: "app.enrollment.status.deferred"
        case .failed: "app.enrollment.status.failed"
        }
    }

    private var statusSymbol: String {
        switch row.status {
        case .pending: "circle.dotted"
        case .running: "circle.hexagonpath"
        case .done: "checkmark.circle.fill"
        case .deferred: "clock.badge.checkmark.fill"
        case .failed: "xmark.circle.fill"
        }
    }

    private var statusTint: Color {
        switch row.status {
        case .pending, .running: .secondary
        case .done: .green
        case .deferred: .orange
        case .failed: .red
        }
    }
}

// MARK: - Previews

#Preview("Enrollment stage rows") {
    VStack(spacing: CapsuleTheme.Spacing.small) {
        EnrollmentStageRailRow(row: EnrollmentStageRow(stage: .masterKey, status: .done))
        EnrollmentStageRailRow(row: EnrollmentStageRow(stage: .deviceKeys, status: .running))
        EnrollmentStageRailRow(
            row: EnrollmentStageRow(
                stage: .publishDirectory,
                status: .deferred(reasonKey: "app.enrollment.deferred.directory")
            )
        )
        EnrollmentStageRailRow(
            row: EnrollmentStageRow(stage: .defaultAlbum, status: .failed(.hardwareKeyUnavailable))
        )
    }
    .padding()
}
