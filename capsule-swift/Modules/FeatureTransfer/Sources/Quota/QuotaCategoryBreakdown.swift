import CapsuleDomain
import Foundation

// MARK: - QuotaCategoryBreakdown

/// The stacked bar behind ``QuotaStatusView``.
///
/// **Trash is the point.** An asset in the trash is still stored, at full size,
/// and counts fully against quota until hard purge (*Quota — Accounting
/// Model*). That number is invisible everywhere else in the app, and "empty the
/// trash" is the highest-leverage action a user over quota can take, so the
/// trash segment is broken out and highlighted rather than folded into a total.
///
/// The honesty constraint: ``QuotaPort`` reports one `used` number, not a
/// server-side breakdown by category. Trash bytes are exact — the server
/// charges them and ``LocalStorageBreakdown`` counts them — while the split of
/// the remainder is **estimated from what this device holds**, and
/// ``isEstimated`` says so on screen. Inventing a precise-looking breakdown
/// from a number the server never sent would be worse than admitting the
/// estimate.
public struct QuotaCategoryBreakdown: Sendable, Equatable {
    /// The categories a person can act on. Not the server's accounting
    /// vocabulary — "provenance blobs" is a true category and a useless one to
    /// show someone trying to free space.
    public enum Category: String, Sendable, Equatable, CaseIterable, Identifiable {
        case originals
        case derivatives
        case metadata
        /// Broken out and highlighted. Still stored, still charged.
        case trash
        /// Charged bytes this device cannot attribute, because it does not hold
        /// the whole library. Shown rather than silently folded into originals.
        case other

        public var id: String { rawValue }
    }

    public struct Segment: Sendable, Equatable, Identifiable {
        public var category: Category
        public var bytes: UInt64

        public var id: Category { category }

        public init(category: Category, bytes: UInt64) {
            self.category = category
            self.bytes = bytes
        }
    }

    /// Non-empty segments in a stable draw order, largest concern last so the
    /// trash segment sits at the leading edge of the free space.
    public var segments: [Segment]
    /// The server's charged total.
    public var usedBytes: UInt64
    /// Headroom before the hard limit.
    public var freeBytes: UInt64
    /// Whether any segment other than trash was inferred from local holdings.
    public var isEstimated: Bool

    public init(segments: [Segment], usedBytes: UInt64, freeBytes: UInt64, isEstimated: Bool) {
        self.segments = segments
        self.usedBytes = usedBytes
        self.freeBytes = freeBytes
        self.isEstimated = isEstimated
    }

    /// A segment's share of the bar, 0…1, measured against the hard limit so
    /// the free space is to scale.
    public func fraction(of segment: Segment) -> Double {
        let denominator = usedBytes &+ freeBytes
        guard denominator > 0 else { return 0 }
        return Double(segment.bytes) / Double(denominator)
    }

    /// Compose the bar from the one exact number and the local ratios.
    public static func make(quota: QuotaStatus, local: LocalStorageBreakdown) -> QuotaCategoryBreakdown {
        let trash = min(local.trashBytes, quota.used)
        let remainder = quota.used - trash
        let originals = local.bytesByTier[.original] ?? 0
        let derivatives = (local.bytesByTier[.preview] ?? 0) + (local.bytesByTier[.thumbnail] ?? 0)
        let metadata = (local.bytesByTier[.lqip] ?? 0) + (local.bytesByTier[.dominantColour] ?? 0)
        let weightTotal = originals + derivatives + metadata

        var segments: [Segment] = []
        if weightTotal == 0 {
            if remainder > 0 { segments.append(Segment(category: .other, bytes: remainder)) }
        } else {
            segments.append(Segment(category: .originals, bytes: share(remainder, originals, weightTotal)))
            segments.append(Segment(category: .derivatives, bytes: share(remainder, derivatives, weightTotal)))
            segments.append(Segment(category: .metadata, bytes: share(remainder, metadata, weightTotal)))
        }
        if trash > 0 { segments.append(Segment(category: .trash, bytes: trash)) }

        return QuotaCategoryBreakdown(
            segments: segments.filter { $0.bytes > 0 },
            usedBytes: quota.used,
            freeBytes: quota.isUnlimited ? 0 : quota.remaining,
            isEstimated: remainder > 0 && weightTotal > 0
        )
    }

    /// Proportional share, computed in `Double` so a 512 GB library cannot
    /// overflow the intermediate product.
    private static func share(_ remainder: UInt64, _ weight: UInt64, _ total: UInt64) -> UInt64 {
        guard total > 0 else { return 0 }
        return UInt64((Double(remainder) * Double(weight) / Double(total)).rounded())
    }
}

// MARK: - Presentation

public extension QuotaCategoryBreakdown.Category {
    var titleKey: String {
        switch self {
        case .originals: "app.quota.category.originals"
        case .derivatives: "app.quota.category.derivatives"
        case .metadata: "app.quota.category.metadata"
        case .trash: "app.quota.category.trash"
        case .other: "app.quota.category.other"
        }
    }

    /// A symbol per category, so the stacked bar's legend never depends on
    /// colour alone.
    var systemImage: String {
        switch self {
        case .originals: "photo.on.rectangle.angled"
        case .derivatives: "square.grid.2x2"
        case .metadata: "text.alignleft"
        case .trash: "trash.fill"
        case .other: "questionmark.circle"
        }
    }
}
