import CapsuleDomain
import Foundation

// MARK: - AlertClock

/// The injected clock the alert loop measures against.
///
/// Nothing in this module calls `Date()` directly: every deadline is a
/// difference between two instants, and the pre-arm lifecycle is only
/// testable when "now" can be pinned and advanced.
public struct AlertClock: Sendable {
    private let instant: @Sendable () -> CapsuleTimestamp

    public init(instant: @escaping @Sendable () -> CapsuleTimestamp) {
        self.instant = instant
    }

    /// The current instant.
    public func now() -> CapsuleTimestamp {
        instant()
    }

    /// The wall clock.
    public static let system = AlertClock {
        CapsuleTimestamp(epochSeconds: Int64(Date().timeIntervalSince1970))
    }

    /// A clock stopped at one instant.
    public static func fixed(_ timestamp: CapsuleTimestamp) -> AlertClock {
        AlertClock { timestamp }
    }
}
