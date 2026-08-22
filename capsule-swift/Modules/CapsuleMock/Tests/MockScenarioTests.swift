import CapsuleDomain
import CapsuleFoundation
import Foundation
import Testing

@testable import CapsuleMock

// MARK: - Scenarios

/// Each scenario must produce the state it claims, coherently.
///
/// A scenario is how roughly thirty screens are reachable at all, and the UI
/// tests select one by a launch argument they cannot type-check. So the raw
/// strings are asserted here, and each world is checked for the state its name
/// promises — including the states that are easy to configure halfway, like
/// ``MockScenario/offline``, where the network must be gone *and* every local
/// read must still answer.
@Suite("Scenarios configure the whole graph")
struct MockScenarioTests {
    /// The strings a UI-test bundle hard-codes because it cannot import this
    /// module. A mismatch here is a mismatch there.
    @Test("Raw values are the contract with the UI tests")
    func rawValuesAreStable() {
        let expected: [MockScenario: String] = [
            .healthy: "healthy",
            .emptyLibrary: "empty-library",
            .neverSignedIn: "never-signed-in",
            .offline: "offline",
            .hugeLibrary: "huge-library",
            .quotaSoftWarning: "quota-soft-warning",
            .quotaGraceExpired: "quota-grace-expired",
            .quarantine: "quarantine",
            .degradedFederation: "degraded-federation",
            .awaitingOriginals: "awaiting-originals",
            .newerVersionState: "newer-version-state",
            .undecodableAssets: "undecodable-assets",
            .recoveryOverdue: "recovery-overdue",
            .protocolUpgradeRequired: "protocol-upgrade-required",
        ]
        #expect(MockScenario.allCases.count == expected.count)
        for scenario in MockScenario.allCases {
            #expect(scenario.rawValue == expected[scenario])
            #expect(MockScenario(rawValue: scenario.rawValue) == scenario)
        }
    }

    @Test("Launch arguments select a scenario, and a bad one falls back")
    func launchArgumentParsing() {
        #expect(MockScenario.resolve(fromArguments: ["app", "-mock-scenario", "offline"]) == .offline)
        #expect(MockScenario.resolve(fromArguments: ["app"]) == .healthy)
        #expect(MockScenario.resolve(fromArguments: ["app", "-mock-scenario"]) == .healthy)
        #expect(MockScenario.resolve(fromArguments: ["app", "-mock-scenario", "typo"]) == .healthy)
    }

    @Test("Healthy is a populated, signed-in, fully synced library")
    func healthy() async throws {
        let environment = MockEnvironment(scenario: .healthy)
        #expect(try await environment.library.assetCount(matching: .default) == 4000)
        #expect(await environment.auth.state() != .signedOut)
        #expect(try await environment.quota.status().state == .withinQuota)
        #expect(try await environment.quarantine.itemCount() == 0)
    }

    @Test("Empty and never-signed-in have no assets, and only one has a session")
    func emptyWorlds() async throws {
        let empty = MockEnvironment(scenario: .emptyLibrary)
        #expect(try await empty.library.assetCount(matching: .default) == 0)
        #expect(try await empty.library.dayCounts(matching: .default).isEmpty)
        #expect(await empty.auth.state() != .signedOut)

        let signedOut = MockEnvironment(scenario: .neverSignedIn)
        #expect(await signedOut.auth.state() == .signedOut)
    }

    /// The offline-first contract, stated as a test: the network is gone and
    /// **every local read still answers**. A scenario that broke the gallery
    /// would be modelling a different product.
    @Test("Offline stalls the network and leaves every local read working")
    func offline() async throws {
        let environment = MockEnvironment(scenario: .offline)
        #expect(try await environment.sync.status().connectionClass == .offline)

        let page = try await environment.library.assets(matching: .default, offset: 0, limit: 200)
        #expect(page.items.count == 200)
        #expect(try await environment.library.dayCounts(matching: .default).totalCount == 4000)
        #expect(try await environment.library.asset(for: page.items[0].id) != nil)
        #expect(try await environment.library.sidecar(for: page.items[0].id) != nil)

        let degraded = page.items.contains {
            if case .fullResolutionUnavailable = $0.syncState { return true }
            return false
        }
        #expect(degraded)

        await #expect(throws: CapsuleError.self) {
            try await environment.sync.synchronize()
        }
    }

    @Test("Quota scenarios reach the states their names claim")
    func quotaStates() async throws {
        let soft = try await MockEnvironment(scenario: .quotaSoftWarning).quota.status()
        #expect(soft.state == .softWarning)
        #expect(soft.state.permitsNewUploads)
        #expect(soft.state.warrantsWarning)

        let grace = try await MockEnvironment(scenario: .quotaGraceExpired).quota.status()
        #expect(grace.state == .graceExpired)
        #expect(!grace.state.permitsNewUploads)
        #expect(!grace.state.permitsMetadataGrowth)
        // A user must always be able to delete their way back under quota.
        #expect(grace.state.permitsReclaimingWrites)
        #expect(try await MockEnvironment(scenario: .quotaGraceExpired)
            .quota.wouldAdmit(additionalBytes: 1) == false)
    }

    /// Several **distinct surfaces**, not several rows of one: the surface is
    /// what decides where the bytes are and therefore what the user can do.
    @Test("Quarantine populates several distinct surfaces")
    func quarantine() async throws {
        let environment = MockEnvironment(scenario: .quarantine)
        let items = try await environment.quarantine.items(offset: 0, limit: 100)
        #expect(items.items.count >= 5)
        #expect(Set(items.items.map(\.surface)).count >= 5)
        #expect(Set(items.items.map(\.surface.storage)).count >= 3)
        // Repair is offered only where the holding area preserves something.
        #expect(items.items.contains { $0.isRecoverable })
        #expect(items.items.contains { !$0.isRecoverable })
        let auditOnly = items.items.first { !$0.surface.storage.preservesOriginalBytes }
        #expect(auditOnly != nil)
        if let auditOnly {
            #expect(try await environment.quarantine.inspect(auditOnly.id) == nil)
        }
    }

    @Test("Degraded federation leaves albums listed but incomplete")
    func degradedFederation() async throws {
        let environment = MockEnvironment(scenario: .degradedFederation)
        let albums = try await environment.federation.aggregatedAlbums()
        #expect(!albums.isEmpty)
        #expect(albums.contains { !$0.isFullyAvailable })
        // Nothing is removed for being unreachable.
        #expect(albums.allSatisfy { !$0.constituents.isEmpty })
        let peers = try await environment.moderation.peers()
        #expect(peers.contains { !$0.state.permitsPull })
    }

    @Test("Awaiting originals stages uploads and badges the assets")
    func awaitingOriginals() async throws {
        let environment = MockEnvironment(scenario: .awaitingOriginals)
        #expect(try await environment.uploads.uploadPolicy() == .staged)
        let page = try await environment.library.assets(matching: .default, offset: 0, limit: 300)
        let awaiting = page.items.filter {
            if case .awaitingOriginal = $0.syncState { return true }
            return false
        }
        #expect(!awaiting.isEmpty)
        // A badge, never a failure: none of them needs the user's attention.
        #expect(awaiting.allSatisfy { !$0.syncState.needsUserAttention })
        #expect(awaiting.allSatisfy { !$0.representations.isFullResolutionAvailable })
        #expect(!(try await environment.uploads.activeSessions()).isEmpty)
    }

    /// Unknown closed-enum values, a `SchemaAhead` marker, and a definition this
    /// build must preserve without evaluating.
    @Test("Newer-version state is readable and unwritable")
    func newerVersionState() async throws {
        let environment = MockEnvironment(scenario: .newerVersionState)
        let page = try await environment.library.assets(matching: .default, offset: 0, limit: 400)
        let ahead = page.items.filter {
            if case .writtenByNewerVersion = $0.syncState { return true }
            return false
        }
        #expect(!ahead.isEmpty)
        #expect(ahead.allSatisfy { !$0.contentType.isKnown })
        #expect(ahead.allSatisfy { !$0.cull.isKnown })
        #expect(ahead.allSatisfy { $0.syncState.needsUserAttention })
        // Writing an unknown value back is a structural rejection.
        if let subject = ahead.first {
            #expect(throws: ClosedEnumWriteRejection.self) {
                try subject.cull.requireWritable()
            }
        }
        let definitions = try await environment.smartAlbums.definitions()
        let unevaluable = definitions.filter { !$0.isEvaluable }
        #expect(unevaluable.count == 1)
        if let subject = unevaluable.first {
            await #expect(throws: CapsuleError.self) {
                _ = try await environment.smartAlbums.evaluate(subject.id, offset: 0, limit: 10)
            }
        }
    }

    @Test("Undecodable assets are valid but unopenable here")
    func undecodableAssets() async throws {
        let environment = MockEnvironment(scenario: .undecodableAssets)
        let page = try await environment.library.assets(matching: .default, offset: 0, limit: 400)
        let unreadable = page.items.compactMap { asset -> UnreadableReason? in
            guard case let .unreadableOnThisDevice(reason) = asset.syncState else { return nil }
            return reason
        }
        #expect(!unreadable.isEmpty)
        #expect(Set(unreadable).count >= 2)
    }

    @Test("Recovery overdue exhausts its snoozes and offers the re-wrap")
    func recoveryOverdue() async throws {
        let environment = MockEnvironment(scenario: .recoveryOverdue)
        let summary = try await environment.recovery.summary()
        #expect(summary.isConfigured)
        #expect(summary.verification.isDue(at: environment.configuration.clock.now))
        #expect(!summary.verification.canSnooze)
        #expect(summary.verification.shouldOfferGuidedRewrap)
    }

    @Test("Protocol upgrade required refuses network work with a stable code")
    func protocolUpgradeRequired() async throws {
        let environment = MockEnvironment(scenario: .protocolUpgradeRequired)
        do {
            try await environment.sync.forceSynchronize()
            Issue.record("expected the reconciliation to be refused")
        } catch let error as CapsuleError {
            #expect(error.code == .protocolVersionUnsupported)
            #expect(error.recoveryAction == .abortWithUpgrade)
        }
        // Local reads keep working: an out-of-date client still shows the
        // library it already has.
        #expect(try await environment.library.assetCount(matching: .default) == 4000)
    }
}
