import CapsuleDomain
import CapsuleMock
import FeatureAuth
import Foundation
import Testing

// MARK: - CrossDeviceAddTests

/// The cross-device add, from the issuing device.
///
/// The code is not the security — the safety-code comparison is — so the tests
/// that matter are about ordering: nothing may advance on its own, and a far
/// side that finishes early must not skip the human past the check.
@Suite("A cross-device add advances only when the human says so")
@MainActor
struct CrossDeviceAddTests {
    private static let joined = DeviceID("device-joined")

    private struct Harness {
        let enrollment: StubEnrollmentPort
        let ceremony: PreviewCrossDeviceCeremony
        let model: CrossDeviceAddViewModel
    }

    private static func harness(
        issueFailure: CapsuleError? = nil,
        safetyCodesDiverge: Bool = false
    ) -> Harness {
        let enrollment = StubEnrollmentPort(issueFailure: issueFailure)
        let ceremony = PreviewCrossDeviceCeremony(
            environment: MockEnvironment(),
            producesMismatch: safetyCodesDiverge
        )
        return Harness(
            enrollment: enrollment,
            ceremony: ceremony,
            model: CrossDeviceAddViewModel(
                enrollment: enrollment,
                ceremony: ceremony,
                now: AuthInstant.frozen
            )
        )
    }

    @Test("issuing a code shows both presentations of it, and waits")
    func issuingShowsBothPresentations() async {
        let harness = Self.harness()

        await harness.model.issueCode()

        #expect(harness.model.step == .awaitingRedemption)
        #expect(harness.model.isInviteLive)
        #expect(harness.model.qrPayload()?.hasPrefix("capsule://enroll?") == true)
        #expect(harness.model.textFallbackDisplay.split(separator: " ").count == 3)
        #expect(harness.model.safetyCodeDisplay.isEmpty, "there is nothing to compare until the far side redeems")
        #expect(!harness.model.canConfirm)
    }

    @Test("a code that needs fresh local authentication is refused, not merely greyed out")
    func staleTokenCannotIssueACode() async {
        let harness = Self.harness(issueFailure: CapsuleError(code: .enrollmentLocalAuthRequired))

        await harness.model.issueCode()

        #expect(harness.model.needsFreshLocalAuth)
        guard case let .failed(error) = harness.model.step else {
            Issue.record("a refused issue must land in the failed step")
            return
        }
        #expect(error.code == .enrollmentLocalAuthRequired)
        #expect(error.kind == .actionable, "re-authenticating is something the user does, not a retry")
        #expect(harness.model.invite == nil)
        #expect(!harness.model.isInviteLive)
    }

    @Test("the safety code is derived from the channel and renders identically every time")
    func safetyCodeDerivationIsStable() async throws {
        let harness = Self.harness()
        await harness.model.issueCode()
        harness.enrollment.relay.emit(.exchangingKeys)
        try await waitUntil("the safety check is on screen") {
            harness.model.step == .verifyingSafetyCode
        }

        let displayed = harness.model.safetyCodeDisplay
        let first = try await harness.ceremony.safetyCheck(channelHandle: StubEnrollmentPort.channel)
        let second = try await harness.ceremony.safetyCheck(channelHandle: StubEnrollmentPort.channel)

        #expect(!displayed.isEmpty)
        #expect(displayed == ChunkedCodeFormatter.chunked(first.safetyCode.reveal()))
        #expect(first.safetyCode.reveal() == second.safetyCode.reveal(), "the code must not move under the user")
        #expect(displayed == displayed.uppercased())
        #expect(displayed.split(separator: " ").allSatisfy { $0.count == 4 })
    }

    @Test("each side's identity is shown beside the code, because digits alone miss a swap")
    func bothIdentitiesAreAvailable() async throws {
        let harness = Self.harness()
        await harness.model.issueCode()
        harness.enrollment.relay.emit(.exchangingKeys)
        try await waitUntil("the safety check is on screen") {
            harness.model.step == .verifyingSafetyCode
        }

        let check = harness.model.safetyCheck
        #expect(check?.localDevice.keyFingerprint.isEmpty == false)
        #expect(check?.remoteDevice.keyFingerprint.isEmpty == false)
        #expect(check?.localDevice.keyFingerprint != check?.remoteDevice.keyFingerprint)
        #expect(check?.remoteDevice.model == "Mac16,7")
    }

    @Test("a relayed code that diverges renders differently, which is the whole point")
    func mitmProducesADifferentCode() async throws {
        let honest = try await Self.harness().ceremony.safetyCheck(channelHandle: StubEnrollmentPort.channel)
        let relayed = try await Self.harness(safetyCodesDiverge: true)
            .ceremony.safetyCheck(channelHandle: StubEnrollmentPort.channel)

        #expect(honest.safetyCode.reveal() != relayed.safetyCode.reveal())
    }

    /// The load-bearing ordering test: the relay can finish before the user has
    /// compared anything, and reporting success then would teach them the check
    /// is decorative.
    @Test("a far side that finishes first does not skip the safety check")
    func fastFarSideCannotSkipTheCheck() async throws {
        let harness = Self.harness()
        await harness.model.issueCode()
        harness.enrollment.relay.emit(.exchangingKeys)
        try await waitUntil("the safety check is on screen") {
            harness.model.step == .verifyingSafetyCode
        }

        harness.enrollment.relay.emit(.completed(Self.joined))
        harness.enrollment.relay.finish()

        try await holdsThroughout("the safety check stays on screen") {
            harness.model.step == .verifyingSafetyCode
        }
        #expect(!harness.model.canConfirm, "the acknowledgement has not been given")
        let confirmedEarly = await harness.ceremony.isConfirmed(channelHandle: StubEnrollmentPort.channel)
        #expect(!confirmedEarly)

        // The completion was held, not lost: acknowledging releases it.
        harness.model.hasAcknowledgedMatch = true
        await harness.model.confirmMatch()

        #expect(harness.model.step == .completed(Self.joined))
        let confirmed = await harness.ceremony.isConfirmed(channelHandle: StubEnrollmentPort.channel)
        #expect(confirmed)
    }

    @Test("acknowledging before the far side finishes shows the transfer, then completes")
    func acknowledgementBeforeCompletionTransfersFirst() async throws {
        let harness = Self.harness()
        await harness.model.issueCode()
        harness.enrollment.relay.emit(.exchangingKeys)
        try await waitUntil("the safety check is on screen") {
            harness.model.step == .verifyingSafetyCode
        }

        harness.model.hasAcknowledgedMatch = true
        await harness.model.confirmMatch()
        #expect(harness.model.step == .transferringKeys)

        harness.enrollment.relay.emit(.completed(Self.joined))
        try await waitUntil("the ceremony completes") {
            harness.model.step == .completed(Self.joined)
        }
    }

    @Test("confirming without the acknowledgement does nothing at all")
    func confirmingWithoutAcknowledgementIsANoOp() async throws {
        let harness = Self.harness()
        await harness.model.issueCode()
        harness.enrollment.relay.emit(.exchangingKeys)
        try await waitUntil("the safety check is on screen") {
            harness.model.step == .verifyingSafetyCode
        }

        await harness.model.confirmMatch()

        let confirmed = await harness.ceremony.isConfirmed(channelHandle: StubEnrollmentPort.channel)
        #expect(!confirmed, "the port must never be told the user acknowledged when they did not")
        #expect(harness.model.step == .verifyingSafetyCode)
    }

    @Test("aborting on a mismatch invalidates the channel and the code")
    func abortOnMismatchInvalidatesEverything() async throws {
        let harness = Self.harness(safetyCodesDiverge: true)
        await harness.model.issueCode()
        harness.enrollment.relay.emit(.exchangingKeys)
        try await waitUntil("the safety check is on screen") {
            harness.model.step == .verifyingSafetyCode
        }

        await harness.model.abortOnMismatch()

        #expect(harness.model.step == .abortedOnMismatch)
        #expect(harness.model.invite == nil)
        #expect(harness.model.safetyCheck == nil)
        #expect(!harness.model.hasAcknowledgedMatch)
        let aborted = await harness.ceremony.isAborted(channelHandle: StubEnrollmentPort.channel)
        #expect(aborted)
        #expect(harness.enrollment.cancelledChannelHandles == [StubEnrollmentPort.channel])
    }

    @Test("an abort is terminal: a late completion cannot undo it")
    func abortIsTerminal() async throws {
        let harness = Self.harness(safetyCodesDiverge: true)
        await harness.model.issueCode()
        harness.enrollment.relay.emit(.exchangingKeys)
        try await waitUntil("the safety check is on screen") {
            harness.model.step == .verifyingSafetyCode
        }
        await harness.model.abortOnMismatch()

        harness.enrollment.relay.emit(.completed(Self.joined))
        harness.enrollment.relay.finish()

        try await holdsThroughout("the aborted step stays aborted") {
            harness.model.step == .abortedOnMismatch
        }
    }

    @Test("walking away cancels the code without claiming a mismatch")
    func cancellingIsNotAMismatch() async {
        let harness = Self.harness()
        await harness.model.issueCode()

        await harness.model.cancel()

        #expect(harness.model.step == .idle)
        #expect(harness.model.invite == nil)
        #expect(harness.enrollment.cancelledChannelHandles == [StubEnrollmentPort.channel])
        let aborted = await harness.ceremony.isAborted(channelHandle: StubEnrollmentPort.channel)
        #expect(!aborted, "an abandoned ceremony is not a reported MITM")
    }

    @Test("an expired code stops being live against the injected clock")
    func expiredCodeIsNotLive() async {
        let enrollment = StubEnrollmentPort()
        let model = CrossDeviceAddViewModel(
            enrollment: enrollment,
            ceremony: PreviewCrossDeviceCeremony(environment: MockEnvironment()),
            now: { AuthInstant.seconds(601) }
        )

        await model.issueCode()

        #expect(!model.isInviteLive, "a ten-minute code is dead at ten minutes and one second")
    }
}
