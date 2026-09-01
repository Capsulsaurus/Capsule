import CapsuleDomain
import CapsuleFoundation
import CapsuleMock
import Foundation

// MARK: - PreviewCrossDeviceCeremony

/// A ``CrossDeviceCeremonyPort`` over ``MockEnvironment``.
///
/// The `producesMismatch` switch is the important one. A MITM on the relay is
/// the threat the safety-code check exists for, and a mock that could only ever
/// produce matching codes would leave the abort path — the one path that must be
/// impossible to miss — unreachable in every preview and every UI test.
public actor PreviewCrossDeviceCeremony: CrossDeviceCeremonyPort {
    private let seed: UInt64
    private let clock: MockClock
    private let producesMismatch: Bool
    private var confirmedChannels: Set<String> = []
    private var abortedChannels: Set<String> = []

    public init(environment: MockEnvironment, producesMismatch: Bool = false) {
        seed = environment.configuration.seed
        clock = environment.configuration.clock
        self.producesMismatch = producesMismatch
    }

    public func invite(for code: EnrollmentCode) async throws -> CrossDeviceInvite {
        CrossDeviceInvite(
            // The QR carries the full-entropy payload; the digits are the
            // deliberately shorter fallback, which is safe only because the code
            // is single-use, short-lived, and rate-limited.
            qrPayload: RedactedSecret("capsule://enroll?c=\(code.code)&h=\(code.channelHandle)"),
            textFallback: RedactedSecret(Self.digits(seed: seed, channelHandle: code.channelHandle)),
            expiresAt: code.expiresAt,
            channelHandle: code.channelHandle
        )
    }

    public func safetyCheck(channelHandle: String) async throws -> SafetyCheck {
        guard !abortedChannels.contains(channelHandle) else {
            throw CapsuleError(code: .enrollmentChannelNotFound, detail: "CapsuleMock: channel aborted")
        }
        let transcript = Self.safetyCode(seed: seed, channelHandle: channelHandle)
        return SafetyCheck(
            safetyCode: RedactedSecret(producesMismatch ? Self.divergent(transcript) : transcript),
            localDevice: DeviceIdentity(
                model: PlatformEnvironment.hardwareModel,
                platform: PlatformTag(rawValue: PlatformEnvironment.platformTag),
                keyFingerprint: Self.fingerprint(seed: seed, ordinal: 0)
            ),
            remoteDevice: DeviceIdentity(
                model: "Mac16,7",
                platform: .macos,
                keyFingerprint: Self.fingerprint(seed: seed, ordinal: 1)
            )
        )
    }

    public func confirmSafetyCheck(channelHandle: String) async throws {
        guard !abortedChannels.contains(channelHandle) else {
            throw CapsuleError(code: .enrollmentChannelNotFound, detail: "CapsuleMock: channel aborted")
        }
        confirmedChannels.insert(channelHandle)
    }

    public func abortSafetyCheck(channelHandle: String) async throws {
        abortedChannels.insert(channelHandle)
        confirmedChannels.remove(channelHandle)
    }

    /// Whether the user acknowledged this channel — for a test that wants to
    /// prove nothing proceeds without the acknowledgement.
    public func isConfirmed(channelHandle: String) -> Bool {
        confirmedChannels.contains(channelHandle)
    }

    public func isAborted(channelHandle: String) -> Bool {
        abortedChannels.contains(channelHandle)
    }

    private static func digits(seed: UInt64, channelHandle: String) -> String {
        var hash = MockHash.mix(seed &+ UInt64(channelHandle.utf8.count))
        return String((0 ..< 9).map { _ in
            hash = MockHash.mix(hash)
            let digit = Int(hash % 10)
            return Character(String(digit))
        })
    }

    private static func safetyCode(seed: UInt64, channelHandle: String) -> String {
        var hash = MockHash.mix(seed ^ 0x9E37_79B9_7F4A_7C15)
        for byte in channelHandle.utf8 {
            hash = MockHash.mix(hash &+ UInt64(byte))
        }
        return MockHash.hex(hash, digits: 12)
    }

    /// One character different — the realistic shape of a MITM's divergence,
    /// and the reason chunked display matters.
    private static func divergent(_ code: String) -> String {
        guard let first = code.first else { return code }
        let replacement: Character = first == "0" ? "1" : "0"
        return String(replacement) + code.dropFirst()
    }

    private static func fingerprint(seed: UInt64, ordinal: Int) -> String {
        MockHash.hex(MockHash.value(seed: seed, index: ordinal, salt: .identity, sub: 3131), digits: 8)
    }
}
