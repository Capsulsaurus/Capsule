import CapsuleDomain
import FeatureAuth
import Foundation
import Testing

// MARK: - PasskeyEnrollTests

/// A passkey replaces the *password* in the login ceremony. It is not a second
/// copy of the master key, and a half-finished ceremony is not an enrolment.
@Suite("Passkey enrolment succeeds, fails, or is cancelled — never half-done")
@MainActor
struct PasskeyEnrollTests {
    @Test("a device with an authenticator is ready to enrol")
    func availableDeviceIsReady() async {
        let model = PasskeyEnrollViewModel(secondFactor: StubSecondFactorPort())

        await model.load()

        #expect(model.state == .ready)
        #expect(model.isAvailable)
        #expect(model.canEnroll)
    }

    /// A device with no authenticator is told so, rather than shown a button
    /// that fails when tapped.
    @Test("a device with no authenticator is told so instead of offered a button")
    func unavailableDeviceIsEmpty() async {
        let port = StubSecondFactorPort(passkeysAvailable: false)
        let model = PasskeyEnrollViewModel(secondFactor: port)

        await model.load()
        await model.enroll()

        #expect(model.state == .empty)
        #expect(!model.isAvailable)
        #expect(!model.canEnroll)
        #expect(model.registration == nil)
        let attempts = await port.enrolledDisplayNames
        #expect(attempts.isEmpty, "an unavailable authenticator must not be asked")
    }

    @Test("a successful enrolment records the credential and closes the offer")
    func successfulEnrolmentRecordsTheCredential() async {
        let port = StubSecondFactorPort()
        let model = PasskeyEnrollViewModel(secondFactor: port, defaultDisplayName: "Avery's iPhone")
        await model.load()

        await model.enroll()

        #expect(model.registration?.id == "credential-1")
        #expect(model.registration?.authenticatorLabel == "Avery's iPhone")
        #expect(model.state == .ready)
        #expect(!model.isEnrolling)
        #expect(!model.canEnroll, "an enrolled credential is not enrolled twice")
        let names = await port.enrolledDisplayNames
        #expect(names == ["Avery's iPhone"])
    }

    @Test("a cancelled system ceremony is surfaced and is never mistaken for an enrolment")
    func cancelledCeremonyIsNotAnEnrolment() async {
        let cancelled = CapsuleError(code: .authInvalidCredentials, detail: "stub: the user dismissed the sheet")
        let model = PasskeyEnrollViewModel(secondFactor: StubSecondFactorPort(passkeyFailure: cancelled))
        await model.load()

        await model.enroll()

        #expect(model.registration == nil, "a dismissed sheet must not register a factor")
        #expect(model.state.failure?.code == .authInvalidCredentials)
        #expect(model.state.failure?.kind == .actionable)
        #expect(!model.isEnrolling)
        #expect(model.canEnroll, "the user may try again")
    }

    @Test("the label the user typed is what the credential is named")
    func labelIsPassedThrough() async {
        let port = StubSecondFactorPort()
        let model = PasskeyEnrollViewModel(secondFactor: port)
        await model.load()
        model.displayNameInput = "Work laptop"

        await model.enroll()

        let names = await port.enrolledDisplayNames
        #expect(names == ["Work laptop"])
    }
}

// MARK: - TotpEnrollTests

/// Nothing is armed until the user has proved they transcribed the seed.
@Suite("A TOTP factor is armed only after the code is confirmed")
@MainActor
struct TotpEnrollTests {
    private static func started(_ port: StubSecondFactorPort) async -> TotpEnrollViewModel {
        let model = TotpEnrollViewModel(secondFactor: port)
        await model.begin()
        return model
    }

    @Test("beginning mints a seed and shows what an authenticator will display")
    func beginMintsASeed() async {
        let model = await Self.started(StubSecondFactorPort())

        #expect(model.state == .ready)
        #expect(model.issuer == "Capsule")
        #expect(model.accountLabel == "avery@capsule.example")
        #expect(model.provisioningURIForQRCode()?.hasPrefix("otpauth://totp/") == true)
        #expect(!model.isConfirmed)
    }

    @Test("the manual seed stays hidden until a deliberate tap")
    func seedStaysHiddenUntilAsked() async {
        let model = await Self.started(StubSecondFactorPort())

        #expect(model.seedDisplay.isEmpty, "a shoulder-surfer should need more than a glance")

        model.revealSeed()

        #expect(model.seedDisplay == ChunkedCodeFormatter.chunked("JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP"))
        #expect(model.seedDisplay.contains(" "))
    }

    @Test("a mint that fails leaves nothing to transcribe")
    func failedMintShowsNothing() async {
        let port = StubSecondFactorPort(beginFailure: CapsuleError(code: .authRateLimited))
        let model = await Self.started(port)

        #expect(model.state.failure?.code == .authRateLimited)
        #expect(model.provisioningURIForQRCode() == nil)
        #expect(!model.canConfirm)
    }

    @Test(
        "confirm is offered only for a full-length code",
        arguments: ["", "1", "12345", "1234567"]
    )
    func confirmNeedsSixDigits(code: String) async {
        let model = await Self.started(StubSecondFactorPort())

        model.codeInput = code

        #expect(TotpEnrollViewModel.codeLength == 6)
        #expect(!model.canConfirm)
    }

    @Test("a wrong code is rejected, the factor stays unarmed, and the entry is forgotten")
    func wrongCodeDoesNotArmTheFactor() async {
        let port = StubSecondFactorPort()
        let model = await Self.started(port)
        model.codeInput = "000000"
        #expect(model.canConfirm)

        await model.confirm()

        #expect(!model.isConfirmed)
        #expect(model.isCodeRejected)
        #expect(!model.isRateLimited)
        #expect(model.codeInput.isEmpty, "the typed code must not linger")
        let attempted = await port.confirmedCodes
        #expect(attempted == ["000000"])
    }

    @Test("a rate-limited confirmation is its own state, not a wrong code")
    func rateLimitIsDistinctFromRejection() async {
        let port = StubSecondFactorPort(confirmFailure: CapsuleError(code: .authRateLimited))
        let model = await Self.started(port)
        model.codeInput = StubSecondFactorPort.acceptedCode

        await model.confirm()

        #expect(model.isRateLimited)
        #expect(!model.isCodeRejected)
        #expect(!model.isConfirmed)
    }

    @Test("a correct code arms the factor and drops the seed")
    func correctCodeArmsTheFactor() async {
        let model = await Self.started(StubSecondFactorPort())
        model.revealSeed()
        model.codeInput = StubSecondFactorPort.acceptedCode

        await model.confirm()

        #expect(model.isConfirmed)
        #expect(model.state == .ready)
        #expect(model.codeInput.isEmpty)
        #expect(model.seedDisplay.isEmpty, "the seed is gone when the screen is done with it")
        #expect(model.provisioningURIForQRCode() == nil)
        #expect(!model.canConfirm, "a confirmed factor cannot be confirmed again")
    }

    @Test("a wrong code can be followed by the right one")
    func retryAfterRejectionStillArms() async {
        let model = await Self.started(StubSecondFactorPort())
        model.codeInput = "000000"
        await model.confirm()
        #expect(!model.isConfirmed)

        model.codeInput = StubSecondFactorPort.acceptedCode
        await model.confirm()

        #expect(model.isConfirmed)
    }
}
