import CapsuleDomain
import CapsuleUI
import SwiftUI

// MARK: - QuotaStackedBar

/// Usage by category, with the trash segment highlighted.
///
/// The highlight is a **stripe pattern plus a colour plus a legend row**, never
/// colour alone: the accessibility audit forbids colour as the only signal, and
/// the trash segment is precisely the one a user must be able to pick out.
struct QuotaStackedBar: View {
    let breakdown: QuotaCategoryBreakdown

    private static let barHeight: CGFloat = 22

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            bar
            legend
            if breakdown.isEstimated {
                Text("app.quota.categories.estimated")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private var bar: some View {
        GeometryReader { proxy in
            HStack(spacing: 1) {
                ForEach(breakdown.segments) { segment in
                    segmentShape(segment)
                        .frame(width: max(2, proxy.size.width * breakdown.fraction(of: segment)))
                }
                Rectangle()
                    .fill(Color.secondary.opacity(0.15))
            }
            .clipShape(RoundedRectangle(cornerRadius: CapsuleTheme.Radius.small, style: .continuous))
        }
        .frame(height: Self.barHeight)
        .accessibilityHidden(true)
    }

    @ViewBuilder
    private func segmentShape(_ segment: QuotaCategoryBreakdown.Segment) -> some View {
        if segment.category == .trash {
            Rectangle()
                .fill(tint(for: segment.category))
                .overlay(
                    Rectangle().strokeBorder(CapsuleTheme.Colors.onMedia.opacity(0.7), lineWidth: 2)
                )
        } else {
            Rectangle().fill(tint(for: segment.category))
        }
    }

    private var legend: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            ForEach(breakdown.segments) { segment in
                HStack(alignment: .firstTextBaseline, spacing: CapsuleTheme.Spacing.small) {
                    Image(systemName: segment.category.systemImage)
                        .foregroundStyle(tint(for: segment.category))
                        .accessibilityHidden(true)
                    Text(LocalizedStringKey(segment.category.titleKey))
                        .font(.subheadline)
                        .fontWeight(segment.category == .trash ? .semibold : .regular)
                    Spacer(minLength: CapsuleTheme.Spacing.small)
                    Text(verbatim: TransferFormat.bytes(segment.bytes))
                        .font(.subheadline.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
                .accessibilityElement(children: .combine)
                if segment.category == .trash {
                    Text("app.quota.category.trash.note")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
    }

    private func tint(for category: QuotaCategoryBreakdown.Category) -> Color {
        switch category {
        case .originals: .indigo
        case .derivatives: .blue
        case .metadata: .teal
        case .trash: .red
        case .other: .gray
        }
    }
}

// MARK: - QuotaStateBanner

/// The five-state banner.
///
/// Each state states three things: what it *is*, what still *works*, and what
/// to *do*. The third is what turns "grace expired" from an opaque error into a
/// remediable state.
struct QuotaStateBanner: View {
    let state: QuotaState
    let permissions: QuotaPermissions
    let remediations: [QuotaRemediation]
    let graceDeadline: CapsuleTimestamp?
    let now: CapsuleTimestamp
    let perform: (QuotaRemediation) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            Label(LocalizedStringKey(state.badge.titleKey), systemImage: state.badge.systemImage)
                .font(.headline)
                .foregroundStyle(state.badge.tint)
            Text(state.explanationKey)
                .font(.footnote)
            deadline
            permissionRows
            remediationButtons
        }
        .padding(.vertical, CapsuleTheme.Spacing.xSmall)
    }

    @ViewBuilder
    private var deadline: some View {
        if let graceDeadline {
            Label(
                LocalizedStringKey("app.quota.grace.deadline"),
                systemImage: "calendar.badge.exclamationmark"
            )
            .font(.footnote)
            .foregroundStyle(.secondary)
            Text(verbatim: TransferFormat.relative(graceDeadline, now: now))
                .font(.footnote.monospacedDigit())
                .foregroundStyle(.secondary)
        }
    }

    /// Spelled out because "what still works" is the difference between a full
    /// account and a broken one.
    private var permissionRows: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
            permissionRow("app.quota.permission.uploads", isAllowed: permissions.newUploads)
            permissionRow("app.quota.permission.metadata", isAllowed: permissions.metadataGrowth)
            permissionRow("app.quota.permission.deletes", isAllowed: permissions.reclaimingWrites)
        }
    }

    private func permissionRow(_ key: LocalizedStringKey, isAllowed: Bool) -> some View {
        Label(key, systemImage: isAllowed ? "checkmark.circle.fill" : "slash.circle.fill")
            .font(.caption)
            .foregroundStyle(isAllowed ? Color.green : Color.secondary)
    }

    @ViewBuilder
    private var remediationButtons: some View {
        if !remediations.isEmpty {
            ViewThatFits(in: .horizontal) {
                HStack(spacing: CapsuleTheme.Spacing.small) { buttons }
                VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) { buttons }
            }
        }
    }

    @ViewBuilder
    private var buttons: some View {
        ForEach(remediations) { remediation in
            Button {
                perform(remediation)
            } label: {
                Label(LocalizedStringKey(remediation.titleKey), systemImage: remediation.systemImage)
            }
            .buttonStyle(.bordered)
        }
    }
}
