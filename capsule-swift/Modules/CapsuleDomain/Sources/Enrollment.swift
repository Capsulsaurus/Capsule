import Foundation

// MARK: - EnrollmentCode

/// A short-lived code that lets a second device join the account
/// (*Device Enrollment*).
///
/// Issuing one requires **fresh local authentication**, not merely a valid
/// session token: a stolen, stale token must not be able to enroll a rogue
/// device. Redemption failures are all reported identically — unknown, already
/// redeemed, expired, rate-limited — so redemption is not an oracle.
public struct EnrollmentCode: Sendable, Equatable, Hashable {
    /// The code the user reads out or scans. A secret for its short lifetime;
    /// never log it.
    public var code: String
    /// When it stops working.
    public var expiresAt: CapsuleTimestamp
    /// The relay channel the two devices exchange opaque payloads over.
    public var channelHandle: String

    public init(code: String, expiresAt: CapsuleTimestamp, channelHandle: String) {
        self.code = code
        self.expiresAt = expiresAt
        self.channelHandle = channelHandle
    }

    /// Whether the code is still redeemable.
    public func isLive(at now: CapsuleTimestamp) -> Bool {
        now < expiresAt
    }
}

// MARK: - EnrollmentProgress

/// Where a cross-device enrollment stands.
///
/// Enrollment is a ceremony, so it follows ``RetryClass/controlCeremony``: it
/// backs off slowly and **never abandons silently**. A ceremony that gives up
/// quietly leaves a user staring at a device that will not join, with nothing to
/// act on.
public enum EnrollmentProgress: Sendable, Equatable, Hashable {
    /// A code has been issued and is waiting to be redeemed.
    case awaitingRedemption(EnrollmentCode)
    /// The new device redeemed the code; the two are exchanging key material
    /// over the relay. The server relays opaque bytes and inspects nothing.
    case exchangingKeys
    /// The new device is in the directory and the directory has been published.
    case publishingDirectory
    /// Done — the new device can decrypt.
    case completed(DeviceID)
    /// Failed, with a stable code.
    case failed(ErrorCode)

    /// Whether the ceremony has finished, either way.
    public var isFinished: Bool {
        switch self {
        case .completed, .failed: true
        case .awaitingRedemption, .exchangingKeys, .publishingDirectory: false
        }
    }
}

// MARK: - RecoveryVerificationOutcome

/// The result of a local recovery-secret check
/// (*Backup & Recovery — Local Verification*).
///
/// The check is **local-only**: the client unwraps a cached escrow blob and
/// compares a derived tag against one derived from the device-held master key.
/// No server round-trip, so it works offline, creates no guessing surface, and
/// a failure cannot lock anything.
public enum RecoveryVerificationOutcome: Sendable, Equatable, Hashable {
    /// The passphrase unwrapped the escrow and the tags matched.
    case verified
    /// The passphrase did not unwrap the cached escrow **after** the client
    /// refreshed it from the server and retried once.
    ///
    /// The refresh is not an optimisation: the escrow may have been rotated
    /// from another device, and without the retry every rotation would
    /// manufacture false failures across the user's other devices.
    case mismatch
    /// The escrow could not be fetched or read, so no conclusion is possible.
    /// Explicitly **not** a failure — recording it as one would punish the user
    /// for a network problem.
    case inconclusive(ErrorCode)

    /// Whether this outcome should advance the failure counter toward the
    /// guided re-wrap flow.
    public var countsAsFailure: Bool {
        self == .mismatch
    }
}

// MARK: - RecoveryEscrowSummary

/// What the account's recovery setup looks like, without any secret in it.
///
/// The **single-root invariant**: there is exactly one root — the recovery
/// secret. The server escrow, the backup artifact's wrap key, and any Shamir
/// shares are all *wraps reachable from* it, not additional roots. Holding the
/// recovery secret reaches all of them; losing everything but the recovery
/// secret loses nothing. A settings screen that presents them as independent
/// backups is telling the user something false.
public struct RecoveryEscrowSummary: Sendable, Equatable, Hashable {
    /// Whether a server-side escrow blob exists.
    public var hasServerEscrow: Bool
    /// When the escrow was last replaced.
    public var escrowUpdatedAt: CapsuleTimestamp?
    /// How many Shamir shares are enrolled, when the user set them up.
    public var shamirShareCount: Int?
    /// The threshold needed to reconstruct, when shares are enrolled.
    public var shamirThreshold: Int?
    /// The verification cadence's state.
    public var verification: RecoveryVerificationState

    public init(
        hasServerEscrow: Bool,
        escrowUpdatedAt: CapsuleTimestamp? = nil,
        shamirShareCount: Int? = nil,
        shamirThreshold: Int? = nil,
        verification: RecoveryVerificationState = RecoveryVerificationState()
    ) {
        self.hasServerEscrow = hasServerEscrow
        self.escrowUpdatedAt = escrowUpdatedAt
        self.shamirShareCount = shamirShareCount
        self.shamirThreshold = shamirThreshold
        self.verification = verification
    }

    /// Whether recovery is set up at all. An account without it can lose
    /// everything to a single device loss, which is the one thing the UI should
    /// nag about.
    public var isConfigured: Bool {
        hasServerEscrow
    }
}
