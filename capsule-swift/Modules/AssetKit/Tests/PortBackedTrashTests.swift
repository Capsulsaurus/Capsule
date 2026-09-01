import Foundation
import Testing

import AssetKit
import CapsuleCatalog
import CapsuleDomain
import CapsuleFoundation
import CapsulePorts

@Suite("Port-backed trash provider")
struct PortBackedTrashTests {
    /// The adapter under test, already through its SR1 gate — the listing
    /// assertions below are about the trash slice, not about the gate, and a
    /// locked provider would make every one of them fail for the wrong reason.
    private static func unlocked(_ library: FakeLibrary) async throws -> PortBackedTrashProvider {
        let trash = PortBackedTrashProvider(
            organize: library,
            library: library,
            authenticator: StubAuthenticator()
        )
        try await trash.unlockTrash()
        return trash
    }

    @Test("the trash slice lists only deleted assets, and restore returns them")
    func listsAndRestores() async throws {
        let assets = BridgeFixtures.libraryAssets(count: 3)
        let library = FakeLibrary(assets: assets)
        let trash = try await Self.unlocked(library)

        let empty = try await trash.trashedAssets()
        #expect(empty.isEmpty)

        try await library.moveToTrash([assets[0].id], retentionDays: nil)
        let trashed = try await trash.trashedAssets()
        #expect(trashed.map(\.id) == [assets[0].id])

        try await trash.restore(assets[0].id)
        let restored = try await trash.trashedAssets()
        #expect(restored.isEmpty)
    }

    @Test("purge removes the asset outright")
    func purges() async throws {
        let assets = BridgeFixtures.libraryAssets(count: 2)
        let library = FakeLibrary(assets: assets)
        let trash = try await Self.unlocked(library)

        try await library.moveToTrash([assets[0].id], retentionDays: nil)
        try await trash.purge(assets[0].id)
        let remaining = try await trash.trashedAssets()
        #expect(remaining.isEmpty)
        let gone = try await library.asset(for: assets[0].id)
        #expect(gone == nil)
    }

    // MARK: The SR1 gate

    //
    // The mock lane has no Rust core to refuse the read, so these assert the
    // adapter refuses it — the same policy `MockCatalogTests` pins for the
    // catalog, at the layer that owns the grant in this lane.

    @Test("the listing refuses until a grant is taken")
    func refusesWithoutGrant() async throws {
        let library = FakeLibrary(assets: BridgeFixtures.libraryAssets(count: 1))
        let trash = PortBackedTrashProvider(
            organize: library,
            library: library,
            authenticator: StubAuthenticator()
        )

        #expect(await trash.isTrashUnlocked() == false)
        await #expect(throws: CatalogError.viewLocked) {
            try await trash.trashedAssets()
        }

        try await trash.unlockTrash()
        #expect(await trash.isTrashUnlocked())
        _ = try await trash.trashedAssets()
    }

    @Test("a cancelled challenge mints nothing")
    func cancelledChallengeMintsNothing() async throws {
        let library = FakeLibrary(assets: BridgeFixtures.libraryAssets(count: 1))
        let trash = PortBackedTrashProvider(
            organize: library,
            library: library,
            authenticator: StubAuthenticator(grants: false)
        )

        await #expect(throws: LocalAuthError.cancelled) {
            try await trash.unlockTrash()
        }
        #expect(await trash.isTrashUnlocked() == false)
    }

    @Test("a grant inside its window is reused rather than re-challenged")
    func grantIsReused() async throws {
        let library = FakeLibrary(assets: BridgeFixtures.libraryAssets(count: 1))
        let authenticator = StubAuthenticator()
        let trash = PortBackedTrashProvider(
            organize: library,
            library: library,
            authenticator: authenticator
        )

        try await trash.unlockTrash()
        try await trash.unlockTrash()
        #expect(await authenticator.challengeCount == 1)
    }

    @Test("a device with no credential opens rather than sealing shut")
    func unavailableGateOpens() async throws {
        let library = FakeLibrary(assets: BridgeFixtures.libraryAssets(count: 1))
        let authenticator = StubAuthenticator(method: .unavailable, grants: false)
        let trash = PortBackedTrashProvider(
            organize: library,
            library: library,
            authenticator: authenticator
        )

        try await trash.unlockTrash()
        #expect(await trash.isTrashUnlocked())
        #expect(await authenticator.challengeCount == 0)
    }
}

// MARK: - StubAuthenticator

/// A scripted ``LocalAuthenticator``. Counts its challenges, because "the grant
/// was reused" is only observable as a challenge that did not happen.
private actor StubAuthenticator: LocalAuthenticator {
    private let method: LocalAuthMethod
    private let grants: Bool
    private(set) var challengeCount = 0

    init(method: LocalAuthMethod = .biometric, grants: Bool = true) {
        self.method = method
        self.grants = grants
    }

    func availableMethod() async -> LocalAuthMethod { method }

    func authenticate(reasonKey _: String) async throws -> Bool {
        challengeCount += 1
        return grants
    }
}
