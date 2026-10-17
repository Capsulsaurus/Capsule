import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - EnrollmentPort

extension MockIdentityStore: EnrollmentPort {
    /// Issue an enrollment code from an already-enrolled device.
    ///
    /// Requires **fresh local authentication**: a valid session token alone
    /// cannot start a cross-device add, so a stolen stale token cannot enroll a
    /// rogue device. The refusal is real here rather than a disabled button,
    /// because a disabled button is not a security control.
    public func issueEnrollmentCode() async throws -> EnrollmentCode {
        guard case .signedIn = currentState else {
            throw CapsuleError(
                code: .enrollmentLocalAuthRequired,
                detail: "CapsuleMock: fresh local authentication is required to issue a code"
            )
        }
        let ordinal = channels.count
        let handle = MockHash.hex(
            MockHash.value(seed: configuration.seed, index: ordinal, salt: .identity, sub: 991),
            digits: 16
        )
        openChannel(handle)
        return EnrollmentCode(
            code: Self.readableCode(seed: configuration.seed, ordinal: ordinal),
            expiresAt: configuration.clock.offset(seconds: 600),
            channelHandle: handle
        )
    }

    /// Redeem a code on the joining device.
    ///
    /// Every failure — unknown, already redeemed, expired, rate-limited — is
    /// reported **identically**, so redemption is not an oracle an attacker can
    /// probe for valid codes. The stream therefore fails with one code rather
    /// than a diagnosis.
    public nonisolated func redeem(code: String) -> AsyncStream<EnrollmentProgress> {
        AsyncStream { continuation in
            Task {
                guard await self.isRedeemable(code) else {
                    continuation.yield(.failed(.enrollmentCodeRefused))
                    continuation.finish()
                    return
                }
                continuation.yield(.exchangingKeys)
                continuation.yield(.publishingDirectory)
                let joined = await self.enrollJoiningDevice()
                continuation.yield(.completed(joined))
                continuation.finish()
            }
        }
    }

    /// Watch a ceremony from the issuing side.
    public nonisolated func observeEnrollment(channelHandle: String) -> AsyncStream<EnrollmentProgress> {
        AsyncStream { continuation in
            Task {
                guard await self.channels.contains(channelHandle) else {
                    continuation.yield(.failed(.enrollmentChannelNotFound))
                    continuation.finish()
                    return
                }
                let code = await self.issuedCode(for: channelHandle)
                continuation.yield(.awaitingRedemption(code))
                continuation.yield(.exchangingKeys)
                continuation.yield(.publishingDirectory)
                let joined = await self.enrollJoiningDevice()
                continuation.yield(.completed(joined))
                continuation.finish()
            }
        }
    }

    public func cancelEnrollment(channelHandle: String) async throws {
        closeChannel(channelHandle)
    }

    // MARK: Ceremony

    /// A code is redeemable when it is well-formed and a channel is open.
    /// Everything else is one indistinguishable refusal.
    private func isRedeemable(_ code: String) -> Bool {
        !channels.isEmpty && code.count >= 8
    }

    private func issuedCode(for channelHandle: String) -> EnrollmentCode {
        EnrollmentCode(
            code: Self.readableCode(seed: configuration.seed, ordinal: channelHandle.utf8.count),
            expiresAt: configuration.clock.offset(seconds: 600),
            channelHandle: channelHandle
        )
    }

    /// Add the joining device to the directory and publish it.
    private func enrollJoiningDevice() async -> DeviceID {
        let ordinal = deviceList.count
        let identifier = MockIdentifiers.deviceID(seed: configuration.seed, ordinal: ordinal)
        let joined = DeviceRecord(
            id: identifier,
            model: "Mac16,7",
            platform: .macos,
            firstSeen: configuration.clock.now,
            lastSeen: configuration.clock.now,
            cohortHash: MockIdentifiers.cohortHash(seed: configuration.seed, ordinal: ordinal)
        )
        setDevices(deviceList + [joined])
        await directoryChanges.send(())
        return identifier
    }

    /// A code a person can read out loud. A secret for its short lifetime, so
    /// nothing may log it — which is why it is derived rather than stored.
    private static func readableCode(seed: UInt64, ordinal: Int) -> String {
        let alphabet = Array("ABCDEFGHJKLMNPQRSTUVWXYZ23456789")
        let hash = MockHash.value(seed: seed, index: ordinal, salt: .identity, sub: 7717)
        let digits = (0 ..< 9).map { position -> Character in
            let shifted = MockHash.mix(hash &+ UInt64(position))
            return alphabet[Int(shifted % UInt64(alphabet.count))]
        }
        return String(digits[0 ..< 3]) + "-" + String(digits[3 ..< 6]) + "-" + String(digits[6 ..< 9])
    }
}
