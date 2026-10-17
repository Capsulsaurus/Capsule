import CapsuleDomain
import Foundation

// MARK: - TransferClock

/// The injected clock every view model in this module reads.
///
/// Nothing here calls `Date()` directly. Three of these screens are *entirely*
/// about elapsed time — the two-week staleness prompt
/// (*Download and Synchronization — Notifications*), the quota grace window
/// (*Quota — Thresholds and States*), and the 60-second verdict freshness rule
/// (*Storage Verification — Verify Before Destroy*) — so a test that asserts
/// "three days of grace remain" has to be able to stop the clock.
public struct TransferClock: Sendable {
    private let source: @Sendable () -> CapsuleTimestamp

    public init(_ source: @escaping @Sendable () -> CapsuleTimestamp) {
        self.source = source
    }

    /// The current instant.
    public var now: CapsuleTimestamp { source() }

    /// The wall clock. The default for the app; never used by a test.
    public static let system = TransferClock {
        CapsuleTimestamp(epochSeconds: Int64(Date().timeIntervalSince1970))
    }

    /// A stopped clock.
    public static func fixed(_ instant: CapsuleTimestamp) -> TransferClock {
        TransferClock { instant }
    }

    /// An instant a whole number of days from now.
    public func offset(days: Int) -> CapsuleTimestamp {
        CapsuleTimestamp(epochSeconds: now.epochSeconds + Int64(days) * 86400)
    }
}
