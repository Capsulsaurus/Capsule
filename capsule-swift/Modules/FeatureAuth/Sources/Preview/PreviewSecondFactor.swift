import CapsuleDomain
import CapsuleMock
import Foundation

// MARK: - PreviewSecondFactor

/// A ``SecondFactorPort`` over ``MockEnvironment``.
///
/// The seed it mints is derived from the world seed so a preview renders the
/// same QR code every time, which is what makes a screenshot diff meaningful.
/// In the real client the seed comes from the CSPRNG, and a comment like this
/// one would be a bug report.
public actor PreviewSecondFactor: SecondFactorPort {
    /// The code the mock accepts. Any other six digits are rejected, so the
    /// wrong-code state is reachable in a preview.
    public static let acceptedCode = "123456"

    private let seed: UInt64
    private let clock: MockClock
    private let passkeysAvailable: Bool
    private var attemptCount = 0

    public init(environment: MockEnvironment, passkeysAvailable: Bool = true) {
        seed = environment.configuration.seed
        clock = environment.configuration.clock
        self.passkeysAvailable = passkeysAvailable
    }

    public func isPasskeyEnrollmentAvailable() async -> Bool {
        passkeysAvailable
    }

    public func enrollPasskey(displayName: String) async throws -> PasskeyRegistration {
        guard passkeysAvailable else {
            throw CapsuleError(code: .authInvalidCredentials, detail: "CapsuleMock: no authenticator")
        }
        let label = displayName.isEmpty ? "iCloud Keychain" : displayName
        return PasskeyRegistration(
            id: MockHash.hex(MockHash.value(seed: seed, index: 0, salt: .identity, sub: 4242), digits: 16),
            authenticatorLabel: label,
            createdAt: clock.now
        )
    }

    public func beginTotpEnrollment() async throws -> TotpEnrollment {
        let secret = Self.base32Seed(seed: seed)
        let issuer = "Capsule"
        let account = "avery@capsule.example"
        let uri = "otpauth://totp/\(issuer):\(account)?secret=\(secret)&issuer=\(issuer)&digits=6&period=30"
        return TotpEnrollment(
            seed: RedactedSecret(secret),
            provisioningURI: RedactedSecret(uri),
            accountLabel: account,
            issuer: issuer
        )
    }

    /// Fails the third attempt with a rate limit, so both refusal states are
    /// reachable without inventing a second mock.
    public func confirmTotp(code: String) async throws {
        attemptCount += 1
        if attemptCount >= 3, code != Self.acceptedCode {
            throw CapsuleError(code: .authRateLimited, detail: "CapsuleMock: too many attempts")
        }
        guard code == Self.acceptedCode else {
            throw CapsuleError(code: .authInvalidCredentials, detail: "CapsuleMock: wrong code")
        }
    }

    /// 32 base32 characters — 160 bits, the usual TOTP seed length.
    private static func base32Seed(seed: UInt64) -> String {
        let alphabet = Array("ABCDEFGHIJKLMNOPQRSTUVWXYZ234567")
        return String((0 ..< 32).map { position in
            let hash = MockHash.value(seed: seed, index: position, salt: .identity, sub: 8080)
            return MockHash.element(hash, from: alphabet) ?? "A"
        })
    }
}
