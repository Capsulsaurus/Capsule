import CapsuleDomain
import CapsulePorts
import FeatureAuth
import Foundation
import Synchronization

// MARK: - ScriptedEnrollmentCeremony

/// A ``FirstDeviceEnrollmentPort`` that emits exactly the events it is given.
///
/// `PreviewEnrollmentCeremony` walks all six stages and is the right double for
/// the documented outcomes. This one exists for the outcome that is *not*
/// documented and must therefore be impossible: a run that leaves a stage
/// un-reported. Only a scripted port can produce it.
actor ScriptedEnrollmentCeremony: FirstDeviceEnrollmentPort {
    private let script: [EnrollmentStageEvent]
    private let availability: HardwareKeyAvailability
    private(set) var cancelCount = 0

    init(script: [EnrollmentStageEvent], availability: HardwareKeyAvailability = .secureElement) {
        self.script = script
        self.availability = availability
    }

    func hardwareKeyAvailability() async -> HardwareKeyAvailability {
        availability
    }

    nonisolated func run(allowingSoftwareKeys _: Bool) -> AsyncStream<EnrollmentStageEvent> {
        AsyncStream { continuation in
            Task {
                for event in await self.events {
                    continuation.yield(event)
                }
                continuation.finish()
            }
        }
    }

    func cancel() async {
        cancelCount += 1
    }

    private var events: [EnrollmentStageEvent] { script }
}

// MARK: - StubEnrollmentPort

/// An ``EnrollmentPort`` whose relay is driven by the test.
///
/// The cross-device ceremony's whole risk is ordering — a far side that
/// finishes before the human has compared anything — so the progress stream has
/// to be crankable one event at a time rather than replayed at whatever speed
/// an actor happens to run at.
final class StubEnrollmentPort: EnrollmentPort, Sendable {
    let relay = EventRelay<EnrollmentProgress>()

    /// The channel every issued code runs over.
    static let channel = "channel-fixture"

    private let issueFailure: CapsuleError?
    private let cancelledChannels = Mutex<[String]>([])

    init(issueFailure: CapsuleError? = nil) {
        self.issueFailure = issueFailure
    }

    var cancelledChannelHandles: [String] {
        cancelledChannels.withLock { $0 }
    }

    func issueEnrollmentCode() async throws -> EnrollmentCode {
        if let issueFailure { throw issueFailure }
        return EnrollmentCode(
            code: "ABC-DEF-GHJ",
            expiresAt: AuthInstant.seconds(600),
            channelHandle: Self.channel
        )
    }

    func redeem(code _: String) -> AsyncStream<EnrollmentProgress> {
        relay.stream()
    }

    func observeEnrollment(channelHandle _: String) -> AsyncStream<EnrollmentProgress> {
        relay.stream()
    }

    func cancelEnrollment(channelHandle: String) async throws {
        cancelledChannels.withLock { $0.append(channelHandle) }
    }
}

// MARK: - StubDevicePort

/// A ``DevicePort`` over a fixed ledger, with a refusal that revokes nothing.
///
/// The all-or-nothing refusal is the point: *Authentication — Explicit
/// Revocation* says a `revoke_all` without valid master-key proof revokes
/// **nothing at all**, so a double that half-applied it would let a broken view
/// model pass.
actor StubDevicePort: DevicePort {
    private var deviceRows: [DeviceRecord]
    private var sessionRows: [SessionRecord]
    private let revokeAllFailure: CapsuleError?
    private let readFailure: CapsuleError?
    private(set) var revokeAllAttempts = 0

    init(
        devices: [DeviceRecord] = [],
        sessions: [SessionRecord] = [],
        revokeAllFailure: CapsuleError? = nil,
        readFailure: CapsuleError? = nil
    ) {
        deviceRows = devices
        sessionRows = sessions
        self.revokeAllFailure = revokeAllFailure
        self.readFailure = readFailure
    }

    /// The ledger as it stands, for asserting what a revocation did and did not
    /// touch.
    var liveSessionIdentifiers: [SessionID] {
        sessionRows.filter { $0.revokedAt == nil }.map(\.id)
    }

    var revokedDeviceIdentifiers: [DeviceID] {
        deviceRows.filter { $0.revokedAt != nil }.map(\.id)
    }

    var deviceRowCount: Int { deviceRows.count }

    func devices() async throws -> [DeviceRecord] {
        if let readFailure { throw readFailure }
        return deviceRows
    }

    func sessions() async throws -> [SessionRecord] {
        if let readFailure { throw readFailure }
        return sessionRows
    }

    func cohorts() async throws -> [DeviceCohort] { [] }

    func revokeSession(_ identifier: SessionID) async throws {
        sessionRows = sessionRows.map { row in
            guard row.id == identifier, row.revokedAt == nil else { return row }
            var revoked = row
            revoked.revokedAt = AuthInstant.reference
            return revoked
        }
    }

    func revokeAllSessions() async throws {
        revokeAllAttempts += 1
        if let revokeAllFailure { throw revokeAllFailure }
        sessionRows = sessionRows.map { row in
            guard row.revokedAt == nil else { return row }
            var revoked = row
            revoked.revokedAt = AuthInstant.reference
            return revoked
        }
    }

    func revokeDevice(_ identifier: DeviceID) async throws {
        deviceRows = deviceRows.map { row in
            guard row.id == identifier, row.revokedAt == nil else { return row }
            var revoked = row
            revoked.revokedAt = AuthInstant.reference
            return revoked
        }
    }

    func supportBundle(for cohortHash: String) async throws -> DeviceCohort {
        DeviceCohort(
            cohortHash: cohortHash,
            firstSeen: AuthInstant.days(-400),
            lastSeen: AuthInstant.reference,
            deviceIDs: deviceRows.filter { $0.cohortHash == cohortHash }.map(\.id)
        )
    }

    /// A stream that finishes immediately, so the view model's observation task
    /// ends instead of racing the assertions.
    nonisolated func changes() -> AsyncStream<Void> {
        AsyncStream { $0.finish() }
    }
}
