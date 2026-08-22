import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - AuthPort

extension MockIdentityStore: AuthPort {
    public func state() async -> AuthState {
        currentState
    }

    /// Sign in with local credentials.
    ///
    /// Which factors are needed is the server's decision and the ceremony is the
    /// SDK's, so there is nothing here for a view model to drive beyond the
    /// handle — which is exactly why this returns an ``AccountSummary`` and not
    /// a token.
    public func signInLocally(handle: String) async throws -> AccountSummary {
        try await behaviourGate.admit()
        return await completeSignIn()
    }

    /// Sign in through an external identity provider.
    ///
    /// The IdP authenticates the **session**. The master key never derives from,
    /// and is never visible to, the credential verifier — so an IdP compromise
    /// costs the user their session, not their photographs.
    public func signInWithIdentityProvider(issuer: URL) async throws -> AccountSummary {
        try await behaviourGate.admit()
        return await completeSignIn()
    }

    /// Re-authenticate locally to satisfy a freshness gate, without a full
    /// sign-out and sign-in.
    public func confirmLocalAuthentication() async throws {
        guard case let .requiresLocalAuth(account) = currentState else { return }
        setState(.signedIn(account))
        await authChanges.send(currentState)
    }

    /// Sign out **this device's session only**.
    ///
    /// Nothing local is lost, which is why the state machine has a separate
    /// `expired` case: a lapsed session is a re-authentication, not a wipe.
    public func signOut() async throws {
        setState(.signedOut)
        await authChanges.send(currentState)
    }

    public nonisolated func changes() -> AsyncStream<AuthState> {
        authChanges.subscribe()
    }

    private func completeSignIn() async -> AccountSummary {
        let account = Self.account(configuration: configuration)
        setState(.signedIn(account))
        await authChanges.send(currentState)
        return account
    }
}

// MARK: - DevicePort

extension MockIdentityStore: DevicePort {
    public func devices() async throws -> [DeviceRecord] {
        deviceList
    }

    public func sessions() async throws -> [SessionRecord] {
        sessionList
    }

    /// The durable cohort map.
    ///
    /// It outlives session expiry deliberately: without it the "have I seen this
    /// device before" question is unanswerable exactly when it matters, which is
    /// after a long gap.
    public func cohorts() async throws -> [DeviceCohort] {
        Dictionary(grouping: deviceList.compactMap { record -> (String, DeviceRecord)? in
            guard let hash = record.cohortHash else { return nil }
            return (hash, record)
        }, by: \.0).map { hash, entries in
            let records = entries.map(\.1)
            return DeviceCohort(
                cohortHash: hash,
                firstSeen: records.map(\.firstSeen).min() ?? configuration.clock.now,
                lastSeen: records.map(\.lastSeen).max() ?? configuration.clock.now,
                deviceIDs: records.map(\.id)
            )
        }.sorted { $0.cohortHash < $1.cohortHash }
    }

    /// Revoke one session. Authenticated by any active session token.
    public func revokeSession(_ identifier: SessionID) async throws {
        try await behaviourGate.admit()
        setSessions(sessionList.map { record in
            var record = record
            if record.id == identifier, record.revokedAt == nil {
                record.revokedAt = configuration.clock.now
            }
            return record
        })
        await directoryChanges.send(())
    }

    /// Revoke **every** session.
    ///
    /// Authenticated by proof of master-key possession, not by a session token —
    /// deliberately asymmetric, so an attacker holding a stolen token can revoke
    /// only that one session and cannot lock the legitimate user out of every
    /// device. A request without valid proof revokes **nothing at all**, so
    /// there is no partial success to clean up.
    public func revokeAllSessions() async throws {
        try await behaviourGate.admit()
        let stamp = configuration.clock.now
        setSessions(sessionList.map { record in
            var record = record
            if record.revokedAt == nil { record.revokedAt = stamp }
            return record
        })
        setState(.signedOut)
        await authChanges.send(currentState)
        await directoryChanges.send(())
    }

    /// Revoke a device, removing it from the album groups it belongs to.
    ///
    /// The row survives revocation — its key stays in the directory forever so
    /// everything it signed remains verifiable. A key-holding attacker can
    /// append forward but cannot rewrite the past, and that property depends on
    /// this row not being deleted.
    public func revokeDevice(_ identifier: DeviceID) async throws {
        try await behaviourGate.admit()
        let stamp = configuration.clock.now
        setDevices(deviceList.map { record in
            var record = record
            if record.id == identifier, record.revokedAt == nil { record.revokedAt = stamp }
            return record
        })
        setSessions(sessionList.map { record in
            var record = record
            if record.deviceID == identifier, record.revokedAt == nil { record.revokedAt = stamp }
            return record
        })
        await directoryChanges.send(())
    }

    /// Bundle a cohort's device and session map for a support report.
    ///
    /// The client **asserts, it does not litigate**: there is deliberately no
    /// "this isn't my device" toggle, because a user cannot adjudicate a hash
    /// and the value is advisory anyway. The dispute path is a support report,
    /// which is what this produces.
    public func supportBundle(for cohortHash: String) async throws -> DeviceCohort {
        guard let cohort = try await cohorts().first(where: { $0.cohortHash == cohortHash }) else {
            throw CapsuleError(code: .directoryMalformed, detail: "CapsuleMock: unknown cohort")
        }
        return cohort
    }

    public nonisolated func changes() -> AsyncStream<Void> {
        directoryChanges.subscribe()
    }
}
