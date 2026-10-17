import CapsuleDomain
import FeatureAuth
import Foundation
import Testing

// MARK: - DeviceCohortGroupTests

/// One phone accumulates several ledger rows over its life, because a reinstall
/// re-enrolls with a new device id by design. Grouping is what stops the ledger
/// presenting them as several strangers — and it is advisory, so a row the
/// client cannot group is still a row the user must see.
@Suite("The ledger groups by cohort and drops nothing")
struct DeviceCohortGroupTests {
    private static let phone = "cohort-phone"
    private static let laptop = "cohort-laptop"

    private static func ledger() -> (devices: [DeviceRecord], sessions: [SessionRecord]) {
        let devices = [
            LedgerFixture.device(ordinal: 0, cohort: phone, lastSeenDays: 0, isCurrent: true),
            LedgerFixture.device(ordinal: 1, cohort: phone, lastSeenDays: -30),
            LedgerFixture.device(ordinal: 2, cohort: laptop, lastSeenDays: -5),
            LedgerFixture.device(ordinal: 3, cohort: nil, lastSeenDays: -200),
        ]
        let sessions = [
            LedgerFixture.session(ordinal: 0, cohort: phone, lastUsedDays: 0, isCurrent: true),
            LedgerFixture.session(ordinal: 2, cohort: laptop, lastUsedDays: -5),
            LedgerFixture.session(ordinal: 3, cohort: nil, lastUsedDays: -200),
        ]
        return (devices, sessions)
    }

    @Test("every row lands in exactly one group, ungrouped rows included")
    func groupingLosesNothing() {
        let ledger = Self.ledger()

        let groups = DeviceCohortGroup.group(devices: ledger.devices, sessions: ledger.sessions)

        #expect(groups.count == 3)
        let devicesInGroups = groups.flatMap(\.devices)
        let sessionsInGroups = groups.flatMap(\.sessions)
        #expect(devicesInGroups.count == ledger.devices.count)
        #expect(sessionsInGroups.count == ledger.sessions.count)
        #expect(Set(groups.map(\.id)).count == groups.count)
    }

    @Test("a row with no cohort hash gets a group of its own rather than being hidden")
    func ungroupedRowsAreStillShown() {
        let ledger = Self.ledger()

        let groups = DeviceCohortGroup.group(devices: ledger.devices, sessions: ledger.sessions)
        let ungrouped = groups.first { $0.cohortHash == nil }

        #expect(ungrouped?.id == "ungrouped")
        #expect(ungrouped?.devices.count == 1)
        #expect(ungrouped?.sessions.count == 1)
        #expect(ungrouped?.isPreviouslySeen == false)
    }

    @Test("groups are ordered by most recent activity")
    func groupsAreOrderedByRecency() {
        let ledger = Self.ledger()

        let groups = DeviceCohortGroup.group(devices: ledger.devices, sessions: ledger.sessions)

        #expect(groups.map(\.cohortHash) == [Self.phone, Self.laptop, nil])
        #expect(groups.first?.lastSeen == AuthInstant.reference)
    }

    @Test("two enrollments under one cohort read as a device you have used before")
    func repeatedEnrollmentsReadAsOneDevice() {
        let ledger = Self.ledger()

        let groups = DeviceCohortGroup.group(devices: ledger.devices, sessions: ledger.sessions)
        let phone = groups.first { $0.cohortHash == Self.phone }
        let laptop = groups.first { $0.cohortHash == Self.laptop }

        #expect(phone?.devices.count == 2)
        #expect(phone?.isPreviouslySeen == true)
        #expect(laptop?.isPreviouslySeen == false, "one enrollment is not a reinstall")
    }

    @Test("the group is named after the current device where there is one")
    func currentDeviceNamesItsGroup() {
        let ledger = Self.ledger()

        let groups = DeviceCohortGroup.group(devices: ledger.devices, sessions: ledger.sessions)
        let phone = groups.first { $0.cohortHash == Self.phone }

        #expect(phone?.representativeDevice?.id == DeviceID("device-0"))
        #expect(phone?.containsCurrentDevice == true)
        #expect(groups.first { $0.cohortHash == Self.laptop }?.containsCurrentDevice == false)
    }

    @Test("without a current device the most recently seen one names the group")
    func mostRecentDeviceNamesTheGroupOtherwise() {
        let group = DeviceCohortGroup(
            cohortHash: "cohort-x",
            devices: [
                LedgerFixture.device(ordinal: 5, cohort: "cohort-x", lastSeenDays: -90),
                LedgerFixture.device(ordinal: 6, cohort: "cohort-x", lastSeenDays: -2),
            ],
            sessions: []
        )

        #expect(group.representativeDevice?.id == DeviceID("device-6"))
        #expect(!group.containsCurrentDevice)
    }

    @Test("a current session alone still marks the group as this device's")
    func currentSessionMarksTheGroup() {
        let group = DeviceCohortGroup(
            cohortHash: "cohort-y",
            devices: [LedgerFixture.device(ordinal: 7, cohort: "cohort-y", lastSeenDays: -1)],
            sessions: [LedgerFixture.session(ordinal: 7, cohort: "cohort-y", lastUsedDays: 0, isCurrent: true)]
        )

        #expect(group.containsCurrentDevice)
    }

    @Test("live sessions exclude the revoked and the lapsed, newest use first")
    func liveSessionsFilterAndSort() {
        let group = DeviceCohortGroup(
            cohortHash: "cohort-z",
            devices: [],
            sessions: [
                LedgerFixture.session(ordinal: 10, cohort: "cohort-z", lastUsedDays: -10),
                LedgerFixture.session(ordinal: 11, cohort: "cohort-z", lastUsedDays: -1),
                LedgerFixture.session(ordinal: 12, cohort: "cohort-z", lastUsedDays: -3, revokedDays: -2),
                LedgerFixture.session(
                    ordinal: 13,
                    cohort: "cohort-z",
                    lastUsedDays: -200,
                    inactivityDays: -20,
                    hardDays: 100
                ),
                LedgerFixture.session(
                    ordinal: 14,
                    cohort: "cohort-z",
                    lastUsedDays: -2,
                    inactivityDays: 100,
                    hardDays: -1
                ),
            ]
        )

        let live = group.liveSessions(at: AuthInstant.reference)

        #expect(live.map(\.id) == [SessionID("session-11"), SessionID("session-10")])
    }

    @Test("a revoked device stays in its group so its history stays verifiable")
    func revokedDevicesAreListedNotHidden() {
        let revoked = LedgerFixture.device(ordinal: 20, cohort: "cohort-w", lastSeenDays: -190, revokedDays: -188)

        let groups = DeviceCohortGroup.group(devices: [revoked], sessions: [])

        #expect(groups.first?.devices.count == 1)
        #expect(groups.first?.devices.first?.isActive == false)
    }

    @Test("an empty ledger groups into nothing rather than into an empty group")
    func emptyLedgerProducesNoGroups() {
        #expect(DeviceCohortGroup.group(devices: [], sessions: []).isEmpty)
    }
}
