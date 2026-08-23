import CapsuleDomain
import CapsuleFoundation
import CapsuleUI
import SwiftUI

// MARK: - AssetSwatch

/// The bottom rung of the degrade ladder, drawn.
///
/// An asset always has *something* to show: the LQIP's dominant colour rides
/// inside the metadata blob and is renderable with no decode at all
/// (*Download and Synchronization — Tiered, On-Demand Fetch*). So a transfer
/// row is never blank and never shows a spinner where a photograph goes.
/// Decorative by construction — the row's text carries the identity.
///
/// `RGBColor` is module-qualified because macOS still exposes QuickDraw's C
/// `RGBColor` through ApplicationServices, so the bare name is ambiguous there
/// while compiling fine on iOS — exactly the kind of difference that would
/// otherwise only surface in the Mac build.
struct AssetSwatch: View {
    let colour: CapsuleDomain.RGBColor?
    let mediaType: MediaType?
    var side: CGFloat = 44

    var body: some View {
        RoundedRectangle(cornerRadius: CapsuleTheme.Radius.small, style: .continuous)
            .fill(fill)
            .frame(width: side, height: side)
            .overlay(glyph)
            .accessibilityHidden(true)
    }

    private var fill: Color {
        guard let colour else { return Color.secondary.opacity(0.2) }
        return Color(
            red: Double(colour.red) / 255,
            green: Double(colour.green) / 255,
            blue: Double(colour.blue) / 255
        )
    }

    @ViewBuilder
    private var glyph: some View {
        switch mediaType {
        case .video:
            Image(systemName: "video.fill").foregroundStyle(CapsuleTheme.Colors.onMedia)
        case .livePhoto:
            Image(systemName: "livephoto").foregroundStyle(CapsuleTheme.Colors.onMedia)
        case .photo, .none:
            EmptyView()
        }
    }
}

// MARK: - TransferRowView

/// One asset in flight.
///
/// Identity is the **capture date**, never a filename: no filename crosses the
/// wire (*Upload Protocol — What Gets Uploaded*), so there is none to show.
struct TransferRowView: View {
    let row: TransferRow

    var body: some View {
        HStack(alignment: .top, spacing: CapsuleTheme.Spacing.medium) {
            AssetSwatch(colour: row.dominantColour, mediaType: row.mediaType)
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
                captureLine
                tierChips
                progressLine
            }
        }
        .padding(.vertical, CapsuleTheme.Spacing.xxSmall)
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder
    private var captureLine: some View {
        if let captureDate = row.captureDate {
            Text(verbatim: TransferFormat.captureDate(captureDate))
                .font(.body)
        } else {
            // Honest about the one case where there is no date yet, rather than
            // substituting an identifier the user has never seen.
            Text("app.transfer.row.capture_date_pending")
                .font(.body)
                .foregroundStyle(.secondary)
        }
    }

    private var tierChips: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: CapsuleTheme.Spacing.xSmall) {
                ForEach(row.tiers, id: \.self) { tier in BadgeChip(tier.badge) }
            }
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
                ForEach(row.tiers, id: \.self) { tier in BadgeChip(tier.badge) }
            }
        }
    }

    private var progressLine: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
            ProgressView(value: row.fractionComplete)
                .progressViewStyle(.linear)
                .accessibilityLabel("app.transfer.row.progress")
                .accessibilityValue(Text(verbatim: TransferFormat.percent(row.fractionComplete)))
            HStack(spacing: CapsuleTheme.Spacing.small) {
                BadgeChip(row.headlineState.badge)
                throughput
            }
        }
    }

    /// "Measuring" rather than "0 B/s" before two samples exist. A stalled
    /// transfer and an unmeasured one are different facts and must not share a
    /// readout.
    @ViewBuilder
    private var throughput: some View {
        if let rate = row.bytesPerSecond {
            Text(verbatim: TransferFormat.rate(bytesPerSecond: rate))
                .font(.caption.monospacedDigit())
                .foregroundStyle(.secondary)
                .accessibilityLabel("app.transfer.row.throughput")
        } else {
            Text("app.transfer.row.throughput_measuring")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }
}

// MARK: - ConnectionFooter

/// The connection-class chip and the transfer policy it implies.
///
/// The policy sentence is the point: `metered` alone means nothing to a person,
/// while "Originals wait for Wi-Fi" is the whole staged-upload contract in five
/// words (*Download and Synchronization — Synchronization Criteria*).
struct ConnectionFooter: View {
    let connection: ConnectionClass
    let policy: UploadPolicy
    let aggregateBytesPerSecond: Double?

    var body: some View {
        CapsuleGlassContainer(spacing: CapsuleTheme.Spacing.small) {
            ViewThatFits(in: .horizontal) {
                HStack(spacing: CapsuleTheme.Spacing.medium) { chip
                    policyText
                    Spacer()
                    rate
                }
                VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) { chip
                    policyText
                    rate
                }
            }
        }
        .padding(CapsuleTheme.Spacing.medium)
        .accessibilityElement(children: .combine)
    }

    private var chip: some View {
        BadgeChip(connection.badge, glass: true)
    }

    private var policyText: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
            Text(connection.policyKey)
                .font(.footnote)
            if policy == .staged {
                Text("app.transfer.policy.staged")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder
    private var rate: some View {
        if let aggregateBytesPerSecond {
            Text(verbatim: TransferFormat.rate(bytesPerSecond: aggregateBytesPerSecond))
                .font(.footnote.monospacedDigit())
                .foregroundStyle(.secondary)
        }
    }
}
