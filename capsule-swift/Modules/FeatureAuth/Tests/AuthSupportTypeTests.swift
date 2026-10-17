import CapsuleDomain
import FeatureAuth
import Foundation
import Testing

// MARK: - RedactedSecretTests

/// The type that makes the wrong thing hard: a secret that can be shown and
/// cannot be kept.
@Suite("A redacted secret shows, compares one word, and never describes itself")
struct RedactedSecretTests {
    @Test("interpolating a secret cannot spill it into a log or a failure message")
    func descriptionsAreRedacted() {
        let secret = RedactedSecret("harbor-lantern-quartz")

        #expect(secret.description == "<redacted>")
        #expect(secret.debugDescription == "<redacted>")
        #expect("\(secret)" == "<redacted>")
        #expect(secret.reveal() == "harbor-lantern-quartz")
    }

    @Test("a phrase is split into words whichever separator the generator used")
    func wordsSplitOnEitherSeparator() {
        #expect(RedactedSecret("harbor-lantern-quartz").words == ["harbor", "lantern", "quartz"])
        #expect(RedactedSecret("harbor lantern quartz").words == ["harbor", "lantern", "quartz"])
        #expect(RedactedSecret("").words.isEmpty)
        #expect(RedactedSecret("harbor-lantern").characterCount == 14)
    }

    @Test("the one sanctioned comparison answers one bit about one word")
    func matchingIsPerWordAndForgiving() {
        let secret = RedactedSecret("harbor-lantern-quartz")

        #expect(secret.matches("lantern", atWordIndex: 1))
        #expect(secret.matches("  LANTERN  ", atWordIndex: 1))
        #expect(!secret.matches("lantern", atWordIndex: 0))
        #expect(!secret.matches("lanterns", atWordIndex: 1))
        #expect(!secret.matches("harbor", atWordIndex: 9), "an out-of-range position can never match")
        #expect(!secret.matches("", atWordIndex: 1))
    }
}

// MARK: - ChunkedCodeFormatterTests

/// One formatter, so two devices comparing a code cannot render it differently.
@Suite("Codes are chunked identically wherever they are drawn")
struct ChunkedCodeFormatterTests {
    @Test("the same value from two sources renders byte-identically")
    func incomingGroupingIsNormalisedAway() {
        let fromJSON = ChunkedCodeFormatter.chunked("a1b2c3d4e5f6")
        let fromTranscript = ChunkedCodeFormatter.chunked("A1B2-C3D4 E5F6")

        #expect(fromJSON == "A1B2 C3D4 E5F6")
        #expect(fromJSON == fromTranscript)
    }

    @Test("a trailing partial group is kept rather than dropped")
    func partialGroupsSurvive() {
        #expect(ChunkedCodeFormatter.chunked("A1B2C") == "A1B2 C")
        #expect(ChunkedCodeFormatter.chunked("").isEmpty)
        #expect(ChunkedCodeFormatter.groupSize == 4)
    }

    @Test("the group size is a parameter, for the digit fallback read aloud")
    func groupSizeIsConfigurable() {
        #expect(ChunkedCodeFormatter.chunked("123456789", groupSize: 3) == "123 456 789")
        #expect(ChunkedCodeFormatter.chunked("123456789", groupSize: 0) == "123456789")
    }

    @Test("a shortened fingerprint keeps whole groups and never over-reads")
    func shorteningKeepsWholeGroups() {
        #expect(ChunkedCodeFormatter.shortened("A1B2C3D4E5F6", groups: 2) == "A1B2 C3D4")
        #expect(ChunkedCodeFormatter.shortened("A1B2C3D4E5F6", groups: 9) == "A1B2 C3D4 E5F6")
        #expect(ChunkedCodeFormatter.shortened("A1B2C3D4", groups: 0).isEmpty)
        #expect(ChunkedCodeFormatter.shortened("A1B2C3D4", groups: -1).isEmpty)
    }
}

// MARK: - AuthPresentableErrorTests

/// Presentation follows the documented recovery matrix rather than a network
/// taxonomy this layer would have to invent.
@Suite("A failure is classified by its documented recovery, not by guesswork")
struct AuthPresentableErrorTests {
    private struct NotACapsuleError: Error {}

    @Test(
        "each code presents as its recovery action says it should",
        arguments: [
            (code: ErrorCode.authRateLimited, kind: AuthErrorKind.temporarilyUnavailable),
            (code: .syncCursorInvalid, kind: .temporarilyUnavailable),
            (code: .protocolVersionUnsupported, kind: .upgradeRequired),
            (code: .authInvalidCredentials, kind: .actionable),
            (code: .enrollmentLocalAuthRequired, kind: .actionable),
            (code: .authRevokeProofRequired, kind: .actionable),
            (code: .uploadMalformedRequest, kind: .defect),
        ]
    )
    func classificationFollowsTheRecoveryMatrix(sample: (code: ErrorCode, kind: AuthErrorKind)) {
        let error = AuthPresentableError(CapsuleError(code: sample.code, detail: "engineering detail"))

        #expect(error.kind == sample.kind)
        #expect(error.messageKey == sample.code.rawValue, "the catalog key is the code, verbatim")
        #expect(error.diagnosticDetail == "engineering detail")
        #expect(error.isRetryable == (sample.kind == .temporarilyUnavailable))
        #expect(error.isOffline == (sample.kind == .temporarilyUnavailable))
    }

    @Test("anything that is not a CapsuleError is reported as this client's defect")
    func foreignErrorsAreDefects() {
        let error = AuthPresentableError(NotACapsuleError())

        #expect(error.kind == .defect)
        #expect(error.messageKey == "error.client.unexpected")
        #expect(!error.isRetryable, "telling the user to check their connection would send them to fix the wrong thing")
        #expect(error.diagnosticDetail == nil)
    }

    @Test("every message key is a catalog key, never display text")
    func messageKeysAreCatalogKeys() {
        for code in [ErrorCode.authRateLimited, .escrowMalformed, .enrollmentCodeRefused] {
            let error = AuthPresentableError(CapsuleError(code: code))
            #expect(error.messageKey.hasPrefix("error."))
            #expect(!error.messageKey.contains(" "))
        }
    }
}

// MARK: - ScreenStateTests

/// One closed value rather than a pile of booleans, because
/// `isLoading && error != nil` is a state no screen has copy for.
@Suite("A screen is in exactly one of five states")
struct ScreenStateTests {
    @Test("loading, empty, and failed are distinguishable from one another")
    func statesDoNotOverlap() {
        let offline = ScreenState.failed(AuthPresentableError(CapsuleError(code: .authRateLimited)))
        let actionable = ScreenState.failed(AuthPresentableError(CapsuleError(code: .authInvalidCredentials)))

        #expect(ScreenState.loading.isLoading)
        #expect(!ScreenState.empty.isLoading)
        #expect(ScreenState.ready.failure == nil)
        #expect(ScreenState.empty.failure == nil)
        #expect(offline.isOffline, "unreachable is a failure kind, not a fifth case")
        #expect(!actionable.isOffline)
        #expect(actionable.failure?.code == .authInvalidCredentials)
        #expect(!ScreenState.idle.isOffline)
    }
}

// MARK: - SecretPasteboardTests

/// The pasteboard write itself is a system service and is deliberately not
/// exercised here — a unit test that clobbered the machine's clipboard would be
/// touching real state to prove a constant. What is assertable is the policy the
/// type publishes: a copied secret survives for a short, bounded window.
@Suite("A copied secret is short-lived by policy")
struct SecretPasteboardTests {
    @Test("the expiry is a real window, and a short one")
    func lifetimeIsShortAndBounded() {
        #expect(SecretPasteboard.lifetime > 0, "a secret with no expiry is a secret that stays")
        #expect(SecretPasteboard.lifetime <= 300, "the window is minutes, not a session")
        #expect(SecretPasteboard.lifetime == 120)
    }
}
