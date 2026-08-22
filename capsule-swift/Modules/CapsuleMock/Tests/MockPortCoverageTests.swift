import CapsuleDomain
import CapsuleFoundation
import Foundation
import Testing

@testable import CapsuleMock

// MARK: - Port coverage

/// Every port answers, and the answers are internally consistent.
///
/// The point of this suite is breadth rather than depth: a screen wired to a
/// port that throws on its first call is a screen nobody can walk, and the
/// failure shows up as a blank view rather than an error. So each port gets at
/// least one round trip, and the ones whose invariants cross ports get checked
/// against each other.
@Suite("Every port answers coherently")
struct MockPortCoverageTests {
    private let environment = MockEnvironment(scenario: .healthy)

    @Test("Views are listed, key-free, and never import destinations")
    func viewAlbums() async throws {
        let views = try await environment.albums.viewAlbums()
        #expect(views.allSatisfy { !$0.canReceiveImports })
        let systemKinds = views.compactMap { view -> ViewAlbum.SystemView? in
            guard case let .system(kind) = view.kind else { return nil }
            return kind
        }
        #expect(Set(systemKinds) == Set(ViewAlbum.SystemView.allCases))
        let gated = views.filter { $0.requiresFreshLocalAuth }
        #expect(gated.count == 2)
    }

    @Test("Stacks expose their members and their derived group state")
    func stacks() async throws {
        let library = environment.libraryStore.library
        guard let index = (0 ..< 400).first(where: { library.stackType(at: $0) != nil }),
              let stack = library.stack(at: index)
        else {
            Issue.record("no stack was derived in the first 400 assets")
            return
        }
        let members = try await environment.stacks.members(of: stack.id)
        #expect(members.count == stack.memberAssetIDs.count)
        #expect(members.first?.isStackCover == true)
        #expect(members.dropFirst().allSatisfy { $0.isStackHidden })
        #expect(try await environment.stacks.stack(stack.id) == stack)

        // Flagging a collapsed stack flags every member: a group has no stored
        // flag of its own.
        try await environment.organize.setCull(.reject, for: [members[0].id])
        let reflagged = try await environment.stacks.members(of: stack.id)
        #expect(reflagged.allSatisfy { $0.cull == .reject })
        #expect(try await environment.stacks.stack(stack.id)?.cullState == .allRejected)
    }

    @Test("People clusters page arithmetically and stay listed when stale")
    func people() async throws {
        let clusters = try await environment.people.clusters(offset: 0, limit: 50)
        #expect(!clusters.items.isEmpty)
        // Most populous first — checked pairwise, because `>=` is not a strict
        // weak ordering and `sorted(by:)` would be undefined with it.
        #expect(zip(clusters.items, clusters.items.dropFirst()).allSatisfy { $0.assetCount >= $1.assetCount })
        guard let subject = clusters.items.first else { return }
        let page = try await environment.people.assets(in: subject.id, offset: 0, limit: 10)
        #expect(page.totalCount == subject.assetCount)
        #expect(page.items.count == min(10, subject.assetCount))

        try await environment.people.setName("riley", for: subject.id)
        let renamed = try await environment.people.cluster(subject.id)
        #expect(renamed?.name.value == "riley")
        #expect(renamed?.isNamed == true)
    }

    @Test("Places cluster by cell and carry their stored datum")
    func places() async throws {
        let region = try #require(try await environment.places.boundingRegion())
        let clusters = try await environment.places.clusters(in: region, granularity: 6)
        #expect(!clusters.isEmpty)
        // A GCJ-02 trip must be marked approximate on a WGS-84 map.
        #expect(clusters.contains { $0.centroid.datum.displaysAsApproximate })
        guard let subject = clusters.first(where: { $0.assetCount > 0 }) else { return }
        let page = try await environment.places.assets(in: subject.id, offset: 0, limit: 5)
        #expect(page.items.count <= 5)
    }

    @Test("Search records history locally and clears it")
    func search() async throws {
        let results = try await environment.search.search("beach", scope: .all, offset: 0, limit: 20)
        #expect(results.items.allSatisfy { !$0.matchedScope.isEmpty })
        #expect(try await environment.search.recentSearches() == ["beach"])
        #expect(!(try await environment.search.suggestions(for: "tra", limit: 10)).isEmpty)
        try await environment.search.clearRecentSearches()
        #expect(try await environment.search.recentSearches().isEmpty)
    }

    @Test("Model slots report a not-downloaded steady state")
    func modelSlots() async throws {
        let statuses = try await environment.intelligence.modelStatuses()
        #expect(statuses.count == 4)
        #expect(statuses.contains { $0.availability == .notDownloaded })
        #expect(statuses.contains { if case .supersededBy = $0.availability { return true } else { return false } })
        var last: AIModelStatus?
        for await status in environment.intelligence.downloadModel(slot: MockTables.imageEmbeddingSlot) {
            last = status
        }
        #expect(last?.availability == .ready)
    }

    /// Plan and confirm are separate steps, and the planner refuses the one
    /// combination that would delete source files against a promise it could
    /// not keep.
    @Test("Import scans, plans, refuses staged streaming, and executes")
    func importing() async throws {
        let scopes = try await environment.importing.availableScopes()
        #expect(scopes.count == 4)
        let scan = try await environment.importing.scan(scopes[0])
        #expect(!scan.candidates.isEmpty)

        await #expect(throws: CapsuleError.self) {
            _ = try await environment.importing.plan(
                scan,
                destination: nil,
                mode: .copy,
                uploadPolicy: .staged,
                streaming: true
            )
        }

        let plan = try await environment.importing.plan(
            scan,
            destination: nil,
            mode: .copy,
            uploadPolicy: .full,
            streaming: false
        )
        #expect(!plan.violatesStagedStreamingExclusion)
        #expect(plan.decisions.count == scan.candidates.count)
        // The destination always resolves to a container, never a view.
        #expect(plan.destinationAlbumID.isUserAlbum)

        var finished: ImportSummary?
        for await event in environment.importing.execute(plan) {
            if case let .finished(summary) = event { finished = summary }
        }
        #expect(finished?.results.count == plan.decisions.count)
        #expect(finished?.importedCount == plan.importCount)
    }

    /// The gate: only a durable asset may release its only local copy, and a
    /// refusal releases nothing at all.
    @Test("Verify-before-destroy refuses a non-durable release")
    func verifyBeforeDestroy() async throws {
        let staged = MockEnvironment(scenario: .awaitingOriginals)
        let page = try await staged.library.assets(matching: .default, offset: 0, limit: 300)
        guard let awaiting = page.items.first(where: {
            if case .awaitingOriginal = $0.syncState { return true }
            return false
        }) else {
            Issue.record("no asset was awaiting its original")
            return
        }
        let verdicts = try await staged.storage.verify(assetIDs: [awaiting.id], deep: false)
        #expect(verdicts.first?.durable == false)
        #expect(verdicts.first?.missingBlobs.isEmpty == false)
        await #expect(throws: CapsuleError.self) {
            try await staged.storage.releaseLocalCopies(for: [awaiting.id])
        }
        // Nothing was released.
        #expect(try await staged.library.asset(for: awaiting.id)?.representations == awaiting.representations)
    }

    @Test("Custody receipts back a durable asset and chain by sequence")
    func custodyReceipts() async throws {
        let page = try await environment.library.assets(matching: .default, offset: 0, limit: 100)
        guard let durable = page.items.first(where: { $0.syncState == .durable }) else { return }
        let receipts = try await environment.uploads.custodyReceipts(for: durable.id)
        #expect(receipts.count == 3)
        #expect(Set(receipts.map(\.receiptSequence)).count == receipts.count)
        #expect(receipts.allSatisfy { $0.serverID == "capsule.example" })
    }

    @Test("A revoked device stays in the directory")
    func deviceDirectory() async throws {
        let devices = try await environment.devices.devices()
        #expect(devices.contains { !$0.isActive })
        #expect(devices.contains { $0.isCurrent })
        let cohorts = try await environment.devices.cohorts()
        #expect(!cohorts.isEmpty)
        guard let cohort = cohorts.first else { return }
        #expect(try await environment.devices.supportBundle(for: cohort.cohortHash) == cohort)
    }

    @Test("Recovery verification accepts the minted secret and rotates the wrap")
    func recovery() async throws {
        let secret = try await environment.recovery.setUpRecovery()
        #expect(try await environment.recovery.verify(passphrase: secret) == .verified)
        #expect(try await environment.recovery.verify(passphrase: "wrong") == .mismatch)
        let rotated = try await environment.recovery.rotateRecoverySecret()
        #expect(rotated != secret)
        #expect(try await environment.recovery.verify(passphrase: rotated) == .verified)
    }

    @Test("Maintenance runs report a completion")
    func maintenance() async throws {
        let tasks = try await environment.maintenance.tasks()
        #expect(tasks.count == MaintenanceTaskKind.knownCases.count)
        var last: MaintenanceTask?
        for await task in environment.maintenance.run(.structuralValidation) { last = task }
        guard case .completed = last?.state else {
            Issue.record("the run did not complete")
            return
        }
    }

    @Test("Settings round-trip and the default album pointer moves")
    func settings() async throws {
        var current = try await environment.settings.settings()
        current.aiProcessingEnabled = false
        try await environment.settings.update(current)
        #expect(try await environment.settings.settings().aiProcessingEnabled == false)
        let albums = try await environment.albums.containerAlbums()
        try await environment.settings.setDefaultAlbumID(albums[2].id)
        #expect(try await environment.settings.defaultAlbumID() == albums[2].id)
    }

    @Test("Share links respect revocation, expiry, and the passphrase layer")
    func sharing() async throws {
        let links = try await environment.sharing.links()
        #expect(links.count == 4)
        let now = environment.configuration.clock.now
        guard let live = links.first(where: { $0.isLive(at: now) && !$0.hasPassphrase }) else { return }
        let opened = try await environment.sharing.openLink(
            opaqueID: live.opaqueID,
            secret: live.secret,
            passphrase: nil
        )
        #expect(!opened.items.isEmpty)

        if let wrapped = links.first(where: { $0.hasPassphrase && $0.isLive(at: now) }) {
            await #expect(throws: CapsuleError.self) {
                _ = try await environment.sharing.openLink(
                    opaqueID: wrapped.opaqueID,
                    secret: wrapped.secret,
                    passphrase: nil
                )
            }
        }

        try await environment.sharing.revokeLink(live.id)
        await #expect(throws: CapsuleError.self) {
            _ = try await environment.sharing.openLink(
                opaqueID: live.opaqueID,
                secret: live.secret,
                passphrase: nil
            )
        }
    }

    @Test("Blocking an origin drops its constituent from this viewer's aggregate")
    func moderation() async throws {
        let store = MockEnvironment(scenario: .degradedFederation)
        try await store.moderation.block(.peer(PeerID("legacy.example")))
        let albums = try await store.federation.aggregatedAlbums()
        let blocked = albums.flatMap(\.constituents).filter { $0.homeServer == "legacy.example" }
        #expect(!blocked.isEmpty)
        #expect(blocked.allSatisfy { $0.availability == .blocked })
        #expect(try await store.moderation.blocks().count == 1)
        // Rate limiting is backpressure, not a slower accept.
        for _ in 0 ..< MockFederationStore.reportsPerWindow {
            _ = try await store.moderation.report(.peer(PeerID("legacy.example")), reason: .spam)
        }
        await #expect(throws: CapsuleError.self) {
            _ = try await store.moderation.report(.peer(PeerID("legacy.example")), reason: .spam)
        }
    }

    @Test("Peering serves originals only to a paired device")
    func peering() async throws {
        #expect(await environment.peering.isEnabled())
        let peers = try await environment.peering.discoveredPeers()
        #expect(peers.contains { $0.trust == .discovered })
        guard let unpaired = peers.first(where: { !$0.permitsTransfer }) else { return }
        await #expect(throws: CapsuleError.self) {
            try await environment.peering.requestOriginals(for: [], from: unpaired.id)
        }
        #expect(!(try await environment.peering.activeTransfers()).isEmpty)
    }
}
