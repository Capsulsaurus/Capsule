import CapsuleDomain
import CapsulePorts
import Foundation
import Observation

// MARK: - RevocationScope

/// The two revocations, and the asymmetry between them.
///
/// *Authentication — Explicit Revocation* makes the asymmetry load-bearing:
/// revoking **one** session is authenticated by any active session token, while
/// revoking **all** of them requires proof of master-key possession — a
/// signature with the user's IK over a server-issued challenge. An attacker
/// holding a stolen token can therefore revoke only that token's session and
/// cannot escalate to locking the legitimate user out of every device.
///
/// The UI must make the distinction visible rather than presenting two similar
/// buttons, so the requirement is modelled here rather than implied by
/// placement.
public enum RevocationScope: Sendable, Equatable, Hashable {
    /// One session. Everyday tool.
    case singleSession
    /// Every session. The nuclear option, gated accordingly.
    case allSessions

    /// Whether this revocation needs proof of master-key possession.
    public var requiresMasterKeyProof: Bool {
        self == .allSessions
    }
}

// MARK: - DevicesAndSessionsViewModel

/// Drives the session ledger, grouped by device cohort.
@MainActor
@Observable
public final class DevicesAndSessionsViewModel {
    public private(set) var state: ScreenState = .idle
    public private(set) var groups: [DeviceCohortGroup] = []
    public private(set) var isRevoking = false
    /// The cohort a support report was just built for, so the screen can
    /// confirm the bundle exists and offer to share it.
    public private(set) var supportBundle: DeviceCohort?

    private let devicePort: any DevicePort
    private let now: @Sendable () -> CapsuleTimestamp
    private nonisolated(unsafe) var observation: Task<Void, Never>?

    public init(
        devices: any DevicePort,
        now: @escaping @Sendable () -> CapsuleTimestamp = {
            CapsuleTimestamp(epochSeconds: Int64(Date().timeIntervalSince1970))
        }
    ) {
        devicePort = devices
        self.now = now
    }

    deinit {
        observation?.cancel()
    }

    /// The instant the ledger is being read against, for expiry arithmetic.
    public var currentInstant: CapsuleTimestamp { now() }

    /// Whether a "log out everywhere" action needs the master-key ceremony.
    /// Always `true`; exposed so the screen can label the button honestly and a
    /// test can assert the distinction has not been flattened.
    public var revokeAllRequiresMasterKeyProof: Bool {
        RevocationScope.allSessions.requiresMasterKeyProof
    }

    /// Whether revoking one session needs it. Always `false`.
    public var revokeSessionRequiresMasterKeyProof: Bool {
        RevocationScope.singleSession.requiresMasterKeyProof
    }

    /// Whether the last failure was the master-key proof being demanded or
    /// rejected, which is a different conversation from an ordinary error.
    public var needsMasterKeyProof: Bool {
        let code = state.failure?.code
        return code == .authRevokeProofRequired || code == .authRevokeProofInvalid
    }

    public func load() async {
        state = .loading
        do {
            let devices = try await devicePort.devices()
            let sessions = try await devicePort.sessions()
            groups = DeviceCohortGroup.group(devices: devices, sessions: sessions)
            state = groups.isEmpty ? .empty : .ready
            observeChanges()
        } catch {
            state = .failed(AuthPresentableError(error))
        }
    }

    /// Revoke one session. Authenticated by any active session.
    public func revokeSession(_ identifier: SessionID) async {
        await revoke { try await self.devicePort.revokeSession(identifier) }
    }

    /// Revoke a device, which also ends its sessions and removes it from the
    /// album groups it belonged to.
    ///
    /// The directory row survives: a revoked device is **listed, not hidden**,
    /// so everything it ever signed stays verifiable and a user auditing their
    /// account sees the same history the cryptography does.
    public func revokeDevice(_ identifier: DeviceID) async {
        await revoke { try await self.devicePort.revokeDevice(identifier) }
    }

    /// Revoke every session. Drives the master-key challenge internally; a
    /// request without valid proof revokes **nothing at all**, so there is no
    /// partial success to clean up.
    public func revokeAllSessions() async {
        await revoke { try await self.devicePort.revokeAllSessions() }
    }

    /// Build the support report for a cohort — the dispute path, in place of a
    /// toggle the user could not meaningfully set.
    public func buildSupportBundle(for cohortHash: String) async {
        do {
            supportBundle = try await devicePort.supportBundle(for: cohortHash)
        } catch {
            state = .failed(AuthPresentableError(error))
        }
    }

    public func dismissSupportBundle() {
        supportBundle = nil
    }

    private func revoke(_ work: () async throws -> Void) async {
        guard !isRevoking else { return }
        isRevoking = true
        defer { isRevoking = false }
        do {
            try await work()
            await reload()
        } catch {
            state = .failed(AuthPresentableError(error))
        }
    }

    private func reload() async {
        do {
            let devices = try await devicePort.devices()
            let sessions = try await devicePort.sessions()
            groups = DeviceCohortGroup.group(devices: devices, sessions: sessions)
            state = groups.isEmpty ? .empty : .ready
        } catch {
            state = .failed(AuthPresentableError(error))
        }
    }

    private func observeChanges() {
        observation?.cancel()
        let port = devicePort
        observation = Task { [weak self] in
            for await _ in port.changes() {
                guard !Task.isCancelled else { return }
                await self?.reload()
            }
        }
    }
}
