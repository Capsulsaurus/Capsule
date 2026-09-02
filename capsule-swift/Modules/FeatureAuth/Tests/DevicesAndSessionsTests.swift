import CapsuleDomain
import FeatureAuth
import Foundation
import Testing

// MARK: - RevocationScopeTests

/// The asymmetry is the security property, so it is stated as a value rather
/// than implied by which button sits where.
@Suite("Revoking one session and revoking all are deliberately different")
struct RevocationScopeTests {
    @Test("only the account-wide revocation needs proof of master-key possession")
    func onlyRevokeAllNeedsTheProof() {
        #expect(RevocationScope.singleSession.requiresMasterKeyProof == false)
        #expect(RevocationScope.allSessions.requiresMasterKeyProof == true)
    }
}

// MARK: - DevicesAndSessionsTests

/// The session ledger screen.
@Suite("The session ledger groups by cohort and gates the nuclear option")
@MainActor
struct DevicesAndSessionsTests {
    private static let phone = "cohort-phone"
    private static let laptop = "cohort-laptop"

    private static func port(
        revokeAllFailure: CapsuleError? = nil,
        readFailure: CapsuleError? = nil
    ) -> StubDevicePort {
        StubDevicePort(
            devices: [
                LedgerFixture.device(ordinal: 0, cohort: phone, lastSeenDays: 0, isCurrent: true),
                LedgerFixture.device(ordinal: 1, cohort: phone, lastSeenDays: -30),
                LedgerFixture.device(ordinal: 2, cohort: laptop, lastSeenDays: -5),
            ],
            sessions: [
                LedgerFixture.session(ordinal: 0, cohort: phone, lastUsedDays: 0, isCurrent: true),
                LedgerFixture.session(ordinal: 2, cohort: laptop, lastUsedDays: -5),
            ],
            revokeAllFailure: revokeAllFailure,
            readFailure: readFailure
        )
    }

    private static func model(_ port: StubDevicePort) -> DevicesAndSessionsViewModel {
        DevicesAndSessionsViewModel(devices: port, now: AuthInstant.frozen)
    }

    @Test("loading groups the ledger by cohort, newest first")
    func loadGroupsTheLedger() async {
        let model = Self.model(Self.port())

        await model.load()

        #expect(model.state == .ready)
        #expect(model.groups.map(\.cohortHash) == [Self.phone, Self.laptop])
        #expect(model.groups.first?.devices.count == 2)
        #expect(model.currentInstant == AuthInstant.reference)
    }

    @Test("an account with no ledger rows loads as empty, not as ready")
    func emptyLedgerIsItsOwnState() async {
        let model = Self.model(StubDevicePort())

        await model.load()

        #expect(model.state == .empty)
        #expect(model.groups.isEmpty)
    }

    @Test("a ledger that cannot be read is a failure the screen can classify")
    func unreadableLedgerFails() async {
        let model = Self.model(Self.port(readFailure: CapsuleError(code: .syncUnauthenticated)))

        await model.load()

        #expect(model.state.failure?.code == .syncUnauthenticated)
        #expect(model.state.failure?.kind == .actionable)
        #expect(!model.needsMasterKeyProof)
    }

    @Test("the screen says which revocation needs the master-key ceremony")
    func theAsymmetryIsExposedToTheScreen() {
        let model = Self.model(Self.port())

        #expect(model.revokeAllRequiresMasterKeyProof)
        #expect(!model.revokeSessionRequiresMasterKeyProof)
    }

    /// A request without valid proof revokes **nothing at all**, so there is no
    /// partial success to clean up.
    @Test("a revoke-all without master-key proof revokes nothing and says why")
    func revokeAllWithoutProofChangesNothing() async {
        let port = Self.port(revokeAllFailure: CapsuleError(code: .authRevokeProofRequired))
        let model = Self.model(port)
        await model.load()

        await model.revokeAllSessions()

        #expect(model.needsMasterKeyProof)
        #expect(model.state.failure?.code == .authRevokeProofRequired)
        let live = await port.liveSessionIdentifiers
        #expect(live == [SessionID("session-0"), SessionID("session-2")])
        let attempts = await port.revokeAllAttempts
        #expect(attempts == 1)
    }

    @Test("an invalid proof is the same conversation as a missing one")
    func invalidProofIsAlsoAProofProblem() async {
        let port = Self.port(revokeAllFailure: CapsuleError(code: .authRevokeProofInvalid))
        let model = Self.model(port)
        await model.load()

        await model.revokeAllSessions()

        #expect(model.needsMasterKeyProof)
        let live = await port.liveSessionIdentifiers
        #expect(live.count == 2)
    }

    @Test("an ordinary failure is not mistaken for a missing master-key proof")
    func ordinaryFailuresAreNotProofProblems() async {
        let port = Self.port(revokeAllFailure: CapsuleError(code: .authRateLimited))
        let model = Self.model(port)
        await model.load()

        await model.revokeAllSessions()

        #expect(!model.needsMasterKeyProof)
        #expect(model.state.failure?.isRetryable == true)
    }

    /// "Log out everywhere" means everywhere: the proof is what authorises it,
    /// and this device's own session goes with the rest.
    @Test("a proven revoke-all ends every session, this device's included")
    func provenRevokeAllEndsEverySession() async {
        let port = Self.port()
        let model = Self.model(port)
        await model.load()

        await model.revokeAllSessions()

        let live = await port.liveSessionIdentifiers
        #expect(live.isEmpty)
        #expect(!model.needsMasterKeyProof)
    }

    @Test("revoking one session leaves the current session alone")
    func revokingOneSessionSparesTheOthers() async {
        let port = Self.port()
        let model = Self.model(port)
        await model.load()

        await model.revokeSession(SessionID("session-2"))

        let live = await port.liveSessionIdentifiers
        #expect(live == [SessionID("session-0")])
        #expect(model.state == .ready)
    }

    @Test("a revoked device is marked, never removed from the directory")
    func revokedDevicesStayListed() async {
        let port = Self.port()
        let model = Self.model(port)
        await model.load()

        await model.revokeDevice(DeviceID("device-2"))

        let rowCount = await port.deviceRowCount
        let revoked = await port.revokedDeviceIdentifiers
        #expect(rowCount == 3, "the row must survive so its signatures stay verifiable")
        #expect(revoked == [DeviceID("device-2")])
        let laptop = model.groups.first { $0.cohortHash == Self.laptop }
        #expect(laptop?.devices.first?.isActive == false)
    }

    @Test("the dispute path is a support bundle, and it can be dismissed")
    func supportBundleIsTheDisputePath() async {
        let model = Self.model(Self.port())
        await model.load()

        await model.buildSupportBundle(for: Self.phone)

        #expect(model.supportBundle?.cohortHash == Self.phone)
        #expect(model.supportBundle?.deviceIDs.count == 2)

        model.dismissSupportBundle()

        #expect(model.supportBundle == nil)
    }
}
