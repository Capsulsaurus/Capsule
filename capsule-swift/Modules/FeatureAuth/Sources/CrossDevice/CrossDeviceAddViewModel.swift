import CapsuleDomain
import CapsulePorts
import Foundation
import Observation

// MARK: - CrossDeviceStep

/// Where the add ceremony stands, from the issuing device's side.
public enum CrossDeviceStep: Sendable, Equatable, Hashable {
    /// Nothing issued yet.
    case idle
    /// A code is on screen, waiting to be scanned or typed.
    case awaitingRedemption
    /// The far device redeemed; both users must now compare the safety code.
    case verifyingSafetyCode
    /// Acknowledged on this side; keys are moving.
    case transferringKeys
    /// The new device is in the directory.
    case completed(DeviceID)
    /// Aborted on a safety-code mismatch. Terminal, and deliberately loud.
    case abortedOnMismatch
    /// Failed with a stable code.
    case failed(AuthPresentableError)
}

// MARK: - CrossDeviceAddViewModel

/// Drives a cross-device add from the already-enrolled device.
///
/// Two properties of *Device Enrollment — Cross-Device Add* shape the whole
/// screen:
///
/// - **The code is not the security.** It is single-use, ten-minute, and
///   rate-limited, which is why a friendlier 8–10 digit text fallback can sit
///   beside the full-entropy QR payload without weakening anything. Channel
///   integrity rests on the safety-code check, not on the code.
/// - **The safety-code check is the MITM defence.** Both devices show the same
///   transcript-derived code in the same chunked format, each beside its own
///   model and short key fingerprint, and confirming requires an explicit
///   match-and-identity acknowledgement. A mismatch is therefore the *abort*
///   path — ``abortOnMismatch()`` — never a missed default: nothing advances on
///   its own, and the destructive-looking button is the safe one.
@MainActor
@Observable
public final class CrossDeviceAddViewModel {
    public private(set) var step: CrossDeviceStep = .idle
    public private(set) var invite: CrossDeviceInvite?
    public private(set) var safetyCheck: SafetyCheck?
    public private(set) var isWorking = false
    /// Whether the user ticked "the codes match and this is my device". Both
    /// halves, in one acknowledgement, because confirming a code without
    /// confirming the device is what a relay swap counts on.
    public var hasAcknowledgedMatch = false

    /// The far side finished before the user acknowledged the safety code.
    /// Held back rather than shown: reporting success while the check is still
    /// on screen would teach the user that the check is decorative.
    private var pendingCompletion: DeviceID?
    private var hasAcknowledgedInPort = false

    private let enrollment: any EnrollmentPort
    private let ceremony: any CrossDeviceCeremonyPort
    private let now: @Sendable () -> CapsuleTimestamp
    private nonisolated(unsafe) var observation: Task<Void, Never>?

    public init(
        enrollment: any EnrollmentPort,
        ceremony: any CrossDeviceCeremonyPort,
        now: @escaping @Sendable () -> CapsuleTimestamp = {
            CapsuleTimestamp(epochSeconds: Int64(Date().timeIntervalSince1970))
        }
    ) {
        self.enrollment = enrollment
        self.ceremony = ceremony
        self.now = now
    }

    deinit {
        observation?.cancel()
    }

    // MARK: Derived state

    /// The chunked safety code, identical in format to the one the far device
    /// draws because both come from ``ChunkedCodeFormatter``.
    public var safetyCodeDisplay: String {
        guard let safetyCheck else { return "" }
        return ChunkedCodeFormatter.chunked(safetyCheck.safetyCode.reveal())
    }

    /// The 8–10 digit fallback, grouped for reading aloud.
    public var textFallbackDisplay: String {
        guard let invite else { return "" }
        return ChunkedCodeFormatter.chunked(invite.textFallback.reveal(), groupSize: 3)
    }

    /// The QR payload. A secret for the code's lifetime — rendered, never
    /// logged, never written.
    public func qrPayload() -> String? {
        invite?.qrPayload.reveal()
    }

    /// Whether the issued code is still redeemable.
    public var isInviteLive: Bool {
        invite?.isLive(at: now()) ?? false
    }

    /// Whether the confirm button may act. Requires the explicit
    /// acknowledgement; there is no path that advances without it.
    public var canConfirm: Bool {
        safetyCheck != nil && hasAcknowledgedMatch && !isWorking
    }

    /// Whether local re-authentication is what is standing in the way.
    ///
    /// Issuing a code needs **fresh local authorization**, not merely a valid
    /// session token, so a remotely-exfiltrated token cannot enroll a rogue
    /// device. The refusal is real, which is why it arrives as an error here
    /// rather than as a disabled button — a disabled button is not a security
    /// control.
    public var needsFreshLocalAuth: Bool {
        guard case let .failed(error) = step else { return false }
        return error.code == .enrollmentLocalAuthRequired
    }

    // MARK: Actions

    /// Issue a code and start watching the ceremony.
    public func issueCode() async {
        guard !isWorking else { return }
        isWorking = true
        defer { isWorking = false }
        do {
            let code = try await enrollment.issueEnrollmentCode()
            invite = try await ceremony.invite(for: code)
            step = .awaitingRedemption
            observe(channelHandle: code.channelHandle)
        } catch {
            step = .failed(AuthPresentableError(error))
        }
    }

    /// Acknowledge that the codes match and the device is the right one.
    public func confirmMatch() async {
        guard canConfirm, let handle = invite?.channelHandle else { return }
        isWorking = true
        defer { isWorking = false }
        do {
            try await ceremony.confirmSafetyCheck(channelHandle: handle)
            hasAcknowledgedInPort = true
            if let completed = pendingCompletion {
                step = .completed(completed)
            } else {
                step = .transferringKeys
            }
        } catch {
            step = .failed(AuthPresentableError(error))
        }
    }

    /// Abort because the codes differ, or because the device shown is not the
    /// one in front of the user.
    ///
    /// Invalidates the channel and the code, so a MITM that got as far as a
    /// divergent code gets nothing further.
    public func abortOnMismatch() async {
        guard let handle = invite?.channelHandle else { return }
        observation?.cancel()
        try? await ceremony.abortSafetyCheck(channelHandle: handle)
        try? await enrollment.cancelEnrollment(channelHandle: handle)
        invite = nil
        safetyCheck = nil
        hasAcknowledgedMatch = false
        hasAcknowledgedInPort = false
        pendingCompletion = nil
        step = .abortedOnMismatch
    }

    /// Abandon a ceremony the user simply walked away from. The code expires;
    /// no state is persisted.
    public func cancel() async {
        guard let handle = invite?.channelHandle else { return }
        observation?.cancel()
        try? await enrollment.cancelEnrollment(channelHandle: handle)
        invite = nil
        safetyCheck = nil
        hasAcknowledgedMatch = false
        hasAcknowledgedInPort = false
        pendingCompletion = nil
        step = .idle
    }

    private func observe(channelHandle: String) {
        observation?.cancel()
        // The stream is created here, on the main actor, rather than inside the
        // task: the task holds only a weak reference to this model, so asking it
        // for the stream would race a dismissal.
        let stream = enrollment.observeEnrollment(channelHandle: channelHandle)
        observation = Task { [weak self] in
            for await progress in stream {
                guard !Task.isCancelled else { return }
                await self?.apply(progress, channelHandle: channelHandle)
            }
        }
    }

    private func apply(_ progress: EnrollmentProgress, channelHandle: String) async {
        guard step != .abortedOnMismatch else { return }
        switch progress {
        case .awaitingRedemption:
            step = .awaitingRedemption
        case .exchangingKeys:
            await loadSafetyCheck(channelHandle: channelHandle)
        case .publishingDirectory:
            if hasAcknowledgedInPort { step = .transferringKeys }
        case let .completed(identifier):
            // The relay can finish before the user has compared anything. The
            // ceremony is not reported as complete until they have, so the
            // safety-code screen is never skipped past by a fast far side.
            if hasAcknowledgedInPort {
                step = .completed(identifier)
            } else {
                pendingCompletion = identifier
            }
        case let .failed(code):
            step = .failed(AuthPresentableError(CapsuleError(code: code)))
        }
    }

    private func loadSafetyCheck(channelHandle: String) async {
        do {
            safetyCheck = try await ceremony.safetyCheck(channelHandle: channelHandle)
            step = .verifyingSafetyCode
        } catch {
            step = .failed(AuthPresentableError(error))
        }
    }
}
