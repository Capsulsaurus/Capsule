import CapsuleDomain
import Foundation

// MARK: - DeviceIdentity

/// A device as the *other* device's user sees it during a cross-device add:
/// model plus a short key fingerprint.
///
/// Both halves are load-bearing. *Device Enrollment — Safety-code check* binds
/// the safety code to device identity precisely so a relay that swapped in a
/// different device is visible: matching digits alone would not catch it.
public struct DeviceIdentity: Sendable, Equatable, Hashable {
    /// The self-reported hardware model, e.g. `iPhone17,2`.
    public var model: String
    public var platform: PlatformTag
    /// A short fingerprint of the device signing key, already chunked.
    public var keyFingerprint: String

    public init(model: String, platform: PlatformTag, keyFingerprint: String) {
        self.model = model
        self.platform = platform
        self.keyFingerprint = keyFingerprint
    }
}

// MARK: - CrossDeviceInvite

/// The two presentations of one enrollment code
/// (*Device Enrollment — Enrollment code*).
///
/// Both are secrets for the code's ten-minute life, so both are
/// ``RedactedSecret``: the QR payload is the full ≥64-bit value, and the text
/// fallback deliberately trades entropy for transcribability. The fallback is
/// safe only because it never stands alone — single-use, ten-minute expiry,
/// rate-limited redemption, and channel integrity resting on the safety-code
/// check rather than on the code.
public struct CrossDeviceInvite: Sendable {
    /// The full-entropy payload the QR encodes.
    public var qrPayload: RedactedSecret
    /// The 8–10 digit fallback a person can read out loud.
    public var textFallback: RedactedSecret
    /// When both stop working.
    public var expiresAt: CapsuleTimestamp
    /// The relay channel this ceremony runs over.
    public var channelHandle: String

    public init(
        qrPayload: RedactedSecret,
        textFallback: RedactedSecret,
        expiresAt: CapsuleTimestamp,
        channelHandle: String
    ) {
        self.qrPayload = qrPayload
        self.textFallback = textFallback
        self.expiresAt = expiresAt
        self.channelHandle = channelHandle
    }

    public func isLive(at now: CapsuleTimestamp) -> Bool {
        now < expiresAt
    }
}

// MARK: - SafetyCheck

/// The channel-verification step both devices must pass.
///
/// The safety code is derived from the channel transcript, so a MITM produces
/// two different codes. The client's whole job is to make the human comparison
/// failure-resistant: identical chunking on both devices, each device's
/// identity beside it, and a mismatch that is the obvious exit rather than a
/// missed default.
public struct SafetyCheck: Sendable {
    /// The transcript-derived code, chunked identically on both devices.
    public var safetyCode: RedactedSecret
    /// This device.
    public var localDevice: DeviceIdentity
    /// The device being added.
    public var remoteDevice: DeviceIdentity

    public init(safetyCode: RedactedSecret, localDevice: DeviceIdentity, remoteDevice: DeviceIdentity) {
        self.safetyCode = safetyCode
        self.localDevice = localDevice
        self.remoteDevice = remoteDevice
    }
}

// MARK: - CrossDeviceCeremonyPort

/// The presentation half of a cross-device add: the code in its two forms, and
/// the safety check.
///
/// ``EnrollmentPort`` owns the ceremony itself — issuing, redeeming, observing,
/// cancelling. This port owns only what the *screen* has to draw and cannot
/// derive for itself: a client that invented the text fallback or the safety
/// code locally would be verifying its own arithmetic rather than the channel.
///
/// **Not yet in `CapsulePorts`.** *Device Enrollment — Status note* puts the
/// native add UI post-v1, so this surface is declared next to the screen that
/// will need it.
public protocol CrossDeviceCeremonyPort: Sendable {
    /// Both presentations of an issued code.
    func invite(for code: EnrollmentCode) async throws -> CrossDeviceInvite

    /// The safety check for a live channel, once the far device has redeemed.
    func safetyCheck(channelHandle: String) async throws -> SafetyCheck

    /// Record the user's explicit match-and-identity acknowledgement and let
    /// the key transfer proceed.
    func confirmSafetyCheck(channelHandle: String) async throws

    /// Abort on a mismatch. Invalidates the channel and the code, so a MITM
    /// that got as far as a divergent code gets nothing further.
    func abortSafetyCheck(channelHandle: String) async throws
}
