import CapsuleDomain
import CapsuleUI
import SwiftUI

// MARK: - CacheBudgetControl

/// The user-configurable cache budget.
///
/// Capsule manages reclamation itself rather than waiting for the user or
/// letting the OS decide — rebuildable data is deliberately **not** stored in
/// OS-managed cache locations, because the OS evicts indiscriminately and a
/// thumbnail that is expensive to regenerate is not genuinely disposable
/// (*Filesystem — Client: Space Recovery*). The budget is the one knob that
/// governs that sweep.
struct CacheBudgetControl: View {
    let budgetBytes: UInt64
    let reclaimableBytes: UInt64
    let overBudgetBytes: UInt64
    let hasExplicitBudget: Bool
    let commit: (UInt64) -> Void

    /// A budget below this is not worth offering: below roughly a gigabyte the
    /// sweep runs constantly and every scroll re-fetches.
    private static let floorBytes: UInt64 = 1073741824

    @State private var draft: Double = 0

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            LabeledContent("app.storage.budget.title") {
                Text(verbatim: TransferFormat.bytes(UInt64(draft)))
                    .monospacedDigit()
            }
            Slider(
                value: $draft,
                in: Double(Self.floorBytes) ... Double(max(ceiling, Self.floorBytes * 2)),
                step: Double(Self.floorBytes / 2)
            ) {
                Text("app.storage.budget.title")
            } minimumValueLabel: {
                Text(verbatim: TransferFormat.bytes(Self.floorBytes))
            } maximumValueLabel: {
                Text(verbatim: TransferFormat.bytes(ceiling))
            } onEditingChanged: { isEditing in
                if !isEditing { commit(UInt64(draft)) }
            }
            .accessibilityLabel("app.storage.budget.title")
            .accessibilityValue(Text(verbatim: TransferFormat.bytes(UInt64(draft))))
            if !hasExplicitBudget {
                Text("app.storage.budget.unset")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            if overBudgetBytes > 0 {
                Label("app.storage.budget.over", systemImage: "exclamationmark.triangle")
                    .font(.caption)
                    .foregroundStyle(.orange)
            }
        }
        .onAppear { draft = Double(budgetBytes) }
    }

    /// Headroom above what is currently reclaimable, so the slider is not
    /// pinned to its maximum on a device with a small library.
    private var ceiling: UInt64 {
        max(reclaimableBytes &* 2, Self.floorBytes &* 8)
    }
}

// MARK: - StorageConsumerRow

/// One consumer of local disk, with its exemption stated in words.
struct StorageConsumerRow: View {
    let consumer: StorageConsumer

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: CapsuleTheme.Spacing.small) {
            Image(systemName: symbol)
                .foregroundStyle(tint)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
                Text(titleKey)
                if consumer.isExempt {
                    Label("app.storage.consumer.exempt", systemImage: "pin.fill")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                if consumer.kind == .trash {
                    Text("app.storage.consumer.trash.note")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            Spacer(minLength: CapsuleTheme.Spacing.small)
            Text(verbatim: TransferFormat.bytes(consumer.bytes))
                .font(.subheadline.monospacedDigit())
                .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .combine)
    }

    private var titleKey: LocalizedStringKey {
        switch consumer.kind {
        case .tier: consumer.tier.map { LocalizedStringKey($0.badge.titleKey) } ?? "app.storage.consumer.unknown"
        case .trash: "app.storage.consumer.trash"
        case .unreleasedOriginals: "app.storage.consumer.unreleased"
        }
    }

    private var symbol: String {
        switch consumer.kind {
        case .tier: consumer.tier?.badge.systemImage ?? "questionmark.circle"
        case .trash: "trash.fill"
        case .unreleasedOriginals: "arrow.up.doc.fill"
        }
    }

    private var tint: Color {
        switch consumer.kind {
        case .tier: consumer.tier?.badge.tint ?? .secondary
        case .trash: .red
        case .unreleasedOriginals: .orange
        }
    }
}

// MARK: - EvictionPreviewList

/// The plan, itemised in the order it would run.
///
/// Numbered explicitly because the order is the safety property: originals
/// first because they are the cheapest to re-fetch and the largest to hold,
/// thumbnails last because losing one costs a round trip on the next scroll.
struct EvictionPreviewList: View {
    let plan: EvictionPlan

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            ForEach(Array(plan.steps.enumerated()), id: \.element.id) { position, step in
                HStack(alignment: .firstTextBaseline, spacing: CapsuleTheme.Spacing.small) {
                    Text(verbatim: TransferFormat.count(position + 1))
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.secondary)
                    Label(LocalizedStringKey(step.tier.badge.titleKey), systemImage: step.tier.badge.systemImage)
                        .foregroundStyle(step.tier.badge.tint)
                    Spacer(minLength: CapsuleTheme.Spacing.small)
                    Text(verbatim: TransferFormat.bytes(step.bytes))
                        .font(.subheadline.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
                .accessibilityElement(children: .combine)
            }
            LabeledContent("app.storage.plan.total") {
                Text(verbatim: TransferFormat.bytes(plan.reclaimedBytes))
            }
            if plan.exemptBytes > 0 {
                Label("app.storage.plan.exempt", systemImage: "lock.fill")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            if plan.shortfallBytes > 0 {
                Label("app.storage.plan.shortfall", systemImage: "exclamationmark.triangle")
                    .font(.caption)
                    .foregroundStyle(.orange)
            }
            Text("app.storage.plan.refetch_note")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }
}
