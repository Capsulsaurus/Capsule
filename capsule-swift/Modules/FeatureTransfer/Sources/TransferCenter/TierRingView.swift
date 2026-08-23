import CapsuleDomain
import CapsuleUI
import SwiftUI

// MARK: - TierRingView

/// The staged-upload ladder as three concentric arcs, T0 outermost.
///
/// The ring is deliberately paired with a **visible legend** rather than being
/// the only carrier of the numbers. Three concentric arcs distinguished by
/// colour alone would fail the "colour is never the only signal" rule and would
/// be unreadable at large Dynamic Type; the legend states each rung's name and
/// percentage as text, so the ring itself is decorative and hidden from
/// assistive technology.
///
/// Owning doc: *Download and Synchronization — Upload Tiering (Staged Uploads)*.
public struct TierRingView: View {
    private let progress: [TierProgress]
    private let isGated: (UploadTier) -> Bool

    /// Outermost ring is T0 — the rung that must escape on any usable link.
    private static let ringWidth: CGFloat = 10
    private static let ringSpacing: CGFloat = 5
    private static let diameter: CGFloat = 132

    public init(progress: [TierProgress], isGated: @escaping (UploadTier) -> Bool) {
        self.progress = progress
        self.isGated = isGated
    }

    public var body: some View {
        ViewThatFits(in: .horizontal) {
            HStack(alignment: .center, spacing: CapsuleTheme.Spacing.xLarge) {
                ring
                legend
            }
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.large) {
                ring.frame(maxWidth: .infinity)
                legend
            }
        }
    }

    // MARK: Ring

    private var ring: some View {
        ZStack {
            ForEach(Array(progress.enumerated()), id: \.element.id) { depth, tier in
                arc(for: tier, depth: depth)
            }
        }
        .frame(width: Self.diameter, height: Self.diameter)
        .accessibilityHidden(true)
    }

    private func arc(for tier: TierProgress, depth: Int) -> some View {
        let inset = CGFloat(depth) * (Self.ringWidth + Self.ringSpacing)
        return ZStack {
            Circle()
                .stroke(tier.tier.badge.tint.opacity(0.15), lineWidth: Self.ringWidth)
            Circle()
                .trim(from: 0, to: tier.fractionComplete)
                .stroke(
                    tier.tier.badge.tint,
                    style: StrokeStyle(lineWidth: Self.ringWidth, lineCap: .round)
                )
                .rotationEffect(.degrees(-90))
        }
        .padding(inset)
    }

    // MARK: Legend

    private var legend: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            ForEach(progress) { tier in
                TierLegendRow(progress: tier, isGated: isGated(tier.tier))
            }
        }
    }
}

// MARK: - TierLegendRow

/// One rung, as text. The ring's accessible counterpart.
struct TierLegendRow: View {
    let progress: TierProgress
    let isGated: Bool

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: CapsuleTheme.Spacing.small) {
            Image(systemName: progress.tier.badge.systemImage)
                .foregroundStyle(progress.tier.badge.tint)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
                Text(LocalizedStringKey(progress.tier.badge.titleKey))
                    .font(.subheadline)
                standingLine
            }
            Spacer(minLength: CapsuleTheme.Spacing.small)
            Text(verbatim: TransferFormat.percent(progress.fractionComplete))
                .font(.subheadline.monospacedDigit())
                .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .combine)
    }

    /// An idle rung says so in words. "Nothing queued" and "everything sent"
    /// look identical on an empty arc and mean opposite things.
    @ViewBuilder
    private var standingLine: some View {
        switch progress.standing {
        case .idle:
            Text("app.transfer.tier.standing.idle")
                .font(.caption)
                .foregroundStyle(.secondary)
        case .settled:
            Text("app.transfer.tier.standing.settled")
                .font(.caption)
                .foregroundStyle(.secondary)
        case .inFlight:
            if isGated {
                Label("app.transfer.tier.standing.waiting", systemImage: "pause.circle")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                Text("app.transfer.tier.standing.in_flight")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }
}
