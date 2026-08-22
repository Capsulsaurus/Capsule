import CapsuleDomain
import Foundation

// MARK: - DeviceCohortGroup

/// One physical device's worth of ledger rows.
///
/// The session ledger has a legibility problem that this type exists to fix:
/// reinstalling the app re-enrolls with a **new** `device_id` by design, because
/// device keys are hardware-bound and non-exportable. One phone therefore
/// accumulates several ledger entries over its life, and a flat list presents
/// them as several strangers.
///
/// The grouping is **advisory**. The cohort hash is client-asserted and
/// unverifiable, so nothing here may drive an authorization decision — and an
/// absent or garbage hash must behave exactly like a valid one, which is why
/// ungrouped rows get a group of their own rather than being hidden or merged.
public struct DeviceCohortGroup: Sendable, Equatable, Hashable, Identifiable {
    /// The cohort hash, or `nil` for rows that reported none.
    public var cohortHash: String?
    public var devices: [DeviceRecord]
    public var sessions: [SessionRecord]

    public var id: String { cohortHash ?? "ungrouped" }

    public init(cohortHash: String?, devices: [DeviceRecord], sessions: [SessionRecord]) {
        self.cohortHash = cohortHash
        self.devices = devices
        self.sessions = sessions
    }

    /// The device to name the group after: the current device if it is here,
    /// else the most recently seen one.
    public var representativeDevice: DeviceRecord? {
        devices.first(where: \.isCurrent) ?? devices.max { $0.lastSeen < $1.lastSeen }
    }

    /// The most recent activity anywhere in the group.
    public var lastSeen: CapsuleTimestamp? {
        let deviceTimes = devices.map(\.lastSeen)
        let sessionTimes = sessions.map(\.lastUsedAt)
        return (deviceTimes + sessionTimes).max()
    }

    /// Whether this is the group the app is running in.
    public var containsCurrentDevice: Bool {
        devices.contains(where: \.isCurrent) || sessions.contains(where: \.isCurrent)
    }

    /// Whether this reads as "a device you've used before".
    ///
    /// More than one enrollment under one cohort is the reinstall signature. The
    /// client **asserts** this — "a device you've used before (last seen
    /// *date*)" — and offers no "this isn't my device" toggle, because a user
    /// cannot adjudicate a hash and the value is advisory anyway. The dispute
    /// path is a support report.
    public var isPreviouslySeen: Bool {
        devices.count > 1
    }

    /// Sessions that would still be honoured, newest use first.
    public func liveSessions(at now: CapsuleTimestamp) -> [SessionRecord] {
        sessions
            .filter { $0.isLive(at: now) }
            .sorted { $0.lastUsedAt > $1.lastUsedAt }
    }

    /// Group a ledger by cohort, newest activity first.
    ///
    /// Devices with no cohort and sessions with no cohort each land in the
    /// `nil` group rather than being dropped: a row the client cannot group is
    /// still a row the user needs to see.
    public static func group(
        devices: [DeviceRecord],
        sessions: [SessionRecord]
    ) -> [DeviceCohortGroup] {
        var hashes: [String?] = []
        for hash in devices.map(\.cohortHash) + sessions.map(\.cohortHash) where !hashes.contains(hash) {
            hashes.append(hash)
        }
        let groups = hashes.map { hash in
            DeviceCohortGroup(
                cohortHash: hash,
                devices: devices.filter { $0.cohortHash == hash },
                sessions: sessions.filter { $0.cohortHash == hash }
            )
        }
        return groups.sorted { left, right in
            guard let leftSeen = left.lastSeen else { return false }
            guard let rightSeen = right.lastSeen else { return true }
            return leftSeen > rightSeen
        }
    }
}
