import CapsuleDomain
import Foundation

// MARK: - EvictionPlan

/// What "free up space" would actually delete, computed **before** anything is
/// deleted.
///
/// Two rules from *Filesystem — Client: Automatic cache management* are encoded
/// here, and they are the reason this is a value type the UI previews rather
/// than a number the port returns:
///
/// - **Tier order.** Where recency does not decide it, eviction proceeds in
///   descending size and ascending value: `original → preview → thumbnail`. The
///   metadata tier — the sidecar and its embedded LQIP — is tiny and canonical
///   and is effectively never reclaimed, so an asset stays listable and
///   previewable at LQIP fidelity after every heavier representation is gone.
/// - **Pin exemption.** Representations the user pinned for offline use, and
///   originals this device still owns as source of truth, are exempt from the
///   automatic sweep regardless of budget pressure. Releasing a device-owned
///   original is gated on a `durable` verdict — which is
///   ``CustodyReceiptView``'s job, not this screen's.
///
/// Previewing rather than acting is the whole point: a user is being asked to
/// consent to deletion, and consent to an unnamed amount of unnamed data is not
/// consent.
public struct EvictionPlan: Sendable, Equatable {
    /// One tier's contribution, in the order it would be taken.
    public struct Step: Sendable, Equatable, Identifiable {
        public var tier: RepresentationTier
        public var bytes: UInt64

        public var id: RepresentationTier { tier }

        public init(tier: RepresentationTier, bytes: UInt64) {
            self.tier = tier
            self.bytes = bytes
        }
    }

    /// The documented sweep order. Not a preference — the doc names it.
    public static let tierOrder: [RepresentationTier] = [.original, .preview, .thumbnail]

    /// Steps in sweep order, zero-byte tiers omitted.
    public var steps: [Step]
    /// What was asked for.
    public var targetBytes: UInt64
    /// What the plan would actually free.
    public var reclaimedBytes: UInt64
    /// Bytes held back because they are pinned or not yet confirmed durable.
    public var exemptBytes: UInt64

    public init(steps: [Step], targetBytes: UInt64, reclaimedBytes: UInt64, exemptBytes: UInt64) {
        self.steps = steps
        self.targetBytes = targetBytes
        self.reclaimedBytes = reclaimedBytes
        self.exemptBytes = exemptBytes
    }

    /// How far short of the target the plan falls. Non-zero means the exempt
    /// set is what is standing in the way, and the screen says so rather than
    /// silently freeing less than asked.
    public var shortfallBytes: UInt64 {
        targetBytes > reclaimedBytes ? targetBytes - reclaimedBytes : 0
    }

    /// Whether the plan would delete anything at all.
    public var isEmpty: Bool { steps.isEmpty }

    /// Compute the plan.
    ///
    /// - Parameters:
    ///   - targetBytes: how much to free.
    ///   - breakdown: what the device holds.
    ///   - pinnedBytes: bytes the user pinned for offline use. Subtracted from
    ///     the original tier, the only tier a pin protects that the sweep would
    ///     otherwise reach first.
    public static func preview(
        targetBytes: UInt64,
        breakdown: LocalStorageBreakdown,
        pinnedBytes: UInt64 = 0
    ) -> EvictionPlan {
        let exempt = breakdown.unreleasedOriginalBytes &+ pinnedBytes
        var remaining = targetBytes
        var steps: [Step] = []
        for tier in tierOrder {
            guard remaining > 0 else { break }
            let held = breakdown.bytesByTier[tier] ?? 0
            let available = tier == .original ? held.subtractingSaturating(exempt) : held
            guard available > 0 else { continue }
            let take = min(remaining, available)
            steps.append(Step(tier: tier, bytes: take))
            remaining -= take
        }
        return EvictionPlan(
            steps: steps,
            targetBytes: targetBytes,
            reclaimedBytes: steps.reduce(UInt64.zero) { $0 + $1.bytes },
            exemptBytes: exempt
        )
    }
}

// MARK: - Saturating arithmetic

extension UInt64 {
    /// Subtraction that floors at zero rather than trapping.
    ///
    /// Byte accounting mixes numbers from two ports; an exempt total larger
    /// than a tier total is a legitimate transient, not a crash.
    func subtractingSaturating(_ other: UInt64) -> UInt64 {
        self > other ? self - other : 0
    }
}
