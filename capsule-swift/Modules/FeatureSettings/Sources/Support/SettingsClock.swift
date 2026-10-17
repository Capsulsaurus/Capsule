import CapsuleDomain
import Foundation

// MARK: - SettingsClock

/// The injected clock every settings view model measures against.
///
/// Nothing in this module calls `Date()` directly. Grace windows, session
/// expiries, staleness thresholds, and retention countdowns are all differences
/// between two instants, and a test that cannot pin "now" can only assert that
/// a countdown is *some* number — which is not the assertion worth writing.
/// `CapsuleMock` makes the same choice for the same reason, so a view model and
/// the world it reads agree on what time it is.
public struct SettingsClock: Sendable {
    private let instant: @Sendable () -> CapsuleTimestamp

    public init(instant: @escaping @Sendable () -> CapsuleTimestamp) {
        self.instant = instant
    }

    /// The current instant.
    public func now() -> CapsuleTimestamp {
        instant()
    }

    /// The wall clock.
    public static let system = SettingsClock {
        CapsuleTimestamp(epochSeconds: Int64(Date().timeIntervalSince1970))
    }

    /// A clock stopped at one instant.
    public static func fixed(_ timestamp: CapsuleTimestamp) -> SettingsClock {
        SettingsClock { timestamp }
    }

    /// A clock stopped at an epoch second.
    public static func fixed(epochSeconds: Int64) -> SettingsClock {
        fixed(CapsuleTimestamp(epochSeconds: epochSeconds))
    }
}
